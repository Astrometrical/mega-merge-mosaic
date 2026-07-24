//! Minimal XISF reader for PixInsight-produced mosaic panels.
//!
//! Supports the subset MosaicByCoordinates emits: a monolithic XISF file with
//! one `<Image>` element, planar pixel storage, little-endian Float32 samples,
//! uncompressed attachment data block. Anything else errors clearly; compressed
//! blocks will be handled later by decompressing into the tile cache.
//!
//! Spec: https://pixinsight.com/doc/docs/XISF-1.0-spec/XISF-1.0-spec.html

use std::fs::File;
use std::path::{Path, PathBuf};

use memmap2::Mmap;
use quick_xml::events::Event;

use super::{FitsKeyword, SampleFormat};
use crate::{Error, Result};

const SIGNATURE: &[u8; 8] = b"XISF0100";

/// Parsed metadata for the image block of an XISF file.
#[derive(Debug, Clone)]
pub struct XisfHeader {
    pub width: u64,
    pub height: u64,
    pub channels: u64,
    pub sample_format: SampleFormat,
    /// Byte offset of the attached data block within the file.
    pub data_offset: u64,
    /// Byte size of the attached data block.
    pub data_size: u64,
    pub fits_keywords: Vec<FitsKeyword>,
}

/// A memory-mapped, read-only XISF panel exposing planar f32 channel data.
pub struct XisfPanel {
    path: PathBuf,
    mmap: Mmap,
    header: XisfHeader,
}

impl XisfPanel {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).map_err(|e| Error::io(path, e))?;
        // SAFETY: read-only map; we accept that truncation by another process
        // during a run is undefined, as is conventional for mmap-based readers.
        let mmap = unsafe { Mmap::map(&file) }.map_err(|e| Error::io(path, e))?;
        let header = parse_header(path, &mmap)?;

        let end = header
            .data_offset
            .checked_add(header.data_size)
            .filter(|&end| end <= mmap.len() as u64)
            .ok_or_else(|| Error::format(path, "data block extends past end of file"))?;
        let expected = header.width * header.height * header.channels
            * header.sample_format.bytes_per_sample() as u64;
        if header.data_size != expected {
            return Err(Error::format(
                path,
                format!(
                    "data block size {} does not match geometry {}x{}x{} ({} bytes expected)",
                    header.data_size, header.width, header.height, header.channels, expected
                ),
            ));
        }
        let _ = end;

        if header.sample_format != SampleFormat::Float32 {
            return Err(Error::format(
                path,
                format!("unsupported sample format {:?} (only Float32 for now)", header.sample_format),
            ));
        }
        if header.data_offset % 4 != 0 {
            // PixInsight aligns attachments to 4096 bytes; anything unaligned
            // would break the zero-copy f32 view.
            return Err(Error::format(path, "attachment offset not 4-byte aligned"));
        }

        Ok(Self { path: path.to_path_buf(), mmap, header })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn header(&self) -> &XisfHeader {
        &self.header
    }

    pub fn width(&self) -> u64 {
        self.header.width
    }

    pub fn height(&self) -> u64 {
        self.header.height
    }

    pub fn channels(&self) -> u64 {
        self.header.channels
    }

    /// Zero-copy view of one full channel plane (row-major, top-down).
    pub fn channel(&self, c: u64) -> &[f32] {
        assert!(c < self.header.channels, "channel {c} out of range");
        let plane = (self.header.width * self.header.height) as usize;
        let start = self.header.data_offset as usize + c as usize * plane * 4;
        bytemuck::cast_slice(&self.mmap[start..start + plane * 4])
    }

    /// Zero-copy view of one row of one channel.
    pub fn row(&self, c: u64, y: u64) -> &[f32] {
        assert!(y < self.header.height, "row {y} out of range");
        let w = self.header.width as usize;
        let plane = self.channel(c);
        &plane[y as usize * w..(y as usize + 1) * w]
    }

    /// Advise the OS that access will be sequential (large streaming scans).
    pub fn advise_sequential(&self) {
        let _ = self.mmap.advise(memmap2::Advice::Sequential);
    }
}

fn parse_header(path: &Path, mmap: &Mmap) -> Result<XisfHeader> {
    if mmap.len() < 16 || &mmap[0..8] != SIGNATURE {
        return Err(Error::format(path, "not a monolithic XISF file (bad signature)"));
    }
    let header_len = u32::from_le_bytes(mmap[8..12].try_into().unwrap()) as usize;
    if mmap.len() < 16 + header_len {
        return Err(Error::format(path, "truncated XISF header"));
    }
    let xml = &mmap[16..16 + header_len];

    let mut reader = quick_xml::Reader::from_reader(xml);
    reader.config_mut().trim_text(true);

    let mut header: Option<XisfHeader> = None;
    let mut in_image = false;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = e.local_name();
                let name = name.as_ref();
                if name == b"Image" && header.is_none() {
                    header = Some(parse_image_element(path, &e)?);
                    in_image = true;
                } else if name == b"FITSKeyword" && in_image {
                    if let Some(h) = header.as_mut() {
                        h.fits_keywords.push(parse_fits_keyword(path, &e)?);
                    }
                }
            }
            Ok(Event::End(e)) => {
                if e.local_name().as_ref() == b"Image" {
                    in_image = false;
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(Error::format(path, format!("XML parse error: {e}"))),
            _ => {}
        }
        buf.clear();
    }

    header.ok_or_else(|| Error::format(path, "no <Image> element in XISF header"))
}

fn attr_string(path: &Path, e: &quick_xml::events::BytesStart, key: &[u8]) -> Result<Option<String>> {
    for attr in e.attributes() {
        let attr = attr.map_err(|err| Error::format(path, format!("bad XML attribute: {err}")))?;
        if attr.key.local_name().as_ref() == key {
            let v = attr
                .decode_and_unescape_value(quick_xml::Decoder {})
                .map_err(|err| Error::format(path, format!("bad XML attribute value: {err}")))?;
            return Ok(Some(v.into_owned()));
        }
    }
    Ok(None)
}

fn parse_image_element(path: &Path, e: &quick_xml::events::BytesStart) -> Result<XisfHeader> {
    let geometry = attr_string(path, e, b"geometry")?
        .ok_or_else(|| Error::format(path, "<Image> missing geometry"))?;
    let dims: Vec<u64> = geometry.split(':').map(|p| p.parse().unwrap_or(0)).collect();
    let (width, height, channels) = match dims.as_slice() {
        [w, h, c] if *w > 0 && *h > 0 && *c > 0 => (*w, *h, *c),
        _ => return Err(Error::format(path, format!("unsupported geometry '{geometry}' (need W:H:C)"))),
    };

    let sample_format_s = attr_string(path, e, b"sampleFormat")?
        .ok_or_else(|| Error::format(path, "<Image> missing sampleFormat"))?;
    let sample_format = SampleFormat::parse(&sample_format_s)
        .ok_or_else(|| Error::format(path, format!("unknown sampleFormat '{sample_format_s}'")))?;

    let location = attr_string(path, e, b"location")?
        .ok_or_else(|| Error::format(path, "<Image> missing location"))?;
    let loc: Vec<&str> = location.split(':').collect();
    let (data_offset, data_size) = match loc.as_slice() {
        ["attachment", off, size] => (
            off.parse::<u64>()
                .map_err(|_| Error::format(path, format!("bad attachment offset in '{location}'")))?,
            size.parse::<u64>()
                .map_err(|_| Error::format(path, format!("bad attachment size in '{location}'")))?,
        ),
        _ => {
            return Err(Error::format(
                path,
                format!("unsupported image location '{location}' (only attachment supported)"),
            ));
        }
    };

    if let Some(compression) = attr_string(path, e, b"compression")? {
        return Err(Error::format(
            path,
            format!("compressed XISF not yet supported (compression='{compression}')"),
        ));
    }
    if let Some(storage) = attr_string(path, e, b"pixelStorage")?
        && storage != "Planar"
    {
        return Err(Error::format(path, format!("unsupported pixelStorage '{storage}'")));
    }
    if let Some(order) = attr_string(path, e, b"byteOrder")?
        && order != "little"
    {
        return Err(Error::format(path, format!("unsupported byteOrder '{order}'")));
    }

    Ok(XisfHeader {
        width,
        height,
        channels,
        sample_format,
        data_offset,
        data_size,
        fits_keywords: Vec::new(),
    })
}

fn parse_fits_keyword(path: &Path, e: &quick_xml::events::BytesStart) -> Result<FitsKeyword> {
    Ok(FitsKeyword {
        name: attr_string(path, e, b"name")?.unwrap_or_default(),
        value: attr_string(path, e, b"value")?.unwrap_or_default(),
        comment: attr_string(path, e, b"comment")?.unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal monolithic XISF file: 3x2 image, 2 channels, Float32.
    fn synth_xisf(dir: &Path) -> PathBuf {
        let (w, h, c) = (3u64, 2u64, 2u64);
        let data_offset = 512u64;
        let data_size = w * h * c * 4;
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><xisf version="1.0" xmlns="http://www.pixinsight.com/xisf"><Image geometry="{w}:{h}:{c}" sampleFormat="Float32" colorSpace="Gray" location="attachment:{data_offset}:{data_size}"><FITSKeyword name="OBJECT" value="'Test'" comment="test object"/></Image></xisf>"#
        );
        let mut bytes = Vec::new();
        bytes.extend_from_slice(SIGNATURE);
        bytes.extend_from_slice(&(xml.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(xml.as_bytes());
        bytes.resize(data_offset as usize, 0);
        for i in 0..(w * h * c) {
            bytes.extend_from_slice(&(i as f32).to_le_bytes());
        }
        let path = dir.join("synth.xisf");
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn reads_synthetic_xisf() {
        let dir = std::env::temp_dir().join(format!("mmm-xisf-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = synth_xisf(&dir);

        let panel = XisfPanel::open(&path).unwrap();
        assert_eq!(panel.width(), 3);
        assert_eq!(panel.height(), 2);
        assert_eq!(panel.channels(), 2);
        assert_eq!(panel.header().fits_keywords.len(), 1);
        assert_eq!(panel.header().fits_keywords[0].name, "OBJECT");

        // Channel 0 = values 0..6, channel 1 = 6..12; row 1 of channel 1 = [9,10,11].
        assert_eq!(panel.channel(0), &[0.0, 1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(panel.row(1, 1), &[9.0, 10.0, 11.0]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_bad_signature() {
        let dir = std::env::temp_dir().join(format!("mmm-xisf-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.xisf");
        std::fs::write(&path, b"NOTXISF0 garbage").unwrap();
        assert!(XisfPanel::open(&path).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
