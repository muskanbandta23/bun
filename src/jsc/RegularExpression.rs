use bun_collections::smallvec::SmallVec;
use bun_core::{String as BunString, strings};
use bun_yarr::{Compiled, Text};

/// A JavaScript regular expression that needs no VM. Threads can share it.
pub struct RegularExpression(Compiled);

#[repr(u16)]
#[derive(Copy, Clone, Eq, PartialEq)]
pub enum Flags {
    None = 0,
}

#[derive(thiserror::Error, strum::IntoStaticStr, Debug)]
pub enum RegularExpressionError {
    #[error("InvalidRegExp")]
    InvalidRegExp,
}

fn with_text<R>(string: &BunString, use_text: impl FnOnce(Text<'_>) -> R) -> R {
    if string.is_utf8() && !strings::is_all_ascii(string.byte_slice()) {
        return with_text(&BunString::clone_utf8(string.byte_slice()), use_text);
    }
    if string.is_utf16() {
        use_text(Text::Utf16(string.utf16()))
    } else {
        use_text(Text::Latin1(string.byte_slice()))
    }
}

impl RegularExpression {
    pub fn init(
        pattern: &BunString,
        flags: Flags,
    ) -> Result<Box<RegularExpression>, RegularExpressionError> {
        Self::compile(pattern, flags as u16).ok_or(RegularExpressionError::InvalidRegExp)
    }

    fn compile(pattern: &BunString, flags: u16) -> Option<Box<RegularExpression>> {
        let compiled = with_text(pattern, |pattern| Compiled::new(pattern, flags)).ok()?;
        Some(Box::new(RegularExpression(compiled)))
    }

    /// The empty string never matches.
    pub fn matches(&self, input: &BunString) -> bool {
        let mut offsets = SmallVec::<[u32; 32]>::from_elem(0, self.0.offsets_len());
        !input.is_empty() && with_text(input, |input| self.0.exec(input, 0, &mut offsets))
    }
}

/// `flags`: of `JSC::Yarr::Flags`. Null if the pattern is not valid.
#[unsafe(no_mangle)]
extern "C" fn Bun__RegularExpression__create(
    pattern: &BunString,
    flags: u16,
) -> Option<Box<RegularExpression>> {
    RegularExpression::compile(pattern, flags)
}

#[unsafe(no_mangle)]
extern "C" fn Bun__RegularExpression__matches(this: &RegularExpression, input: &BunString) -> bool {
    this.matches(input)
}

#[unsafe(no_mangle)]
extern "C" fn Bun__RegularExpression__destroy(this: Box<RegularExpression>) {
    drop(this);
}

// ──────────────────────────────────────────────────────────────────────────
// `bun_install_types::NodeLinker` / `bun_install::PnpmMatcher` extern impls.
//
// Those lower-tier crates cannot name `jsc::RegularExpression`.
// The bodies live here as `#[no_mangle]` Rust-ABI
// fns, declared `extern "Rust"` on the low-tier side; link-time resolved.
// ──────────────────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
fn __bun_regex_compile(pattern: &BunString) -> Option<core::ptr::NonNull<()>> {
    let regex = RegularExpression::init(pattern, Flags::None).ok()?;
    Some(core::ptr::NonNull::from(Box::leak(regex)).cast())
}

#[unsafe(no_mangle)]
unsafe fn __bun_regex_matches(regex: core::ptr::NonNull<()>, input: &BunString) -> bool {
    // SAFETY: produced by `__bun_regex_compile`, and live until `__bun_regex_drop`.
    unsafe { regex.cast::<RegularExpression>().as_ref() }.matches(input)
}

#[unsafe(no_mangle)]
unsafe fn __bun_regex_drop(regex: core::ptr::NonNull<()>) {
    // SAFETY: produced by `__bun_regex_compile`; consumed here.
    drop(unsafe { Box::from_raw(regex.cast::<RegularExpression>().as_ptr()) });
}
