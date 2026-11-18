use bun_engine_text::Text;
use smallvec::SmallVec;

use crate::Result;

/// Room for text in the width that ICU reads.
#[derive(Default)]
pub(crate) struct Utf16Buffer(SmallVec<[u16; 128]>);

impl Utf16Buffer {
    #[inline]
    pub(crate) fn widen<'a>(&'a mut self, text: Text<'a>) -> Result<&'a [u16]> {
        match text {
            Text::Utf16(units) => Ok(units),
            Text::Latin1(characters) => {
                self.0.try_reserve_exact(characters.len())?;
                self.0.resize(characters.len(), 0);
                for (unit, &character) in self.0.iter_mut().zip(characters) {
                    *unit = character.into();
                }
                Ok(&self.0)
            }
        }
    }
}
