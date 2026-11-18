use bun_engine_standalone as _;
use bun_icu::date_pattern::{self, Field, PatternGenerator, Width};

fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn text(units: &[u16]) -> String {
    String::from_utf16(units).unwrap()
}

#[test]
fn patterns_from_skeletons() {
    let mut generator = PatternGenerator::open(c"de").unwrap();
    let mut best = |skeleton| text(&generator.best_pattern(&units(skeleton)).unwrap());
    assert_eq!(best("yMd"), "d.M.y");
    assert_eq!(best("j"), "HH 'Uhr'");
    assert_eq!(best(""), "");
}

#[test]
fn skeletons_from_patterns() {
    let skeleton = |pattern| text(&date_pattern::skeleton(&units(pattern)).unwrap());
    assert_eq!(skeleton("EEEE, d 'de' MMMM 'de' y"), "yMMMMEEEEd");
    assert_eq!(skeleton("h 'o''clock' a"), "ah");
    assert_eq!(skeleton(""), "");
}

#[test]
fn names_of_fields() {
    let generator = PatternGenerator::open(c"en").unwrap();
    let name = |field, width| text(&generator.field_name(field, width).unwrap());
    assert_eq!(name(Field::Weekday, Width::Wide), "day of the week");
    assert_eq!(name(Field::Quarter, Width::Abbreviated), "qtr.");
    assert_eq!(name(Field::Minute, Width::Narrow), "min");
}
