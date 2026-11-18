use bun_engine_text::{CodeUnit, scan};

pub(super) trait Encoding {
    type Unit: CodeUnit;
    const IS_UTF8: bool = false;

    /// A byte for each unit, which is ASCII and no control character only if the unit is that character.
    fn bytes<const N: usize>(units: &[Self::Unit; N]) -> [u8; N];

    /// Whether what `bytes` gives may be anything but a Latin-1 character that stands for itself in a string.
    fn ends_plain_run(byte: u8) -> bool;
}

pub(super) struct Latin1;
pub(super) struct Utf16;
pub(super) struct Utf8;

impl Encoding for Latin1 {
    type Unit = u8;

    #[inline(always)]
    fn bytes<const N: usize>(units: &[u8; N]) -> [u8; N] {
        *units
    }

    #[inline(always)]
    fn ends_plain_run(byte: u8) -> bool {
        scan::is_json_special(byte.into())
    }
}

impl Encoding for Utf16 {
    type Unit = u16;

    #[inline(always)]
    fn bytes<const N: usize>(units: &[u16; N]) -> [u8; N] {
        units.map(|unit| (unit as i16).clamp(0, 0xff) as u8)
    }

    #[inline(always)]
    fn ends_plain_run(byte: u8) -> bool {
        scan::is_json_special(byte.into()) | (byte == 0xff)
    }
}

impl Encoding for Utf8 {
    type Unit = u8;
    const IS_UTF8: bool = true;

    #[inline(always)]
    fn bytes<const N: usize>(units: &[u8; N]) -> [u8; N] {
        *units
    }

    #[inline(always)]
    fn ends_plain_run(byte: u8) -> bool {
        ((byte as i8) < 0x20) | (byte == b'"') | (byte == b'\\')
    }
}
