use bun_engine_text::CodeUnit;

use crate::set::Set;

/// Searches for each width.
pub(super) trait Unit: CodeUnit {
    fn select<'a>(latin1: Option<&'a [u8]>, utf16: &'a [u16]) -> Option<&'a [Self]>;
    fn find_unit(units: &[Self], unit: u16) -> Option<usize>;
    fn find_either(units: &[Self], a: u8, b: u8) -> Option<usize>;
    fn find_units(units: &[Self], needle: &[Self]) -> Option<usize>;
    fn count(units: &[Self], unit: u16) -> usize;
}

impl Unit for u8 {
    #[inline(always)]
    fn select<'a>(latin1: Option<&'a [u8]>, _: &'a [u16]) -> Option<&'a [u8]> {
        latin1
    }

    #[inline(always)]
    fn find_unit(units: &[u8], unit: u16) -> Option<usize> {
        bun_highway::index_of_char(units, u8::try_from(unit).ok()?)
    }

    #[inline(always)]
    fn find_either(units: &[u8], a: u8, b: u8) -> Option<usize> {
        bun_highway::index_of_any_char(units, &[a, b])
    }

    #[inline(always)]
    fn find_units(units: &[u8], needle: &[u8]) -> Option<usize> {
        bun_highway::memmem(units, needle)
    }

    fn count(units: &[u8], unit: u16) -> usize {
        u8::try_from(unit).map_or(0, |unit| bun_highway::count_char(units, unit))
    }
}

impl Unit for u16 {
    #[inline(always)]
    fn select<'a>(_: Option<&'a [u8]>, utf16: &'a [u16]) -> Option<&'a [u16]> {
        Some(utf16)
    }

    #[inline(always)]
    fn find_unit(units: &[u16], unit: u16) -> Option<usize> {
        position(units, |it| it == unit)
    }

    #[inline(always)]
    fn find_either(units: &[u16], a: u8, b: u8) -> Option<usize> {
        position(units, |it| it.is(a) | it.is(b))
    }

    #[inline(always)]
    fn find_units(units: &[u16], needle: &[u16]) -> Option<usize> {
        bun_highway::memmem16(units, needle)
    }

    fn count(units: &[u16], unit: u16) -> usize {
        let in_chunk = |chunk: &[u16]| chunk.iter().map(|&it| u16::from(it == unit)).sum::<u16>();
        let chunks = units.chunks(u16::MAX.into());
        chunks.map(|chunk| usize::from(in_chunk(chunk))).sum()
    }
}

/// Tests a block at a time without a branch, which compiles to vector instructions.
#[inline(always)]
fn position(units: &[u16], test: impl Fn(u16) -> bool) -> Option<usize> {
    const BLOCK: usize = 16;
    let has_none = |block: &&[u16; BLOCK]| !block.iter().fold(false, |any, &it| any | test(it));
    let skipped = units.as_chunks().0.iter().take_while(has_none).count() * BLOCK;
    let found = units[skipped..].iter().position(|&unit| test(unit));
    found.map(|at| skipped + at)
}

/// A set of code units.
pub(super) struct Members {
    set: Set,
    latin1: Box<[bool; 256]>,
    /// The first members, and the number of those in Latin-1.
    listed: ([u8; 2], u32),
    has_wide: bool,
}

impl Members {
    pub(super) fn new(set: Set) -> Members {
        let mut latin1 = Box::new([false; 256]);
        let mut first = [0; 2];
        for (index, c) in set.latin1().iter().enumerate() {
            latin1[usize::from(c)] = true;
            if let Some(slot) = first.get_mut(index) {
                *slot = c;
            }
        }
        Members {
            latin1,
            listed: (first, set.latin1().len()),
            has_wide: !set.above().is_empty(),
            set,
        }
    }

    #[inline(always)]
    fn contains<C: Unit>(&self, unit: C) -> bool {
        match self.latin1.get(usize::from(unit.to_u16())) {
            Some(&is_member) => is_member,
            None => self.set.contains(unit.to_u32()),
        }
    }

    #[inline(always)]
    pub(super) fn find<C: Unit>(&self, units: &[C]) -> Option<usize> {
        let has_only_latin1 = C::IS_LATIN1 || !self.has_wide;
        match self.listed {
            ([only, _], 1) if has_only_latin1 => C::find_unit(units, only.into()),
            ([a, b], 2) if has_only_latin1 => C::find_either(units, a, b),
            _ => units.iter().position(|&unit| self.contains(unit)),
        }
    }

    #[inline(always)]
    pub(super) fn leading<C: Unit>(&self, units: &[C]) -> usize {
        units.iter().take_while(|&&it| self.contains(it)).count()
    }

    #[inline(always)]
    pub(super) fn trailing<C: Unit>(&self, units: &[C]) -> usize {
        let backwards = units.iter().rev();
        backwards.take_while(|&&it| self.contains(it)).count()
    }
}
