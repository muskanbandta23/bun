//! Where the members of a set are in a text, with vector code where it pays.

use bun_engine_text::{CodeUnit, Text};
use bun_highway::ByteSet;
use smallvec::SmallVec;

use super::Latin1Set;

/// The shortest text for which building the vector form of a set pays for itself.
const WORTH_PREPARING: usize = 1 << 10;

/// The most members that the kernel for a few characters takes.
const FEW: usize = 16;

/// The index of the first unit that `set` holds. A unit above U+00FF is never a member.
#[inline]
pub fn find_member<U: CodeUnit>(units: &[U], set: &Latin1Set) -> Option<usize> {
    find(units, set, true)
}

/// The index of the first unit that `set` does not hold. A unit above U+00FF is never a member.
#[inline]
pub fn find_non_member<U: CodeUnit>(units: &[U], set: &Latin1Set) -> Option<usize> {
    find(units, set, false)
}

#[inline]
fn find<U: CodeUnit>(units: &[U], set: &Latin1Set, inside: bool) -> Option<usize> {
    // The characters that end the search, and whether a unit above U+00FF is one of them.
    let (wanted, above_ends) = match inside {
        true => (*set, false),
        false => (set.complement(), true),
    };
    match (U::text(units), wanted.len()) {
        (_, 0) if !above_ends => None,
        (Text::Latin1(bytes), _) => in_bytes(bytes, wanted),
        (Text::Utf16(wide), 1) if !above_ends => {
            let unit = wanted.iter().next()?;
            bun_highway::text::find_unit(wide, unit.into())
        }
        (Text::Utf16(wide), _) => in_wide(wide, wanted, above_ends),
    }
}

fn in_bytes(bytes: &[u8], wanted: Latin1Set) -> Option<usize> {
    let members: SmallVec<[u8; FEW]> = wanted.iter().take(FEW + 1).collect();
    match members[..] {
        [] => return None,
        [only] => return bun_highway::index_of_char(bytes, only),
        _ if members.len() <= FEW => return bun_highway::index_of_any_char(bytes, &members),
        _ => {}
    }
    if let [only] = (!wanted).iter().take(2).collect::<SmallVec<[u8; 2]>>()[..] {
        return bun_highway::index_of_not_char(bytes, only);
    }
    if bytes.len() >= WORTH_PREPARING
        && let Some(outside) = vector_form(!wanted)
    {
        // Everything the set does not hold comes before the first unit it does.
        let skipped = outside.prefix_len(bytes);
        return (skipped < bytes.len()).then_some(skipped);
    }
    position(bytes, wanted, false)
}

fn in_wide(wide: &[u16], wanted: Latin1Set, above_ends: bool) -> Option<usize> {
    if wanted.is_empty() {
        return bun_highway::text::find_above(wide, 0xff);
    }
    position(wide, wanted, above_ends)
}

/// The rows that the kernel takes, which `ByteSet::new` would otherwise refuse.
fn vector_form(set: Latin1Set) -> Option<ByteSet> {
    let rows = |words: &[u64; 4]| {
        let mut rows = [0u16; 16];
        for (index, row) in rows.iter_mut().enumerate() {
            *row = (words[index / 4] >> (16 * (index % 4))) as u16;
        }
        rows
    };
    let rows = rows(set.words());
    let mut patterns: SmallVec<[u16; 8]> = SmallVec::new();
    for row in rows.into_iter().filter(|&it| it != 0) {
        if !patterns.contains(&row) {
            if patterns.len() == patterns.capacity() {
                return None;
            }
            patterns.push(row);
        }
    }
    let ranges: SmallVec<[_; 32]> = set
        .ranges()
        .map(|it| it.first as u8..=it.last as u8)
        .collect();
    Some(ByteSet::new(&ranges))
}

/// Tests a block at a time without a branch, which compiles to vector instructions.
#[inline]
fn position<U: CodeUnit>(units: &[U], wanted: Latin1Set, above_ends: bool) -> Option<usize> {
    const BLOCK: usize = 16;
    let words = *wanted.words();
    let test = move |unit: U| {
        let c = unit.to_u32();
        let member = (words[(c >> 6) as usize & 3] >> (c & 63)) & 1 == 1;
        let is_latin1 = c <= 0xff;
        (is_latin1 & member) | (!is_latin1 & above_ends)
    };
    let has_none = |block: &&[U; BLOCK]| !block.iter().fold(false, |any, &it| any | test(it));
    let skipped = units.as_chunks().0.iter().take_while(has_none).count() * BLOCK;
    let found = units[skipped..].iter().position(|&unit| test(unit));
    found.map(|at| skipped + at)
}
