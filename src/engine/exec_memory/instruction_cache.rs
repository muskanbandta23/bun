//! Makes written instructions visible to the part of the processor that fetches them.

#[cfg(target_arch = "x86_64")]
pub(crate) fn flush(_start: usize, _len: usize) {}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
pub(crate) fn flush(start: usize, len: usize) {
    unsafe extern "C" {
        safe fn sys_icache_invalidate(start: usize, len: usize);
    }
    sys_icache_invalidate(start, len);
}

#[cfg(all(target_arch = "aarch64", windows))]
pub(crate) fn flush(start: usize, len: usize) {
    const CURRENT_PROCESS: isize = -1;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        safe fn FlushInstructionCache(process: isize, start: usize, len: usize) -> i32;
    }
    FlushInstructionCache(CURRENT_PROCESS, start, len);
}

#[cfg(all(target_arch = "aarch64", not(any(target_os = "macos", windows))))]
pub(crate) fn flush(start: usize, len: usize) {
    use core::arch::asm;

    let cache_type: u64;
    // SAFETY: the register is readable in user mode on these systems.
    unsafe {
        asm!("mrs {}, ctr_el0", out(reg) cache_type, options(nomem, nostack, preserves_flags))
    };
    let lines = |log2_words: u64| {
        let size = 4 << (log2_words & 15);
        (start & !(size - 1)..start + len).step_by(size)
    };
    let data_is_coherent = cache_type >> 28 & 1 != 0;
    let instructions_are_coherent = cache_type >> 29 & 1 != 0;
    if !data_is_coherent {
        for line in lines(cache_type >> 16) {
            // SAFETY: the pages of the range are mapped, and their contents do not change.
            unsafe { asm!("dc cvau, {}", in(reg) line, options(nostack, preserves_flags)) };
        }
    }
    // SAFETY: a barrier.
    unsafe { asm!("dsb ish", options(nostack, preserves_flags)) };
    if !instructions_are_coherent {
        for line in lines(cache_type) {
            // SAFETY: as above.
            unsafe { asm!("ic ivau, {}", in(reg) line, options(nostack, preserves_flags)) };
        }
        // SAFETY: a barrier.
        unsafe { asm!("dsb ish", options(nostack, preserves_flags)) };
    }
    synchronize_instructions();
}

/// For a thread that is about to run code which another thread wrote.
#[inline]
pub fn synchronize_instructions() {
    #[cfg(target_arch = "aarch64")]
    // SAFETY: a barrier.
    unsafe {
        core::arch::asm!("isb", options(nostack, preserves_flags));
    }
}
