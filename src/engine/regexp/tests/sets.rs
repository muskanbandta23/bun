use bun_engine_regexp::set::{
    CharRange, Latin1Set, Set, SetBuilder, SharedSet, find_member, find_non_member,
};
use bun_engine_standalone as _;
use bun_engine_text::Text;
use bun_engine_unicode::{IgnoreCase, MAX_CODE_POINT, Property, is_str_white_space};

const BOUNDS: [u32; 12] = [
    0,
    0x3f,
    0x40,
    0x41,
    0xfe,
    0xff,
    0x100,
    0x101,
    0xffff,
    0x10000,
    MAX_CODE_POINT - 1,
    MAX_CODE_POINT,
];

fn set_of(ranges: &[(u32, u32)], ignore_case: Option<IgnoreCase>) -> Set {
    let mut builder = SetBuilder::default();
    for &(first, last) in ranges {
        builder.add_range(first, last).unwrap();
    }
    builder.finish(ignore_case).unwrap()
}

/// Bit `n` stands for the characters from bound `n` up to the next one.
fn set_of_cells(cells: u32) -> Set {
    let cell = |n: usize| {
        let last = BOUNDS.get(n + 1).map_or(MAX_CODE_POINT, |next| next - 1);
        (BOUNDS[n], last)
    };
    let chosen = (0..BOUNDS.len()).rev().filter(|n| cells >> n & 1 != 0);
    set_of(&chosen.map(cell).collect::<Vec<_>>(), None)
}

fn property(name: &str) -> &'static SharedSet {
    SharedSet::of_property(Property::of_characters(Text::from_ascii(name)).unwrap())
}

#[test]
fn operations_agree_with_bit_operations() {
    let all = (1 << BOUNDS.len()) - 1;
    for a in (0..=all).step_by(37).chain([all]) {
        let x = set_of_cells(a);
        assert_eq!(x.complement().unwrap(), set_of_cells(!a & all));
        assert_eq!(x.is_empty(), a == 0);
        let ranges: Vec<_> = x.ranges().collect();
        assert!(ranges.is_sorted_by(|a, b| a.last + 1 < b.first));
        let pairs: Vec<_> = ranges.iter().map(|it| (it.first, it.last)).collect();
        assert_eq!(set_of(&pairs, None), x);
        let table = x.build_table(MAX_CODE_POINT).unwrap();
        for (n, &c) in BOUNDS.iter().enumerate() {
            assert_eq!(x.contains(c), a >> n & 1 != 0);
            assert_eq!(table.contains(c), a >> n & 1 != 0);
            for (m, &last) in BOUNDS.iter().enumerate().skip(n) {
                let (range, cells) = (CharRange::new(c, last), (2 << m) - (1 << n));
                assert_eq!(x.intersects_range(range), a & cells != 0);
                assert_eq!(x.contains_range(range), a & cells == cells);
            }
        }
        for b in (0..=all).step_by(53).chain([all]) {
            let y = set_of_cells(b);
            assert_eq!(x.union(&y).unwrap(), set_of_cells(a | b));
            assert_eq!(x.intersection(&y).unwrap(), set_of_cells(a & b));
            assert_eq!(x.difference(&y).unwrap(), set_of_cells(a & !b));
            assert_eq!(x.intersects(&y), a & b != 0);
            assert_eq!(x.is_subset_of(&y), a & !b == 0);
        }
    }
}

#[test]
fn closure_of_a_character_is_its_equivalents() {
    for ignore_case in [IgnoreCase::CodeUnits, IgnoreCase::CodePoints] {
        for c in 0..0x2200 {
            let equal = ignore_case.equivalents(c).chain([c]).map(|c| (c, c));
            let expected = set_of(&equal.collect::<Vec<_>>(), None);
            assert_eq!(set_of(&[(c, c)], Some(ignore_case)), expected);
        }
    }
}

#[test]
fn built_in_sets() {
    let (space, dot) = (Set::space(), Set::NOT_LINE_TERMINATOR);
    assert!((0..0x10000).all(|c| space.contains(c) == is_str_white_space(c)));
    assert_eq!(Set::LINE_TERMINATOR.complement().unwrap(), dot);
    let word = [(0x30, 0x39), (0x41, 0x5a), (0x5f, 0x5f), (0x61, 0x7a)];
    assert_eq!(set_of(&word, Some(IgnoreCase::CodeUnits)), Set::WORD);
    let folded = set_of(&word, Some(IgnoreCase::CodePoints));
    assert_eq!(folded, Set::WORD_IGNORING_CASE);
    assert_eq!(set_of(&[(0x1234, 0x1234)], None).single(), Some(0x1234));
    assert_eq!(set_of(&[(0xff, 0x100)], None).single(), None);
}

#[test]
fn derived_sets_are_shared() {
    let upper = property("Lu");
    assert!(core::ptr::eq(upper.complement().complement(), upper));
    let cased = upper.closed_over_case();
    assert!(core::ptr::eq(cased.closed_over_case(), cased));
    assert!(cased.to_set().contains(u32::from('a')) && !upper.to_set().contains(u32::from('a')));

    let mut builder = SetBuilder::default();
    builder.add(0xfeff).unwrap();
    builder.add(u32::from(',')).unwrap();
    builder.add_set(&Set::space()).unwrap();
    let inverted = builder.finish(None).unwrap().complement().unwrap();
    let not_space = Set::space().complement().unwrap();
    assert!(core::ptr::eq(
        inverted.shared().unwrap(),
        not_space.shared().unwrap()
    ));
}

#[test]
fn tables_agree_with_ranges() {
    for name in ["L", "Assigned", "Co", "ASCII", "Emoji"] {
        for shared in [property(name), property(name).complement()] {
            let (set, table) = (shared.to_set(), shared.table());
            let units = set.build_table(0xffff).unwrap();
            assert_eq!(units.limit(), 0x10000);
            let bounds = set
                .ranges()
                .flat_map(|it| [it.first.saturating_sub(1), it.first, it.last, it.last + 1]);
            for c in bounds.filter(|&c| c <= MAX_CODE_POINT) {
                assert_eq!(table.contains(c), set.contains(c));
                assert!(c > 0xffff || units.contains(c) == set.contains(c));
            }
        }
    }
}

/// A deterministic sequence, so that a failure is reproducible.
fn numbers(seed: u64) -> impl FnMut() -> u64 {
    let mut state = seed | 1;
    move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    }
}

#[test]
fn finds_members_in_text() {
    let mut next = numbers(0x5eed);
    for round in 0..600 {
        // From a single character to nearly all of Latin-1, and the complement of each.
        let mut members = Latin1Set::EMPTY;
        for _ in 0..1 + round % 37 {
            let first = (next() % 0x100) as u8;
            let last = first.saturating_add((next() % (1 + round as u64 % 19)) as u8);
            members.insert_range(first, last);
        }
        if round % 3 == 0 {
            members = !members;
        }
        for length in [0, 1, 15, 16, 17, 63, 64, 100, 1100, 4096] {
            let mut units: Vec<u16> = (0..length)
                .map(|_| match next() % 8 {
                    0 => (next() % 0x11000) as u16,
                    _ => (next() % 0x100) as u16,
                })
                .collect();
            // Texts whose only member is at the end, and texts without one.
            match next() % 4 {
                0 => units.iter_mut().for_each(|it| *it = 0x3000),
                1 if length > 0 => units[length - 1] = 0x3000,
                _ => {}
            }
            let member = |&unit: &u16| unit <= 0xff && members.contains(unit as u8);
            let expected = (
                units.iter().position(member),
                units.iter().position(|it| !member(it)),
            );
            let narrow: Option<Vec<u8>> = units.iter().map(|&it| u8::try_from(it).ok()).collect();
            if let Some(narrow) = &narrow {
                let found = (
                    find_member(narrow, &members),
                    find_non_member(narrow, &members),
                );
                assert_eq!(found, expected, "{length} units of Latin-1, round {round}");
            }
            let found = (
                find_member(&units, &members),
                find_non_member(&units, &members),
            );
            assert_eq!(found, expected, "{length} units, round {round}");
        }
    }
}
