use core::ops::Range;

use bun_engine_text::{CodeUnit, Text};
use bun_engine_unicode::IgnoreCase;

use crate::program::{Class, Direction, Program, Stride};
use crate::surrogate;

#[derive(Clone, Copy)]
pub(super) struct Subject<'a, U> {
    units: &'a [U],
    bytes: &'a [u8],
}

impl<'a, U: CodeUnit> Subject<'a, U> {
    #[inline]
    pub(super) fn new(units: &'a [U]) -> Subject<'a, U> {
        let bytes = match U::text(units) {
            Text::Latin1(characters) => characters,
            Text::Utf16(units) => bun_pod::slice_as_bytes(units),
        };
        Subject { units, bytes }
    }

    #[inline(always)]
    pub(super) fn len(self) -> usize {
        self.units.len()
    }

    #[inline(always)]
    pub(super) fn room(self, position: usize, direction: Direction) -> usize {
        match direction {
            Direction::Forward => self.len().wrapping_sub(position),
            Direction::Backward => position,
        }
    }

    #[inline(always)]
    pub(super) fn unit(self, at: usize) -> Option<u16> {
        self.units.get(at).map(|unit| unit.to_u16())
    }

    #[inline(always)]
    pub(super) fn word(self, at: usize, bytes: u8) -> Option<u64> {
        let rest = self.bytes.get(at.wrapping_mul(size_of::<U>())..)?;
        Some(match bytes {
            2 => u16::from_le_bytes(*rest.first_chunk()?).into(),
            4 => u32::from_le_bytes(*rest.first_chunk()?).into(),
            _ => u64::from_le_bytes(*rest.first_chunk()?),
        })
    }

    #[inline(always)]
    pub(super) fn is_in(self, at: usize, class: &Class) -> bool {
        self.unit(at)
            .is_some_and(|unit| class.contains(unit.into()))
    }

    #[inline]
    pub(super) fn pair(self, at: usize) -> Option<u32> {
        surrogate::join(self.unit(at)?, self.unit(at.wrapping_add(1))?)
    }

    #[inline]
    pub(super) fn is_in_pair(self, position: usize) -> bool {
        self.pair(position.wrapping_sub(1)).is_some()
    }

    /// The next character in the direction, and the position after it.
    #[inline]
    fn code_point(self, position: usize, direction: Direction) -> Option<(u32, usize)> {
        let (single, double) = match direction {
            Direction::Forward => (position, position),
            Direction::Backward => (position.wrapping_sub(1), position.wrapping_sub(2)),
        };
        let unit = self.unit(single)?;
        let pair = surrogate::is_surrogate(unit).then(|| self.pair(double));
        let (c, length) = match pair.flatten() {
            Some(c) => (c, 2),
            None => (unit.into(), 1),
        };
        let step = direction.signed(length) as isize;
        Some((c, position.wrapping_add_signed(step)))
    }

    #[inline]
    pub(super) fn run(
        self,
        position: usize,
        direction: Direction,
        class: &Class,
        max: u32,
        reserve: u32,
    ) -> usize {
        let room = self.room(position, direction);
        let length = room.saturating_sub(reserve as usize).min(max as usize);
        let is_member = |unit: &&U| class.contains(unit.to_u32());
        match direction {
            Direction::Forward => {
                let units = self.units.get(position..position + length);
                position
                    + units
                        .unwrap_or_default()
                        .iter()
                        .take_while(is_member)
                        .count()
            }
            Direction::Backward => {
                let units = self.units.get(position - length..position);
                let backward = units.unwrap_or_default().iter().rev();
                position - backward.take_while(is_member).count()
            }
        }
    }

    pub(super) fn run_code_points(
        self,
        mut position: usize,
        direction: Direction,
        class: &Class,
        max: u32,
        reserve: u32,
    ) -> usize {
        for _ in 0..max {
            match self.code_point(position, direction) {
                Some((c, after))
                    if class.contains(c) && self.room(after, direction) >= reserve as usize =>
                {
                    position = after;
                }
                _ => break,
            }
        }
        position
    }

    #[inline]
    pub(super) fn give_back(
        self,
        position: &mut usize,
        direction: Direction,
        stride: Stride,
        floor: usize,
        filter: Option<&Class>,
    ) -> bool {
        let units = stride.least_units() as usize;
        loop {
            let next_unit = match direction {
                Direction::Forward if *position >= floor + units => {
                    *position -= units;
                    if stride.is_code_point() && *position > floor && self.is_in_pair(*position) {
                        *position -= 1;
                    }
                    *position
                }
                Direction::Backward if *position + units <= floor => {
                    *position += units;
                    if stride.is_code_point() && *position < floor && self.is_in_pair(*position) {
                        *position += 1;
                    }
                    *position - 1
                }
                _ => return false,
            };
            if filter.is_none_or(|class| self.is_in(next_unit, class)) {
                return true;
            }
        }
    }

    /// The position after a second occurrence of the captured text.
    pub(super) fn repeats(
        self,
        captured: Range<usize>,
        position: usize,
        direction: Direction,
        ignore_case: bool,
        program: &Program,
    ) -> Option<usize> {
        if ignore_case && program.reads_pairs() {
            return self.repeats_folded(captured, position, direction);
        }
        let original = self.units.get(captured)?;
        let (candidate, end) = match direction {
            Direction::Forward => {
                let end = position + original.len();
                (self.units.get(position..end)?, end)
            }
            Direction::Backward => {
                let end = position.checked_sub(original.len())?;
                (self.units.get(end..position)?, end)
            }
        };
        let canonical = |unit: &U| program.case_equivalence().canonicalize(unit.to_u32());
        let is_equal = original == candidate
            || ignore_case
                && original
                    .iter()
                    .map(canonical)
                    .eq(candidate.iter().map(canonical));
        (is_equal && !(program.reads_pairs() && self.is_in_pair(end))).then_some(end)
    }

    /// Code points that are equal can differ in length.
    fn repeats_folded(
        self,
        captured: Range<usize>,
        mut position: usize,
        direction: Direction,
    ) -> Option<usize> {
        let original = Subject::new(self.units.get(captured)?);
        let mut compared = match direction {
            Direction::Forward => 0,
            Direction::Backward => original.len(),
        };
        while let Some((expected, next)) = original.code_point(compared, direction) {
            let (c, after) = self.code_point(position, direction)?;
            let canonical = |c| IgnoreCase::CodePoints.canonicalize(c);
            if c != expected && canonical(c) != canonical(expected) {
                return None;
            }
            (compared, position) = (next, after);
        }
        Some(position)
    }
}
