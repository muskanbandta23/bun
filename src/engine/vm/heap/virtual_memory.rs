//! Address space that is reserved first and backed by memory later.

#[cfg(unix)]
mod platform {
    use core::ptr::{null_mut, with_exposed_provenance_mut};

    pub fn reserve(size: usize) -> Option<usize> {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        const NO_RESERVE: i32 = libc::MAP_NORESERVE;
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        const NO_RESERVE: i32 = 0;
        let flags = libc::MAP_PRIVATE | libc::MAP_ANON | NO_RESERVE;
        // SAFETY: a new mapping at an address that the kernel picks.
        let address = unsafe { libc::mmap(null_mut(), size, libc::PROT_NONE, flags, -1, 0) };
        if address == libc::MAP_FAILED {
            return None;
        }
        #[cfg(any(target_os = "linux", target_os = "android"))]
        // SAFETY: advice about the new mapping.
        unsafe {
            libc::madvise(address, size, libc::MADV_NOHUGEPAGE);
        }
        Some(address.expose_provenance())
    }

    pub unsafe fn commit(address: usize, size: usize) -> bool {
        let protection = libc::PROT_READ | libc::PROT_WRITE;
        // SAFETY: the caller's.
        unsafe { libc::mprotect(with_exposed_provenance_mut(address), size, protection) == 0 }
    }

    pub unsafe fn discard(address: usize, size: usize) {
        #[cfg(target_os = "macos")]
        const ADVICE: i32 = libc::MADV_FREE_REUSABLE;
        #[cfg(not(target_os = "macos"))]
        const ADVICE: i32 = libc::MADV_DONTNEED;
        // SAFETY: the caller's.
        unsafe { libc::madvise(with_exposed_provenance_mut(address), size, ADVICE) };
    }

    pub unsafe fn reuse(_address: usize, _size: usize) {
        #[cfg(target_os = "macos")]
        // SAFETY: the caller's.
        unsafe {
            libc::madvise(
                with_exposed_provenance_mut(_address),
                _size,
                libc::MADV_FREE_REUSE,
            );
        }
    }

    pub unsafe fn protect_none(address: usize, size: usize) -> bool {
        // SAFETY: the caller's.
        unsafe {
            discard(address, size);
            libc::mprotect(with_exposed_provenance_mut(address), size, libc::PROT_NONE) == 0
        }
    }

    pub unsafe fn release(address: usize, size: usize) {
        // SAFETY: the caller's.
        unsafe { libc::munmap(with_exposed_provenance_mut(address), size) };
    }
}

#[cfg(windows)]
mod platform {
    use core::ffi::c_void;
    use core::ptr::{null_mut, with_exposed_provenance_mut};

    const MEM_COMMIT: u32 = 0x1000;
    const MEM_RESERVE: u32 = 0x2000;
    const MEM_DECOMMIT: u32 = 0x4000;
    const MEM_RELEASE: u32 = 0x8000;
    const PAGE_NOACCESS: u32 = 0x01;
    const PAGE_READWRITE: u32 = 0x04;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn VirtualAlloc(address: *mut c_void, size: usize, kind: u32, protect: u32) -> *mut c_void;
        fn VirtualFree(address: *mut c_void, size: usize, kind: u32) -> i32;
    }

    pub fn reserve(size: usize) -> Option<usize> {
        // SAFETY: a new reservation at an address that the system picks.
        let address = unsafe { VirtualAlloc(null_mut(), size, MEM_RESERVE, PAGE_NOACCESS) };
        (!address.is_null()).then(|| address.expose_provenance())
    }

    pub unsafe fn commit(address: usize, size: usize) -> bool {
        let address = with_exposed_provenance_mut(address);
        // SAFETY: the caller's.
        unsafe { !VirtualAlloc(address, size, MEM_COMMIT, PAGE_READWRITE).is_null() }
    }

    pub unsafe fn discard(address: usize, size: usize) {
        // SAFETY: the caller's.
        unsafe { VirtualFree(with_exposed_provenance_mut(address), size, MEM_DECOMMIT) };
    }

    pub unsafe fn reuse(address: usize, size: usize) {
        // SAFETY: the caller's.
        if !unsafe { commit(address, size) } {
            bun_alloc::out_of_memory();
        }
    }

    pub unsafe fn protect_none(address: usize, size: usize) -> bool {
        // SAFETY: the caller's.
        unsafe { VirtualFree(with_exposed_provenance_mut(address), size, MEM_DECOMMIT) != 0 }
    }

    pub unsafe fn release(address: usize, _size: usize) {
        // SAFETY: the caller's.
        unsafe { VirtualFree(with_exposed_provenance_mut(address), 0, MEM_RELEASE) };
    }
}

/// `reserve`: an inaccessible range. The others take whole pages of such a range that nothing else uses.
/// `commit`: makes them readable and writable, filled with zeros. `discard`: gives the memory back until `reuse`,
/// after which the content is unspecified. `protect_none`: undoes `commit`. `release`: undoes `reserve`.
pub use platform::{commit, discard, protect_none, release, reserve, reuse};
