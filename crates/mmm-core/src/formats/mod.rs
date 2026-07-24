//! Input format readers.
//!
//! All readers expose the same shape of data: per-channel planar `f32` planes
//! over a memory-mapped file, plus passthrough metadata (FITS keywords, WCS).

pub mod xisf;

/// A FITS header keyword carried through from input to output.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FitsKeyword {
    pub name: String,
    pub value: String,
    pub comment: String,
}

/// Sample formats we recognize in headers. Only `Float32` is readable for now —
/// MosaicByCoordinates output is always Float32/Float64, and Float32 is the
/// overwhelmingly common case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    UInt8,
    UInt16,
    UInt32,
    Float32,
    Float64,
}

impl SampleFormat {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "UInt8" => Self::UInt8,
            "UInt16" => Self::UInt16,
            "UInt32" => Self::UInt32,
            "Float32" => Self::Float32,
            "Float64" => Self::Float64,
            _ => return None,
        })
    }

    pub fn bytes_per_sample(self) -> usize {
        match self {
            Self::UInt8 => 1,
            Self::UInt16 => 2,
            Self::UInt32 | Self::Float32 => 4,
            Self::Float64 => 8,
        }
    }
}
