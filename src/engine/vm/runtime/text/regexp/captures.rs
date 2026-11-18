use core::ops::Range;

use bun_alloc::AllocError;
use bun_engine_regexp::program::UNSET;

use crate::{StringView, Value, Vm};

/// Where in the subject a pattern has matched.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
#[repr(C)]
pub struct Match {
    pub start: u32,
    pub end: u32,
}

impl Match {
    #[inline(always)]
    pub fn is_empty(self) -> bool {
        self.start == self.end
    }

    #[inline(always)]
    pub fn range(self) -> Range<usize> {
        self.start as usize..self.end as usize
    }
}

impl From<Range<usize>> for Match {
    #[inline(always)]
    fn from(range: Range<usize>) -> Match {
        Match {
            start: range.start as u32,
            end: range.end as u32,
        }
    }
}

const INLINE_OFFSETS: usize = 32;

/// What a search writes its `Captures` to.
#[derive(Default)]
pub struct CaptureBuffer {
    inline: [u32; INLINE_OFFSETS],
    spilled: Vec<u32>,
}

impl CaptureBuffer {
    /// Two for the match and two for each group.
    #[inline(always)]
    pub(super) fn offsets(&mut self, group_count: usize) -> Result<&mut [u32], AllocError> {
        let length = (group_count + 1) * 2;
        match self.inline.get_mut(..length) {
            Some(offsets) => Ok(offsets),
            None => Self::spill(&mut self.spilled, length),
        }
    }

    #[inline(never)]
    fn spill(spilled: &mut Vec<u32>, length: usize) -> Result<&mut [u32], AllocError> {
        let additional = length.saturating_sub(spilled.len());
        spilled
            .try_reserve_exact(additional)
            .map_err(|_| AllocError)?;
        spilled.resize(length, 0);
        Ok(spilled)
    }
}

/// The match, which is number zero, and the groups.
#[derive(Clone, Copy)]
pub struct Captures<'a>(&'a [[u32; 2]]);

impl<'a> Captures<'a> {
    /// There are two at least.
    #[inline(always)]
    pub(super) fn new(offsets: &'a [u32]) -> Captures<'a> {
        Captures(offsets.as_chunks().0)
    }

    #[inline(always)]
    pub fn whole(self) -> Match {
        let [start, end] = self.0[0];
        Match { start, end }
    }

    /// With the match.
    #[inline(always)]
    pub fn count(self) -> usize {
        self.0.len()
    }

    /// `None` if there is no such group, or it has taken no part in the match.
    #[inline(always)]
    pub fn get(self, index: usize) -> Option<Match> {
        let &[start, end] = self.0.get(index)?;
        (start != UNSET).then_some(Match { start, end })
    }

    /// The start and the end of each. Both are `u32::MAX` for a group that has taken no part.
    #[inline(always)]
    pub fn offsets(self) -> &'a [u32] {
        self.0.as_flattened()
    }

    /// A string or undefined.
    #[inline(always)]
    pub(super) fn value(self, vm: &Vm, subject: &StringView, index: usize) -> Value {
        match self.get(index) {
            Some(found) => subject.substring(vm, found.range()).into(),
            None => Value::UNDEFINED,
        }
    }
}
