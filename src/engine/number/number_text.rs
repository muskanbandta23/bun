use core::fmt;
use core::ops::Deref;

use bun_engine_text::Text;

use crate::{write_f64, write_i32, write_i64, write_u32, write_u64};

/// The room that the `write_*` functions need. They store whole words, so this is more than the longest text.
pub const NUMBER_TEXT_CAPACITY: usize = 32;

/// The ASCII text of a number. `from` is Number::toString of ECMA-262 in radix 10.
#[derive(Clone, Copy)]
pub struct NumberText<const CAPACITY: usize = NUMBER_TEXT_CAPACITY> {
    bytes: [u8; CAPACITY],
    start: u32,
    end: u32,
}

impl<const CAPACITY: usize> NumberText<CAPACITY> {
    const EMPTY: Self = NumberText {
        bytes: [0; CAPACITY],
        start: 0,
        end: 0,
    };

    /// `write` fills the start of the buffer and returns where the text ends.
    #[inline(always)]
    pub(crate) fn written_forward(write: impl FnOnce(&mut [u8; CAPACITY]) -> usize) -> Self {
        let mut text = Self::EMPTY;
        text.end = write(&mut text.bytes) as u32;
        text
    }

    /// `write` fills the end of the buffer and returns where the text starts.
    #[inline(always)]
    pub(crate) fn written_backward(write: impl FnOnce(&mut [u8; CAPACITY]) -> usize) -> Self {
        let mut text = Self::EMPTY;
        text.start = write(&mut text.bytes) as u32;
        text.end = CAPACITY as u32;
        text
    }

    #[inline]
    pub fn text(&self) -> Text<'_> {
        Text::Latin1(self)
    }
}

impl<const CAPACITY: usize> Deref for NumberText<CAPACITY> {
    type Target = [u8];

    #[inline]
    fn deref(&self) -> &[u8] {
        &self.bytes[self.start as usize..self.end as usize]
    }
}

impl<const CAPACITY: usize> fmt::Display for NumberText<CAPACITY> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.text().fmt(formatter)
    }
}

impl<const CAPACITY: usize> fmt::Debug for NumberText<CAPACITY> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.text().fmt(formatter)
    }
}

macro_rules! from_numbers {
    ($($number:ty => $write:ident,)*) => {$(
        impl From<$number> for NumberText {
            #[inline]
            fn from(number: $number) -> NumberText {
                NumberText::written_forward(|out| $write(number, out))
            }
        }
    )*};
}

from_numbers! {
    f64 => write_f64,
    i32 => write_i32,
    u32 => write_u32,
    i64 => write_i64,
    u64 => write_u64,
}
