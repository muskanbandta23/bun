icu_functions! {
    fn u_getVersion(version: &mut [u8; 4]);
    fn u_getUnicodeVersion(version: &mut [u8; 4]);
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Version {
    pub major: u8,
    pub minor: u8,
}

fn version(function: unsafe extern "C" fn(&mut [u8; 4])) -> Version {
    let mut fields = [0; 4];
    // SAFETY: it writes a `UVersionInfo`, which is four bytes.
    unsafe { function(&mut fields) };
    Version {
        major: fields[0],
        minor: fields[1],
    }
}

/// Of the library that is running, which on macOS is the system's.
pub fn library_version() -> Version {
    version(u_getVersion)
}

pub fn unicode_version() -> Version {
    version(u_getUnicodeVersion)
}
