//! Whole-file UTF-8 reads capped at a byte limit.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

/// The editor's document limit, as a file length.
pub(crate) const MAX_DOCUMENT_BYTES: u64 = super::editor::DEFAULT_MAX_DOCUMENT_BYTES as u64;

/// Why a bounded read gave no text.
#[derive(Debug)]
pub(crate) enum ReadError {
    /// The file would not open or read; text that is not UTF-8 is
    /// `InvalidData`.
    Io(io::Error),
    /// The file is over the limit: its length, or `limit + 1`, a lower
    /// bound, when the read itself ran past the limit.
    TooLarge(u64),
}

/// A whole file as UTF-8 text when it is at most `limit` bytes.
pub(crate) fn read_to_string(path: &Path, limit: u64) -> Result<String, ReadError> {
    let file = File::open(path).map_err(ReadError::Io)?;
    let size = file.metadata().map_err(ReadError::Io)?.len();
    if size > limit {
        return Err(ReadError::TooLarge(size));
    }
    // The size can change between metadata and read, and some inputs report
    // no useful length. Limit the read itself as well as the initial check.
    read_bounded(file, limit)
}

fn read_bounded(reader: impl Read, limit: u64) -> Result<String, ReadError> {
    let mut bytes = Vec::new();
    reader
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(ReadError::Io)?;
    if bytes.len() as u64 > limit {
        return Err(ReadError::TooLarge(bytes.len() as u64));
    }
    String::from_utf8(bytes)
        .map_err(|error| ReadError::Io(io::Error::new(io::ErrorKind::InvalidData, error)))
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::Cursor;

    use super::*;

    #[test]
    fn stream_limit_accepts_the_limit_and_refuses_one_more_byte() {
        let source = vec![b'x'; MAX_DOCUMENT_BYTES as usize + 1];
        assert_eq!(
            read_bounded(
                Cursor::new(&source[..MAX_DOCUMENT_BYTES as usize]),
                MAX_DOCUMENT_BYTES
            )
            .unwrap(),
            "x".repeat(MAX_DOCUMENT_BYTES as usize)
        );
        assert!(matches!(
            read_bounded(Cursor::new(source), MAX_DOCUMENT_BYTES),
            Err(ReadError::TooLarge(size)) if size == MAX_DOCUMENT_BYTES + 1
        ));
    }

    #[test]
    fn file_length_refuses_a_large_sparse_file_without_reading_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("prebake.strudel");
        // A capped read reports `limit + 1`, so only the length check gives
        // this size.
        let length = MAX_DOCUMENT_BYTES + 2;
        File::create(&path).unwrap().set_len(length).unwrap();
        assert!(matches!(
            read_to_string(&path, MAX_DOCUMENT_BYTES),
            Err(ReadError::TooLarge(size)) if size == length
        ));
    }
}
