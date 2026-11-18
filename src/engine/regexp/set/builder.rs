use bun_engine_text::AllocError;
use bun_engine_unicode::IgnoreCase;

use super::{Above, CharRange, Latin1Set, Set, SharedSet, ranges};

/// Collects the members of a set in any order. `finish` leaves it empty, with its memory, for the next set.
#[derive(Default)]
pub struct SetBuilder {
    latin1: Latin1Set,
    /// Not yet closed over case.
    open: Vec<CharRange>,
    closed: Vec<CharRange>,
    shared: Option<&'static SharedSet>,
}

/// One of each group of equal characters that has members on both sides of U+00FF.
const fn equal_to_wide_characters(ignore_case: IgnoreCase) -> Latin1Set {
    match ignore_case {
        IgnoreCase::CodeUnits => const { Latin1Set::of(b"\xb5\xff") },
        IgnoreCase::CodePoints => const { Latin1Set::of(b"ks\xb5\xdf\xe5\xff") },
    }
}

impl SetBuilder {
    #[inline]
    pub fn add(&mut self, c: u32) -> Result<(), AllocError> {
        match u8::try_from(c) {
            Ok(c) => {
                self.latin1.insert(c);
                Ok(())
            }
            Err(_) => push(&mut self.open, CharRange::single(c)),
        }
    }

    #[inline]
    pub fn add_range(&mut self, first: u32, last: u32) -> Result<(), AllocError> {
        add_range(&mut self.latin1, &mut self.open, first, last)
    }

    /// If the result is to be closed over case, `set` is so already.
    pub fn add_set(&mut self, set: &Set) -> Result<(), AllocError> {
        self.latin1 |= set.latin1;
        match (set.shared(), self.shared) {
            (Some(shared), None) => self.shared = Some(shared),
            (Some(shared), Some(held)) if core::ptr::eq(shared, held) => {}
            _ => {
                let above = set.above();
                self.closed
                    .try_reserve(above.len())
                    .map_err(|_| AllocError)?;
                self.closed.extend_from_slice(above);
            }
        }
        Ok(())
    }

    /// The number of ranges that `finish` has to go through.
    #[inline]
    pub(crate) fn pending_ranges(&self) -> usize {
        self.open.len() + self.closed.len()
    }

    pub fn finish(&mut self, ignore_case: Option<IgnoreCase>) -> Result<Set, AllocError> {
        if let Some(ignore_case) = ignore_case {
            self.close_over_case(ignore_case)?;
        }
        let (latin1, shared) = (core::mem::take(&mut self.latin1), self.shared.take());
        if self.pending_ranges() == 0 {
            return Ok(match shared {
                Some(shared) => Set::with_shared(latin1, shared),
                None => Set::from_latin1(latin1),
            });
        }
        self.open
            .try_reserve(self.closed.len())
            .map_err(|_| AllocError)?;
        self.open.append(&mut self.closed);
        ranges::normalize(&mut self.open);
        let above = match shared {
            None => Above::from_slice(&self.open),
            Some(shared) if ranges::is_subset(&self.open, shared.above()) => {
                Ok(Above::from_shared(shared))
            }
            Some(shared) => ranges::union(&self.open, shared.above())
                .and_then(|union| Above::from_slice(union.as_slice())),
        };
        self.open.clear();
        Ok(Set {
            latin1,
            above: above?,
        })
    }

    fn close_over_case(&mut self, ignore_case: IgnoreCase) -> Result<(), AllocError> {
        let mut result = Ok(());
        ranges::normalize(&mut self.open);
        for range in &self.open {
            ignore_case.close_over(range.first..=range.last, |equal| {
                let (first, last) = equal.into_inner();
                result = result
                    .and_then(|()| add_range(&mut self.latin1, &mut self.closed, first, last));
            });
        }
        self.latin1 = self.latin1.with_both_cases();
        for c in (self.latin1 & equal_to_wide_characters(ignore_case)).iter() {
            for equal in ignore_case.equivalents(c.into()).filter(|&it| it > 0xff) {
                push(&mut self.closed, CharRange::single(equal))?;
            }
        }
        result
    }
}

#[inline]
fn add_range(
    latin1: &mut Latin1Set,
    above: &mut Vec<CharRange>,
    first: u32,
    last: u32,
) -> Result<(), AllocError> {
    if let Ok(first) = u8::try_from(first) {
        latin1.insert_range(first, last.min(0xff) as u8);
    }
    if last > 0xff {
        push(above, CharRange::new(first.max(0x100), last))?;
    }
    Ok(())
}

#[inline]
fn push(ranges: &mut Vec<CharRange>, range: CharRange) -> Result<(), AllocError> {
    ranges.try_reserve(1).map_err(|_| AllocError)?;
    ranges.push(range);
    Ok(())
}
