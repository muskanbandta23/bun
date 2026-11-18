//! The order of `sort` without a comparator: that of the code units of the elements as strings.

use bun_engine_text::{AllocError, Text, with_text};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Entry {
    /// The next units of the text, and in the low byte how many it has left, as far as that matters.
    key: u64,
    index: u32,
}

/// Up to here, comparing whole texts is cheaper than another round of keys.
const SMALL_GROUP: usize = 6;

#[inline(always)]
fn key<const UNIT_BITS: usize>(text: Text<'_>, depth: usize) -> u64 {
    let units_in_key = 56 / UNIT_BITS;
    if let (8, Text::Latin1(characters)) = (UNIT_BITS, text)
        && let Some(&bytes) = characters[depth..].first_chunk()
    {
        return u64::from_be_bytes(bytes) & !0xff | 8;
    }
    with_text!(text.slice_from(depth), |rest| {
        let mut key = rest.len().min(units_in_key + 1) as u64;
        for (position, &unit) in (1..).zip(rest.iter().take(units_in_key)) {
            key |= u64::from(unit) << (64 - position * UNIT_BITS);
        }
        key
    })
}

fn sort_entries<'a, const UNIT_BITS: usize>(entries: &mut [Entry], text: impl Fn(u32) -> Text<'a>) {
    let units_in_key = 56 / UNIT_BITS;
    // Groups of entries whose texts are the same up to a depth.
    let mut pending = vec![(0..entries.len(), 0)];
    while let Some((range, mut depth)) = pending.pop() {
        let start = range.start;
        let group = &mut entries[range];
        if group.len() <= SMALL_GROUP {
            let rest = |entry: &Entry| (text(entry.index).slice_from(depth), entry.index);
            group.sort_unstable_by(|a, b| rest(a).cmp(&rest(b)));
            continue;
        }
        let mut shared = text(group[0].index).slice_from(depth);
        for entry in &group[1..] {
            if shared.is_empty() {
                break;
            }
            let other = text(entry.index).slice_from(depth);
            shared = shared.slice(0..shared.common_prefix_len(other));
        }
        depth += shared.len();
        for entry in &mut *group {
            entry.key = key::<UNIT_BITS>(text(entry.index), depth);
        }
        group.sort_unstable();
        let mut offset = start;
        for ties in group.chunk_by(|a, b| a.key == b.key) {
            if ties.len() > 1 && usize::from(ties[0].key as u8) > units_in_key {
                pending.push((offset..offset + ties.len(), depth + units_in_key));
            }
            offset += ties.len();
        }
    }
}

/// The indices below `count` in the order of their texts. Those of equal texts ascend.
pub(super) fn sorted_indices<'a>(
    count: u32,
    text: impl Fn(u32) -> Text<'a>,
) -> Result<impl Iterator<Item = u32>, AllocError> {
    let mut entries = Vec::new();
    (entries.try_reserve_exact(count as usize)).map_err(|_| AllocError)?;
    entries.extend((0..count).map(|index| Entry { key: 0, index }));
    if (0..count).all(|index| text(index).is_latin1()) {
        sort_entries::<8>(&mut entries, text);
    } else {
        sort_entries::<16>(&mut entries, text);
    }
    Ok(entries.into_iter().map(|entry| entry.index))
}

const MAX_DIGITS: usize = 10;
const POWERS_OF_TEN: [u64; MAX_DIGITS] = {
    let (mut powers, mut exponent) = ([1; MAX_DIGITS], 1);
    while exponent < MAX_DIGITS {
        powers[exponent] = powers[exponent - 1] * 10;
        exponent += 1;
    }
    powers
};
const IS_NOT_NEGATIVE: u64 = 1 << 63;

/// Compares as the decimal text does: the sign, the digits padded with zeros, and their number.
#[inline(always)]
pub(super) fn key_of_int32(number: i32) -> u64 {
    let magnitude = u64::from(number.unsigned_abs());
    let digits = bun_engine_number::decimal_length(magnitude);
    let sign = if number < 0 { 0 } else { IS_NOT_NEGATIVE };
    sign | (magnitude * POWERS_OF_TEN[MAX_DIGITS - digits]) << 4 | digits as u64
}

#[inline(always)]
pub(super) fn int32_of_key(key: u64) -> i32 {
    let digits = (key & 0xf) as usize;
    let magnitude = ((key & !IS_NOT_NEGATIVE) >> 4) / POWERS_OF_TEN[MAX_DIGITS - digits];
    if key & IS_NOT_NEGATIVE == 0 {
        (magnitude as i32).wrapping_neg()
    } else {
        magnitude as i32
    }
}
