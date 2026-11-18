//! The code that leaves optimized code. It is jumped to, not called, and returns to no one.

use core::arch::naked_asm;

use super::machine::{Saved, Transfer};

macro_rules! save_gprs {
    () => {
        concat!(
            "stp x0, x1, [sp, #0x00]\n",
            "stp x2, x3, [sp, #0x10]\n",
            "stp x4, x5, [sp, #0x20]\n",
            "stp x6, x7, [sp, #0x30]\n",
            "stp x8, x9, [sp, #0x40]\n",
            "stp x10, x11, [sp, #0x50]\n",
            "stp x12, x13, [sp, #0x60]\n",
            "stp x14, x15, [sp, #0x70]\n",
            "stp x16, x17, [sp, #0x80]\n",
            "stp x18, x19, [sp, #0x90]\n",
            "stp x20, x21, [sp, #0xa0]\n",
            "stp x22, x23, [sp, #0xb0]\n",
            "stp x24, x25, [sp, #0xc0]\n",
            "stp x26, x27, [sp, #0xd0]\n",
            "stp x28, x29, [sp, #0xe0]\n",
            "stp x30, xzr, [sp, #0xf0]\n",
        )
    };
}

macro_rules! save_fprs {
    () => {
        concat!(
            "stp d0, d1, [sp, #0x100]\n",
            "stp d2, d3, [sp, #0x110]\n",
            "stp d4, d5, [sp, #0x120]\n",
            "stp d6, d7, [sp, #0x130]\n",
            "stp d8, d9, [sp, #0x140]\n",
            "stp d10, d11, [sp, #0x150]\n",
            "stp d12, d13, [sp, #0x160]\n",
            "stp d14, d15, [sp, #0x170]\n",
            "stp d16, d17, [sp, #0x180]\n",
            "stp d18, d19, [sp, #0x190]\n",
            "stp d20, d21, [sp, #0x1a0]\n",
            "stp d22, d23, [sp, #0x1b0]\n",
            "stp d24, d25, [sp, #0x1c0]\n",
            "stp d26, d27, [sp, #0x1d0]\n",
            "stp d28, d29, [sp, #0x1e0]\n",
            "stp d30, d31, [sp, #0x1f0]\n",
        )
    };
}

const _: () = assert!(Saved::OFFSET_OF_FPRS == 0x100 && Saved::SIZE == 0x200);

/// Writes the frames of the tier below where the optimized frame was and goes on there. `x17` has the index of the
/// exit, `x29` the frame pointer and `x21` the `Vm`.
#[unsafe(naked)]
pub(super) unsafe extern "C" fn exit() {
    naked_asm!(
        "sub sp, sp, {saved}",
        save_gprs!(),
        save_fprs!(),
        "mov x0, x21",
        "mov x1, sp",
        "mov x2, x29",
        "bl {leave}",
        "ldp x3, x4, [x0, {source}]",
        "ldp x5, x6, [x0, {words}]",
        "ldr x7, [x0, {target}]",
        "sub x8, x4, #16",
        "and x8, x8, #-16",
        "mov sp, x8",
        "mov x29, x6",
        "2:",
        "cbz x5, 3f",
        "sub x5, x5, #1",
        "ldr x9, [x3, x5, lsl #3]",
        "str x9, [x4, x5, lsl #3]",
        "b 2b",
        "3:",
        "br x7",
        saved = const Saved::SIZE,
        source = const Transfer::OFFSET_OF_SOURCE,
        words = const Transfer::OFFSET_OF_WORDS,
        target = const Transfer::OFFSET_OF_TARGET,
        leave = sym super::exits::leave,
    )
}

const _: () = assert!(Transfer::OFFSET_OF_DESTINATION == Transfer::OFFSET_OF_SOURCE + 8);
const _: () = assert!(Transfer::OFFSET_OF_FRAME_POINTER == Transfer::OFFSET_OF_WORDS + 8);
