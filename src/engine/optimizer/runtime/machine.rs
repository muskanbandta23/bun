//! What the thunks that leave optimized code hand to Rust and take back.

use core::mem::offset_of;

#[cfg(target_arch = "x86_64")]
mod counts {
    pub(super) const GPRS: usize = 16;
    pub(super) const FPRS: usize = 16;
}

#[cfg(target_arch = "aarch64")]
mod counts {
    pub(super) const GPRS: usize = 32;
    pub(super) const FPRS: usize = 32;
}

/// The registers as the thunk saved them, by register number. The stack pointer holds what it had in the thunk.
#[repr(C)]
pub(crate) struct Saved {
    pub(crate) gprs: [u64; counts::GPRS],
    /// The low 64 bits, which is all that a value needs.
    pub(crate) fprs: [u64; counts::FPRS],
}

impl Saved {
    pub(crate) const SIZE: usize = size_of::<Saved>();
    pub(crate) const OFFSET_OF_FPRS: usize = offset_of!(Saved, fprs);

    pub(crate) fn gpr(&self, number: u8) -> u64 {
        self.gprs[usize::from(number)]
    }

    pub(crate) fn fpr(&self, number: u8) -> u64 {
        self.fprs[usize::from(number)]
    }
}

const _: () = assert!(Saved::SIZE % 16 == 0);

/// Where the frames that have been built go, and where to carry on.
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub(crate) struct Transfer {
    /// The first word of the image.
    pub(crate) source: usize,
    /// Its lowest address on the stack.
    pub(crate) destination: usize,
    pub(crate) words: usize,
    pub(crate) frame_pointer: usize,
    pub(crate) target: usize,
}

impl Transfer {
    pub(crate) const OFFSET_OF_SOURCE: usize = offset_of!(Transfer, source);
    pub(crate) const OFFSET_OF_DESTINATION: usize = offset_of!(Transfer, destination);
    pub(crate) const OFFSET_OF_WORDS: usize = offset_of!(Transfer, words);
    pub(crate) const OFFSET_OF_FRAME_POINTER: usize = offset_of!(Transfer, frame_pointer);
    pub(crate) const OFFSET_OF_TARGET: usize = offset_of!(Transfer, target);
}
