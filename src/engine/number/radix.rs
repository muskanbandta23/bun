use crate::FormatError;

/// A radix from 2 to 36.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Radix(u8);

impl Radix {
    pub const BINARY: Radix = Radix(2);
    pub const OCTAL: Radix = Radix(8);
    pub const DECIMAL: Radix = Radix(10);
    pub const HEXADECIMAL: Radix = Radix(16);

    #[inline]
    pub const fn new(radix: i32) -> Result<Radix, FormatError> {
        match radix {
            2..=36 => Ok(Radix(radix as u8)),
            _ => Err(FormatError::Radix),
        }
    }

    #[inline]
    pub const fn get(self) -> u32 {
        self.0 as u32
    }

    /// The largest power that fits in 32 bits, and its exponent.
    pub(crate) const fn largest_power(self) -> (u32, usize) {
        let exponent = u32::MAX.ilog(self.get());
        (self.get().pow(exponent), exponent as usize)
    }
}
