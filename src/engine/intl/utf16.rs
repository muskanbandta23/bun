use core::ops::Deref;

use bun_engine_text::Text;
use smallvec::SmallVec;

use crate::Result;

/// Text in the width that ICU reads.
#[expect(clippy::large_enum_variant, reason = "a buffer on the stack")]
pub(crate) enum Utf16<'a> {
    Borrowed(&'a [u16]),
    Widened(SmallVec<[u16; 128]>),
}

impl<'a> Utf16<'a> {
    pub(crate) fn new(text: Text<'a>) -> Result<Utf16<'a>> {
        match text {
            Text::Utf16(units) => Ok(Utf16::Borrowed(units)),
            Text::Latin1(characters) => {
                let mut units = SmallVec::new();
                units.try_reserve_exact(characters.len())?;
                units.extend(characters.iter().map(|&character| u16::from(character)));
                Ok(Utf16::Widened(units))
            }
        }
    }
}

impl Deref for Utf16<'_> {
    type Target = [u16];

    fn deref(&self) -> &[u16] {
        match self {
            Utf16::Borrowed(units) => units,
            Utf16::Widened(units) => units,
        }
    }
}
