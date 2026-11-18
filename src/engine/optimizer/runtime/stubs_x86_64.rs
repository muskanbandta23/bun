//! The code that leaves optimized code. It is jumped to, not called, and returns to no one.

use core::arch::naked_asm;

use super::machine::{Saved, Transfer};

macro_rules! save_gprs {
    () => {
        concat!(
            "mov [rsp + 0x00], rax\n",
            "mov [rsp + 0x08], rcx\n",
            "mov [rsp + 0x10], rdx\n",
            "mov [rsp + 0x18], rbx\n",
            "mov [rsp + 0x20], rsp\n",
            "mov [rsp + 0x28], rbp\n",
            "mov [rsp + 0x30], rsi\n",
            "mov [rsp + 0x38], rdi\n",
            "mov [rsp + 0x40], r8\n",
            "mov [rsp + 0x48], r9\n",
            "mov [rsp + 0x50], r10\n",
            "mov [rsp + 0x58], r11\n",
            "mov [rsp + 0x60], r12\n",
            "mov [rsp + 0x68], r13\n",
            "mov [rsp + 0x70], r14\n",
            "mov [rsp + 0x78], r15\n",
        )
    };
}

macro_rules! save_fprs {
    () => {
        concat!(
            "movsd [rsp + {fprs} + 0x00], xmm0\n",
            "movsd [rsp + {fprs} + 0x08], xmm1\n",
            "movsd [rsp + {fprs} + 0x10], xmm2\n",
            "movsd [rsp + {fprs} + 0x18], xmm3\n",
            "movsd [rsp + {fprs} + 0x20], xmm4\n",
            "movsd [rsp + {fprs} + 0x28], xmm5\n",
            "movsd [rsp + {fprs} + 0x30], xmm6\n",
            "movsd [rsp + {fprs} + 0x38], xmm7\n",
            "movsd [rsp + {fprs} + 0x40], xmm8\n",
            "movsd [rsp + {fprs} + 0x48], xmm9\n",
            "movsd [rsp + {fprs} + 0x50], xmm10\n",
            "movsd [rsp + {fprs} + 0x58], xmm11\n",
            "movsd [rsp + {fprs} + 0x60], xmm12\n",
            "movsd [rsp + {fprs} + 0x68], xmm13\n",
            "movsd [rsp + {fprs} + 0x70], xmm14\n",
            "movsd [rsp + {fprs} + 0x78], xmm15\n",
        )
    };
}

/// Writes the frames of the tier below where the optimized frame was and goes on there. `r11` has the index of the
/// exit, `rbp` the frame pointer and `r13` the `Vm`.
#[bun_engine_macros::in_call_frame(naked)]
#[unsafe(naked)]
pub(super) unsafe extern "sysv64" fn exit() {
    naked_asm!(
        "sub rsp, {saved}",
        save_gprs!(),
        save_fprs!(),
        "mov rdi, r13",
        "mov rsi, rsp",
        "mov rdx, rbp",
        "call {leave}",
        "mov r8, [rax + {source}]",
        "mov r9, [rax + {destination}]",
        "mov r10, [rax + {words}]",
        "mov rbp, [rax + {frame_pointer}]",
        "mov r11, [rax + {target}]",
        "lea rsp, [r9 - 16]",
        "and rsp, -16",
        "2:",
        "sub r10, 1",
        "jb 3f",
        "mov rax, [r8 + r10 * 8]",
        "mov [r9 + r10 * 8], rax",
        "jmp 2b",
        "3:",
        "jmp r11",
        saved = const Saved::SIZE,
        fprs = const Saved::OFFSET_OF_FPRS,
        source = const Transfer::OFFSET_OF_SOURCE,
        destination = const Transfer::OFFSET_OF_DESTINATION,
        words = const Transfer::OFFSET_OF_WORDS,
        frame_pointer = const Transfer::OFFSET_OF_FRAME_POINTER,
        target = const Transfer::OFFSET_OF_TARGET,
        leave = sym super::exits::leave,
    )
}
