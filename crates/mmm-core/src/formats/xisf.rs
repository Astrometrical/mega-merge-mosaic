//! Minimal XISF reader for PixInsight-produced mosaic panels.
//!
//! Supports the subset MosaicByCoordinates emits: a monolithic XISF file with
//! one `<Image>` element, planar pixel storage, little-endian Float32 samples,
//! uncompressed attachment data block. Anything else errors clearly; compressed
//! blocks will be handled later by decompressing into the tile cache.
//!
//! Spec: <https://pixinsight.com/doc/docs/XISF-1.0-spec/XISF-1.0-spec.html>

use std::fs::File;
use std::path::{Path, PathBuf};

use memmap2::Mmap;
use quick_xml::events::Event;

use super::{FitsKeyword, PropertyValue, SampleFormat, XisfProperty};
use crate::{Error, Result};

const SIGNATURE: &[u8; 8] = b"XISF0100";

/// Parsed metadata for the image block of an XISF file.
#[derive(Debug, Clone)]
pub struct XisfHeader {
    /// Image width in pixels.
    pub width: u64,
    /// Image height in pixels.
    pub height: u64,
    /// Number of channels (planar planes).
    pub channels: u64,
    /// Per-sample data type; only [`SampleFormat::Float32`] is readable.
    pub sample_format: SampleFormat,
    /// Byte offset of the attached data block within the file.
    pub data_offset: u64,
    /// Byte size of the attached data block.
    pub data_size: u64,
    /// All `<FITSKeyword>` cards of the image element, in header order.
    pub fits_keywords: Vec<FitsKeyword>,
    /// All `<Property>` elements in the header (image-level and file-level).
    /// Attachment-located f64 vectors/matrices are resolved by [`XisfPanel::open`].
    pub properties: Vec<XisfProperty>,
}

/// A memory-mapped, read-only XISF panel exposing planar f32 channel data.
pub struct XisfPanel {
    path: PathBuf,
    mmap: Mmap,
    header: XisfHeader,
}

impl XisfPanel {
    /// Open and validate a monolithic XISF file, resolving attachment-located
    /// f64 vector/matrix properties (e.g. astrometric solution data) so
    /// downstream code sees decoded values. Errors on anything outside the
    /// supported subset (module docs): non-Float32 samples, compressed or
    /// non-planar storage, malformed headers.
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).map_err(|e| Error::io(path, e))?;
        // SAFETY: read-only map; we accept that truncation by another process
        // during a run is undefined, as is conventional for mmap-based readers.
        let mmap = unsafe { Mmap::map(&file) }.map_err(|e| Error::io(path, e))?;
        let mut header = parse_header(path, &mmap)?;

        // Resolve attachment-located f64 vectors/matrices (tiny blocks — e.g.
        // astrometric solution data) so downstream code sees decoded values.
        for prop in &mut header.properties {
            if prop.location.is_some() && prop.value.needs_attachment_data() {
                prop.value = read_attached_f64s(&mmap, path, prop)?;
            }
        }

        header
            .data_offset
            .checked_add(header.data_size)
            .filter(|&end| end <= mmap.len() as u64)
            .ok_or_else(|| Error::format(path, "data block extends past end of file"))?;
        let expected = header.width
            * header.height
            * header.channels
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

        if header.sample_format != SampleFormat::Float32 {
            return Err(Error::format(
                path,
                format!(
                    "unsupported sample format {:?} (only Float32 for now)",
                    header.sample_format
                ),
            ));
        }
        if header.data_offset % 4 != 0 {
            // PixInsight aligns attachments to 4096 bytes; anything unaligned
            // would break the zero-copy f32 view.
            return Err(Error::format(path, "attachment offset not 4-byte aligned"));
        }

        Ok(Self {
            path: path.to_path_buf(),
            mmap,
            header,
        })
    }

    /// The file this panel was opened from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Parsed header metadata (geometry, FITS keywords, properties).
    pub fn header(&self) -> &XisfHeader {
        &self.header
    }

    /// Image width in pixels.
    pub fn width(&self) -> u64 {
        self.header.width
    }

    /// Image height in pixels.
    pub fn height(&self) -> u64 {
        self.header.height
    }

    /// Number of channels (planar planes).
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
    /// A no-op where `madvise(2)` is unavailable (e.g. Windows).
    pub fn advise_sequential(&self) {
        #[cfg(unix)]
        let _ = self.mmap.advise(memmap2::Advice::Sequential);
    }
}

fn parse_header(path: &Path, mmap: &Mmap) -> Result<XisfHeader> {
    if mmap.len() < 16 || &mmap[0..8] != SIGNATURE {
        return Err(Error::format(
            path,
            "not a monolithic XISF file (bad signature)",
        ));
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
    let mut properties: Vec<XisfProperty> = Vec::new();
    // <Property> currently being read: (partial parse, accumulated text,
    // nesting depth of unrelated child elements to skip).
    let mut pending: Option<(PendingProperty, String, u32)> = None;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.local_name();
                let name = name.as_ref();
                if let Some((_, _, depth)) = pending.as_mut() {
                    *depth += 1; // nested element inside <Property> (e.g. <Data>)
                } else if name == b"Property" {
                    pending = Some((parse_property_start(path, &e)?, String::new(), 0));
                } else if name == b"Image" && header.is_none() {
                    header = Some(parse_image_element(path, &e)?);
                    in_image = true;
                } else if name == b"FITSKeyword"
                    && in_image
                    && let Some(h) = header.as_mut()
                {
                    h.fits_keywords.push(parse_fits_keyword(path, &e)?);
                }
            }
            Ok(Event::Empty(e)) => {
                let name = e.local_name();
                let name = name.as_ref();
                if pending.is_some() {
                    // self-closing child inside <Property>: nothing to track
                } else if name == b"Property" {
                    let partial = parse_property_start(path, &e)?;
                    properties.push(finish_property(path, partial, "")?);
                } else if name == b"Image" && header.is_none() {
                    header = Some(parse_image_element(path, &e)?);
                } else if name == b"FITSKeyword"
                    && in_image
                    && let Some(h) = header.as_mut()
                {
                    h.fits_keywords.push(parse_fits_keyword(path, &e)?);
                }
            }
            Ok(Event::Text(t)) => {
                if let Some((_, text, 0)) = pending.as_mut() {
                    let s = t
                        .unescape()
                        .map_err(|e| Error::format(path, format!("bad XML text: {e}")))?;
                    text.push_str(&s);
                }
            }
            Ok(Event::CData(t)) => {
                if let Some((_, text, 0)) = pending.as_mut() {
                    text.push_str(&String::from_utf8_lossy(&t));
                }
            }
            Ok(Event::End(e)) => {
                if let Some((_, _, depth)) = pending.as_mut() {
                    if *depth > 0 {
                        *depth -= 1;
                    } else {
                        let (partial, text, _) = pending.take().unwrap();
                        properties.push(finish_property(path, partial, &text)?);
                    }
                } else if e.local_name().as_ref() == b"Image" {
                    in_image = false;
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(Error::format(path, format!("XML parse error: {e}"))),
            _ => {}
        }
        buf.clear();
    }

    let mut header =
        header.ok_or_else(|| Error::format(path, "no <Image> element in XISF header"))?;
    header.properties = properties;
    Ok(header)
}

/// `<Property>` attributes captured before its element text is known.
struct PendingProperty {
    id: String,
    type_: String,
    value_attr: Option<String>,
    location: Option<String>,
    length: Option<u64>,
    rows: Option<u32>,
    cols: Option<u32>,
}

fn parse_property_start(path: &Path, e: &quick_xml::events::BytesStart) -> Result<PendingProperty> {
    fn parse_num<T: std::str::FromStr>(s: Option<String>) -> Option<T> {
        s.and_then(|s| s.parse().ok())
    }
    Ok(PendingProperty {
        id: attr_string(path, e, b"id")?.unwrap_or_default(),
        type_: attr_string(path, e, b"type")?.unwrap_or_default(),
        value_attr: attr_string(path, e, b"value")?,
        location: attr_string(path, e, b"location")?,
        length: parse_num(attr_string(path, e, b"length")?),
        rows: parse_num(attr_string(path, e, b"rows")?),
        cols: parse_num(attr_string(path, e, b"columns")?),
    })
}

/// Decode a completed `<Property>` from its attributes and element text.
///
/// Shapes handled (all three occur in PixInsight files):
/// - scalars via the `value` attribute (or element text as fallback),
/// - String/TimePoint via element text or `value`,
/// - `F64Vector`/`F64Matrix` via `location="inline:base64|inline:hex"`
///   (payload is the element text) or `location="attachment:offset:size"`
///   (value left with empty data; resolved from the file by the caller).
///
/// Types we do not decode become [`PropertyValue::Unread`] (or `Str` when a
/// plain `value` attribute is present).
fn finish_property(path: &Path, p: PendingProperty, text: &str) -> Result<XisfProperty> {
    let err = |msg: String| Error::format(path, msg);
    let scalar = |p: &PendingProperty| -> String {
        p.value_attr
            .clone()
            .unwrap_or_else(|| text.trim().to_string())
    };

    let mut location = None;
    let value = match p.type_.as_str() {
        "Float32" | "Float64" => PropertyValue::F64(
            scalar(&p)
                .parse()
                .map_err(|_| err(format!("property {}: bad float value", p.id)))?,
        ),
        "Int8" | "Int16" | "Int32" | "Int64" | "UInt8" | "UInt16" | "UInt32" | "UInt64" => {
            PropertyValue::I64(
                scalar(&p)
                    .parse()
                    .map_err(|_| err(format!("property {}: bad integer value", p.id)))?,
            )
        }
        "Boolean" => match scalar(&p).as_str() {
            "1" | "true" => PropertyValue::I64(1),
            "0" | "false" => PropertyValue::I64(0),
            v => return Err(err(format!("property {}: bad boolean '{v}'", p.id))),
        },
        "String" | "TimePoint" => PropertyValue::Str(
            p.value_attr
                .clone()
                .unwrap_or_else(|| text.trim().to_string()),
        ),
        "F64Vector" | "F64Matrix" => {
            let n = if p.type_ == "F64Vector" {
                p.length
                    .ok_or_else(|| err(format!("property {}: missing length", p.id)))?
                    as usize
            } else {
                let (r, c) = p
                    .rows
                    .zip(p.cols)
                    .ok_or_else(|| err(format!("property {}: missing rows/columns", p.id)))?;
                (r as usize) * (c as usize)
            };
            let data: Vec<f64> = match p.location.as_deref() {
                Some(loc) if loc.starts_with("inline:") => {
                    let bytes = decode_inline(loc, text).ok_or_else(|| {
                        err(format!("property {}: bad inline data ({loc})", p.id))
                    })?;
                    if bytes.len() != n * 8 {
                        return Err(err(format!(
                            "property {}: inline data is {} bytes, expected {}",
                            p.id,
                            bytes.len(),
                            n * 8
                        )));
                    }
                    bytes
                        .chunks_exact(8)
                        .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
                        .collect()
                }
                Some(loc) if loc.starts_with("attachment:") => {
                    let mut it = loc.splitn(3, ':').skip(1);
                    let off: u64 = it.next().and_then(|s| s.parse().ok()).ok_or_else(|| {
                        err(format!(
                            "property {}: bad attachment location '{loc}'",
                            p.id
                        ))
                    })?;
                    let size: u64 = it.next().and_then(|s| s.parse().ok()).ok_or_else(|| {
                        err(format!(
                            "property {}: bad attachment location '{loc}'",
                            p.id
                        ))
                    })?;
                    if size != (n * 8) as u64 {
                        return Err(err(format!(
                            "property {}: attachment is {size} bytes, expected {}",
                            p.id,
                            n * 8
                        )));
                    }
                    location = Some((off, size));
                    Vec::new() // resolved from the file by the caller
                }
                other => {
                    return Err(err(format!(
                        "property {}: unsupported vector/matrix location {other:?}",
                        p.id
                    )));
                }
            };
            if p.type_ == "F64Vector" {
                PropertyValue::F64Vec(data)
            } else {
                PropertyValue::F64Mat {
                    rows: p.rows.unwrap(),
                    cols: p.cols.unwrap(),
                    data,
                }
            }
        }
        _ => match p.value_attr.clone() {
            Some(v) => PropertyValue::Str(v),
            None => PropertyValue::Unread,
        },
    };

    Ok(XisfProperty {
        id: p.id,
        type_: p.type_,
        value,
        location,
    })
}

/// Decode `inline:base64` / `inline:hex` payload text to raw bytes.
fn decode_inline(location: &str, text: &str) -> Option<Vec<u8>> {
    match location {
        "inline:base64" => base64_decode(text),
        "inline:hex" => hex_decode(text),
        _ => None,
    }
}

/// Minimal standard-alphabet base64 decoder (payloads here are tens of bytes;
/// not worth a dependency). Ignores whitespace and padding.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return None,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    let digits: Vec<u8> = s
        .bytes()
        .filter(|b| !b.is_ascii_whitespace())
        .map(|b| match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        })
        .collect::<Option<_>>()?;
    if !digits.len().is_multiple_of(2) {
        return None;
    }
    Some(digits.chunks_exact(2).map(|p| (p[0] << 4) | p[1]).collect())
}

/// Typed reader for an attachment-located f64 vector/matrix property: reads
/// `location` bytes from the mapped file and returns the filled-in value.
fn read_attached_f64s(file: &[u8], path: &Path, prop: &XisfProperty) -> Result<PropertyValue> {
    let (off, size) = prop
        .location
        .ok_or_else(|| Error::format(path, format!("property {} has no attachment", prop.id)))?;
    let end = off
        .checked_add(size)
        .filter(|&e| e <= file.len() as u64)
        .ok_or_else(|| {
            Error::format(
                path,
                format!("property {}: attachment out of bounds", prop.id),
            )
        })?;
    let bytes = &file[off as usize..end as usize];
    if !bytes.len().is_multiple_of(8) {
        return Err(Error::format(
            path,
            format!(
                "property {}: attachment size {} not a multiple of 8",
                prop.id, size
            ),
        ));
    }
    let data: Vec<f64> = bytes
        .chunks_exact(8)
        .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
        .collect();
    match &prop.value {
        PropertyValue::F64Vec(_) => Ok(PropertyValue::F64Vec(data)),
        PropertyValue::F64Mat { rows, cols, .. } => Ok(PropertyValue::F64Mat {
            rows: *rows,
            cols: *cols,
            data,
        }),
        _ => Err(Error::format(
            path,
            format!("property {}: not an f64 vector/matrix", prop.id),
        )),
    }
}

fn attr_string(
    path: &Path,
    e: &quick_xml::events::BytesStart,
    key: &[u8],
) -> Result<Option<String>> {
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
    let dims: Vec<u64> = geometry
        .split(':')
        .map(|p| p.parse().unwrap_or(0))
        .collect();
    let (width, height, channels) = match dims.as_slice() {
        [w, h, c] if *w > 0 && *h > 0 && *c > 0 => (*w, *h, *c),
        _ => {
            return Err(Error::format(
                path,
                format!("unsupported geometry '{geometry}' (need W:H:C)"),
            ));
        }
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
            off.parse::<u64>().map_err(|_| {
                Error::format(path, format!("bad attachment offset in '{location}'"))
            })?,
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
        return Err(Error::format(
            path,
            format!("unsupported pixelStorage '{storage}'"),
        ));
    }
    if let Some(order) = attr_string(path, e, b"byteOrder")?
        && order != "little"
    {
        return Err(Error::format(
            path,
            format!("unsupported byteOrder '{order}'"),
        ));
    }

    Ok(XisfHeader {
        width,
        height,
        channels,
        sample_format,
        data_offset,
        data_size,
        fits_keywords: Vec::new(),
        properties: Vec::new(),
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

    /// Base64-encode f64s little-endian (test helper mirroring what PixInsight
    /// writes for `location="inline:base64"` properties).
    fn b64_f64s(data: &[f64]) -> String {
        const ALPHA: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let bytes: Vec<u8> = data.iter().flat_map(|v| v.to_le_bytes()).collect();
        let mut s = String::new();
        for chunk in bytes.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
            for i in 0..4 {
                if i <= chunk.len() {
                    s.push(ALPHA[(n >> (18 - 6 * i)) as usize & 63] as char);
                } else {
                    s.push('=');
                }
            }
        }
        s
    }

    /// Synthetic XISF exercising all property shapes: scalar `value` attrs,
    /// element text, inline base64 vectors/matrices, and an attachment-located
    /// vector whose payload lives at byte 256.
    fn synth_xisf_with_properties(dir: &Path) -> PathBuf {
        let (w, h, c) = (2u64, 2u64, 1u64);
        let data_offset = 2048u64;
        let data_size = w * h * c * 4;
        let att = [10.5f64, -20.25, 30.0];
        let vec2 = [4627.5f64, 9155.0];
        let mat = [-4.4e-4f64, 0.0, 0.0, 4.4e-4];
        let xml = format!(
            concat!(
                r#"<?xml version="1.0" encoding="UTF-8"?>"#,
                r#"<xisf version="1.0" xmlns="http://www.pixinsight.com/xisf">"#,
                r#"<Image geometry="{w}:{h}:{c}" sampleFormat="Float32" colorSpace="Gray" location="attachment:{off}:{size}">"#,
                r#"<Property id="P:Scalar" type="Float64" value="1.5"/>"#,
                r#"<Property id="P:Int" type="Int32" value="-7"/>"#,
                r#"<Property id="P:Time" type="TimePoint" value="2026-07-24T03:43:50.946Z"/>"#,
                r#"<Property id="P:Text" type="String">hello &amp; world</Property>"#,
                r#"<Property id="P:Vec" type="F64Vector" length="2" location="inline:base64">{vec_b64}</Property>"#,
                r#"<Property id="P:Mat" type="F64Matrix" rows="2" columns="2" location="inline:base64">{mat_b64}</Property>"#,
                r#"<Property id="P:Att" type="F64Vector" length="3" location="attachment:1536:24"/>"#,
                r#"<Property id="P:Odd" type="I32Vector" length="2" location="inline:base64">AAAAAAEAAAA=</Property>"#,
                r#"</Image></xisf>"#,
            ),
            w = w,
            h = h,
            c = c,
            off = data_offset,
            size = data_size,
            vec_b64 = b64_f64s(&vec2),
            mat_b64 = b64_f64s(&mat),
        );
        let mut bytes = Vec::new();
        bytes.extend_from_slice(SIGNATURE);
        bytes.extend_from_slice(&(xml.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(xml.as_bytes());
        assert!(
            bytes.len() <= 1536,
            "header must fit below the attachment payload"
        );
        bytes.resize(1536, 0);
        for v in att {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        bytes.resize(data_offset as usize, 0);
        for i in 0..(w * h * c) {
            bytes.extend_from_slice(&(i as f32).to_le_bytes());
        }
        let path = dir.join("synth-props.xisf");
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn parses_properties_in_all_shapes() {
        let dir = std::env::temp_dir().join(format!("mmm-xisf-props-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = synth_xisf_with_properties(&dir);

        let panel = XisfPanel::open(&path).unwrap();
        let props = &panel.header().properties;
        let get = |id: &str| {
            props
                .iter()
                .find(|p| p.id == id)
                .unwrap_or_else(|| panic!("property {id} missing"))
        };

        assert_eq!(get("P:Scalar").value.as_f64(), Some(1.5));
        assert_eq!(get("P:Scalar").type_, "Float64");
        assert_eq!(get("P:Int").value, PropertyValue::I64(-7));
        assert_eq!(
            get("P:Time").value.as_str(),
            Some("2026-07-24T03:43:50.946Z")
        );
        assert_eq!(get("P:Text").value.as_str(), Some("hello & world"));
        assert_eq!(get("P:Vec").value.as_f64_vec(), Some(&[4627.5, 9155.0][..]));
        let (rows, cols, data) = get("P:Mat").value.as_f64_mat().unwrap();
        assert_eq!((rows, cols), (2, 2));
        assert_eq!(data, &[-4.4e-4, 0.0, 0.0, 4.4e-4]);

        let att = get("P:Att");
        assert_eq!(
            att.location,
            Some((1536, 24)),
            "attachment offset/size exposed"
        );
        assert_eq!(
            att.value.as_f64_vec(),
            Some(&[10.5, -20.25, 30.0][..]),
            "attachment-located f64 vector resolved on open"
        );

        assert_eq!(
            get("P:Odd").value,
            PropertyValue::Unread,
            "undecoded types stay Unread"
        );

        // Image data still reads with properties present.
        assert_eq!(panel.channel(0), &[0.0, 1.0, 2.0, 3.0]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn decodes_real_pixinsight_inline_base64() {
        // Exact bytes captured from a real MosaicByCoordinates panel header
        // (PCL:AstrometricSolution:LinearTransformationMatrix).
        let bytes = base64_decode("qj/NbcwTPb8AAAAAAAAAAAAAAAAAAAAAqj/NbcwTPT8=").unwrap();
        let vals: Vec<f64> = bytes
            .chunks_exact(8)
            .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
            .collect();
        assert_eq!(
            vals,
            vec![-4.436849683786501e-4, 0.0, 0.0, 4.436849683786501e-4]
        );
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
