use core::ops::{Deref, DerefMut};

use bun_engine_text::{AllocError, MAX_LENGTH};
use smallvec::SmallVec;

/// Bytes on the stack while they are few, and never more than a string can have.
#[derive(Default)]
pub(crate) struct Buffer(SmallVec<[u8; 256]>);

impl Buffer {
    #[inline]
    pub(crate) fn new() -> Buffer {
        Buffer(SmallVec::new())
    }

    #[inline(always)]
    fn reserve(&mut self, additional: usize) -> Result<(), AllocError> {
        if additional > self.0.capacity() - self.0.len() {
            return self.grow(additional);
        }
        Ok(())
    }

    #[cold]
    fn grow(&mut self, additional: usize) -> Result<(), AllocError> {
        let length = self.0.len().saturating_add(additional);
        if length > MAX_LENGTH {
            return Err(AllocError);
        }
        let capacity = length.max(self.0.capacity() * 2).min(MAX_LENGTH);
        self.0.try_grow(capacity).map_err(|_| AllocError)
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, byte: u8) -> Result<(), AllocError> {
        self.reserve(1)?;
        self.0.push(byte);
        Ok(())
    }

    #[inline(always)]
    pub(crate) fn extend(&mut self, bytes: &[u8]) -> Result<(), AllocError> {
        self.insert(self.0.len(), bytes)
    }

    #[inline(always)]
    pub(crate) fn insert(&mut self, index: usize, bytes: &[u8]) -> Result<(), AllocError> {
        self.reserve(bytes.len())?;
        self.0.insert_from_slice(index, bytes);
        Ok(())
    }

    pub(crate) fn remove(&mut self, index: usize, count: usize) {
        self.0.drain(index..index + count);
    }

    #[inline]
    pub(crate) fn truncate(&mut self, length: usize) {
        self.0.truncate(length);
    }
}

impl Deref for Buffer {
    type Target = [u8];

    #[inline(always)]
    fn deref(&self) -> &[u8] {
        &self.0
    }
}

impl DerefMut for Buffer {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}
