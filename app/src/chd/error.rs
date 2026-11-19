#![forbid(unsafe_code)]
//! Why a CHD could not be opened or read.

use std::fmt;

/// A CHD failure. The messages are the format's customary ones, so a person
/// who has seen chdman's or another reader's wording recognises them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChdError {
    OutOfMemory,
    InvalidFile,
    InvalidParameter,
    InvalidData,
    RequiresParent,
    ReadError,
    CodecError,
    HunkOutOfRange,
    DecompressionError,
    MetadataNotFound,
    UnsupportedVersion,
    UnsupportedFormat,
}

impl fmt::Display for ChdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ChdError::OutOfMemory => "out of memory",
            ChdError::InvalidFile => "invalid file",
            ChdError::InvalidParameter => "invalid parameter",
            ChdError::InvalidData => "invalid data",
            ChdError::RequiresParent => "requires parent",
            ChdError::ReadError => "read error",
            ChdError::CodecError => "codec error",
            ChdError::HunkOutOfRange => "hunk out of range",
            ChdError::DecompressionError => "decompression error",
            ChdError::MetadataNotFound => "can't find metadata",
            ChdError::UnsupportedVersion => "unsupported CHD version",
            ChdError::UnsupportedFormat => "unsupported format",
        })
    }
}

impl std::error::Error for ChdError {}
