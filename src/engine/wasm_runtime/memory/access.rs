//! Reads and writes of the run time. Other threads may access the same bytes meanwhile, so no reference covers them.

use core::ptr;

use bun_engine_wasm_compiler::abi::Trap;

use super::Memory;

/// The ranges may overlap.
///
/// # Safety
/// Both are accessible.
#[inline]
pub(crate) unsafe fn copy_bytes(dst: *mut u8, src: *const u8, count: usize) {
    // SAFETY: the caller's.
    unsafe { ptr::copy(src, dst, count) };
}

/// # Safety
/// The range is accessible.
#[inline]
pub(crate) unsafe fn fill_bytes(dst: *mut u8, value: u8, count: usize) {
    // SAFETY: the caller's.
    unsafe { ptr::write_bytes(dst, value, count) };
}

impl Memory {
    /// The bytes stay there until the memory grows.
    #[inline]
    pub(crate) fn address_of(&self, offset: u64, count: u64) -> Result<*mut u8, Trap> {
        let pages = self.pages.borrow();
        match offset.checked_add(count) {
            Some(end) if end <= pages.len() => Ok(pages.base().wrapping_add(offset as usize)),
            _ => Err(Trap::MemoryOutOfBounds),
        }
    }

    pub fn write(&self, offset: u64, bytes: &[u8]) -> Result<(), Trap> {
        let dst = self.address_of(offset, bytes.len() as u64)?;
        // SAFETY: in bounds.
        unsafe { copy_bytes(dst, bytes.as_ptr(), bytes.len()) };
        Ok(())
    }

    pub fn read(&self, offset: u64, bytes: &mut [u8]) -> Result<(), Trap> {
        let src = self.address_of(offset, bytes.len() as u64)?;
        // SAFETY: in bounds.
        unsafe { copy_bytes(bytes.as_mut_ptr(), src, bytes.len()) };
        Ok(())
    }
}
