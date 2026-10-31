//! Content-defined chunking with FastCDC. Boundaries depend on the bytes around them, so inserting or deleting a few bytes in a large file moves only the
//! chunk boundaries next to the edit; every other chunk keeps its hash and is stored once.

use crate::error::{Error, Result};
use fastcdc::v2020::StreamCDC;
use std::io::Read;

#[derive(Clone, Copy, Debug)]
pub struct ChunkSizes {
    pub min: usize,
    pub avg: usize,
    pub max: usize,
}

impl Default for ChunkSizes {
    fn default() -> Self {
        Self {
            min: 256 << 10,
            avg: 1 << 20,
            max: 4 << 20,
        }
    }
}

pub struct Chunker<R: Read> {
    inner: StreamCDC<R>,
}

impl<R: Read> Chunker<R> {
    pub fn new(r: R, s: ChunkSizes) -> Self {
        Self {
            inner: StreamCDC::new(r, s.min, s.avg, s.max),
        }
    }
}

impl<R: Read> Iterator for Chunker<R> {
    type Item = Result<Vec<u8>>;
    fn next(&mut self) -> Option<Self::Item> {
        match self.inner.next()? {
            Ok(c) => Some(Ok(c.data)),
            Err(e) => Some(Err(Error::Io(std::io::Error::other(e.to_string())))),
        }
    }
}
