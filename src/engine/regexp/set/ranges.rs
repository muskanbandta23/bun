//! Operations on lists of ranges that ascend with a gap between any two.

use bun_engine_text::AllocError;
use bun_engine_unicode::MAX_CODE_POINT;
use smallvec::SmallVec;

use super::CharRange;

#[derive(Default)]
pub(super) struct RangeList(SmallVec<[CharRange; 8]>);

impl RangeList {
    #[inline]
    pub(super) fn as_slice(&self) -> &[CharRange] {
        &self.0
    }

    /// `range` does not start before the last one does.
    #[inline]
    fn push(&mut self, range: CharRange) -> Result<(), AllocError> {
        match self.0.last_mut() {
            Some(last) if range.first <= last.last + 1 => last.last = last.last.max(range.last),
            _ => {
                self.0.try_reserve(1).map_err(|_| AllocError)?;
                self.0.push(range);
            }
        }
        Ok(())
    }
}

pub(super) fn contains(ranges: &[CharRange], c: u32) -> bool {
    let candidate = ranges.partition_point(|range| range.last < c);
    ranges.get(candidate).is_some_and(|range| range.first <= c)
}

pub(super) fn intersects(a: &[CharRange], b: &[CharRange]) -> bool {
    let (few, mut many) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    few.iter().any(|range| {
        many = &many[many.partition_point(|other| other.last < range.first)..];
        many.first().is_some_and(|other| other.first <= range.last)
    })
}

pub(super) fn is_subset(part: &[CharRange], mut whole: &[CharRange]) -> bool {
    part.iter().all(|range| {
        whole = &whole[whole.partition_point(|other| other.last < range.first)..];
        whole
            .first()
            .is_some_and(|other| other.first <= range.first && range.last <= other.last)
    })
}

pub(super) fn union<'a>(
    mut a: &'a [CharRange],
    mut b: &'a [CharRange],
) -> Result<RangeList, AllocError> {
    let mut result = RangeList::default();
    loop {
        let source = match (a.first(), b.first()) {
            (Some(x), Some(y)) if y.first < x.first => &mut b,
            (None, _) => &mut b,
            _ => &mut a,
        };
        match source.split_off_first() {
            Some(&range) => result.push(range)?,
            None => return Ok(result),
        }
    }
}

pub(super) fn intersection(
    mut a: &[CharRange],
    mut b: &[CharRange],
) -> Result<RangeList, AllocError> {
    let mut result = RangeList::default();
    while let (Some(x), Some(y)) = (a.first(), b.first()) {
        let (first, last) = (x.first.max(y.first), x.last.min(y.last));
        if first <= last {
            result.push(CharRange::new(first, last))?;
        }
        if x.last < y.last {
            a = &a[1..];
        } else {
            b = &b[1..];
        }
    }
    Ok(result)
}

pub(super) fn difference(a: &[CharRange], mut b: &[CharRange]) -> Result<RangeList, AllocError> {
    let mut result = RangeList::default();
    for range in a {
        let mut next = range.first;
        while let Some(hole) = b.first().filter(|hole| hole.first <= range.last) {
            if hole.first > next {
                result.push(CharRange::new(next, hole.first - 1))?;
            }
            next = next.max(hole.last + 1);
            if hole.last > range.last {
                break;
            }
            b = &b[1..];
        }
        if next <= range.last {
            result.push(CharRange::new(next, range.last))?;
        }
    }
    Ok(result)
}

/// Among the characters above U+00FF.
pub(super) fn complement(ranges: &[CharRange]) -> Result<RangeList, AllocError> {
    difference(&[CharRange::new(0x100, MAX_CODE_POINT)], ranges)
}

/// Brings ranges that overlap or come in any order into the form that the other operations take.
pub(super) fn normalize(ranges: &mut Vec<CharRange>) {
    if !ranges.is_sorted_by_key(|range| range.first) {
        ranges.sort_unstable_by_key(|range| range.first);
    }
    ranges.dedup_by(|range, kept| {
        let touches = range.first <= kept.last + 1;
        if touches {
            kept.last = kept.last.max(range.last);
        }
        touches
    });
}
