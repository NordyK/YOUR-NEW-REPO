//! Feed frame corpus reader.

use std::fs::File;
use std::io::{BufRead, BufReader, Result as IoResult};
use std::path::Path;

/// One frame from the replay corpus: the raw JSON envelope as bytes.
#[derive(Debug, Clone)]
pub struct FeedFrame {
    /// 0-based index in the corpus.
    pub index: u64,
    /// Raw envelope JSON bytes.
    pub raw: Vec<u8>,
}

/// Source of replay frames. Wraps a line-oriented file reader (one frame per line).
pub struct FeedFrameSource {
    inner: Box<dyn Iterator<Item = IoResult<String>>>,
    counter: u64,
}

impl FeedFrameSource {
    /// Open a file at `path`.
    pub fn open(path: impl AsRef<Path>) -> IoResult<Self> {
        let f = File::open(path)?;
        let r = BufReader::new(f);
        Ok(Self {
            inner: Box::new(r.lines()),
            counter: 0,
        })
    }

    /// Construct from an in-memory iterator (used in tests).
    pub fn from_lines(lines: Vec<String>) -> Self {
        Self {
            inner: Box::new(lines.into_iter().map(Ok)),
            counter: 0,
        }
    }
}

impl Iterator for FeedFrameSource {
    type Item = FeedFrame;
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.inner.next()? {
                Ok(line) => {
                    let trimmed = line.trim().to_string();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let frame = FeedFrame {
                        index: self.counter,
                        raw: trimmed.into_bytes(),
                    };
                    self.counter += 1;
                    return Some(frame);
                }
                Err(_) => return None,
            }
        }
    }
}
