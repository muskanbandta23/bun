use smallvec::SmallVec;

use crate::Text;

impl Text<'_> {
    /// The first index of `unit` at or after `start`.
    #[inline]
    pub fn find_unit(self, unit: u16, start: usize) -> Option<usize> {
        let found = match self {
            Text::Latin1(characters) => {
                bun_highway::index_of_char(characters.get(start..)?, u8::try_from(unit).ok()?)
            }
            Text::Utf16(units) => bun_highway::memmem16(units.get(start..)?, &[unit]),
        };
        found.map(|index| start + index)
    }

    /// The last index of `unit` before `end`.
    pub fn rfind_unit(self, unit: u16, end: usize) -> Option<usize> {
        match self.slice(0..end.min(self.len())) {
            Text::Latin1(characters) => {
                bun_highway::last_index_of_char(characters, u8::try_from(unit).ok()?)
            }
            Text::Utf16(units) => bun_highway::memrmem16(units, &[unit]),
        }
    }

    /// StringIndexOf of ECMA-262.
    pub fn find(self, needle: Text<'_>, start: usize) -> Option<usize> {
        if start > self.len() || needle.len() > self.len() - start {
            return None;
        }
        if needle.len() == 1 {
            return self.find_unit(needle.at(0), start);
        }
        search(
            self.slice_from(start),
            needle,
            bun_highway::memmem,
            bun_highway::memmem16,
        )
        .map(|index| start + index)
    }

    /// The last index of `needle` at or before `start`.
    pub fn rfind(self, needle: Text<'_>, start: usize) -> Option<usize> {
        let end = start.saturating_add(needle.len()).min(self.len());
        search(
            self.slice(0..end),
            needle,
            bun_highway::memrmem,
            bun_highway::memrmem16,
        )
    }

    #[inline]
    pub fn contains_unit(self, unit: u16) -> bool {
        self.find_unit(unit, 0).is_some()
    }

    #[inline]
    pub fn contains(self, needle: Text<'_>) -> bool {
        self.find(needle, 0).is_some()
    }
}

/// Brings the needle to the width of the haystack.
#[inline(always)]
fn search(
    haystack: Text<'_>,
    needle: Text<'_>,
    narrow: impl Fn(&[u8], &[u8]) -> Option<usize>,
    wide: impl Fn(&[u16], &[u16]) -> Option<usize>,
) -> Option<usize> {
    match (haystack, needle) {
        (Text::Latin1(haystack), Text::Latin1(needle)) => narrow(haystack, needle),
        (Text::Utf16(haystack), Text::Utf16(needle)) => wide(haystack, needle),
        _ if needle.len() > haystack.len() => None,
        (Text::Latin1(haystack), Text::Utf16(needle)) => {
            let needle: Option<SmallVec<[u8; 64]>> =
                needle.iter().map(|&unit| u8::try_from(unit).ok()).collect();
            narrow(haystack, &needle?)
        }
        (Text::Utf16(haystack), Text::Latin1(needle)) => {
            let needle: SmallVec<[u16; 32]> = needle.iter().map(|&unit| u16::from(unit)).collect();
            wide(haystack, &needle)
        }
    }
}
