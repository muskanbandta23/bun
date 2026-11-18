use core::ops::Deref;

/// Short text that needs no allocation.
#[derive(Clone, Copy)]
pub struct AsciiBuffer<const N: usize> {
    bytes: [u8; N],
    length: u8,
}

impl<const N: usize> AsciiBuffer<N> {
    #[inline]
    pub(crate) const fn new() -> Self {
        AsciiBuffer {
            bytes: [0; N],
            length: 0,
        }
    }

    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.length)]
    }

    #[inline]
    pub(crate) fn push<const M: usize>(&mut self, ascii: &[u8; M]) {
        let start = usize::from(self.length);
        self.bytes[start..start + M].copy_from_slice(ascii);
        self.length += M as u8;
    }

    /// The sign, the hours and the minutes of an offset of less than a hundred hours.
    #[inline]
    pub(crate) fn push_offset<const M: usize>(&mut self, minutes: i16, separator: &[u8; M]) {
        self.push(if minutes < 0 { b"-" } else { b"+" });
        let minutes = u32::from(minutes.unsigned_abs());
        self.push(&two_digits(minutes / 60));
        self.push(separator);
        self.push(&two_digits(minutes % 60));
    }
}

impl<const N: usize> Deref for AsciiBuffer<N> {
    type Target = str;

    #[inline]
    fn deref(&self) -> &str {
        str::from_utf8(self.as_bytes()).unwrap_or_default()
    }
}

/// The last two of them.
#[inline]
pub(crate) const fn two_digits(value: u32) -> [u8; 2] {
    [b'0' + (value / 10 % 10) as u8, b'0' + (value % 10) as u8]
}
