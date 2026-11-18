use bun_engine_standalone as _;
use bun_icu::calendar::{self, Calendar, Direction, Field, Limit, Side, ZoneName, ZoneOffset};
use bun_icu::{Enumeration, Error, time_zone};

const HOUR: i32 = 3_600_000;

fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn items(mut enumeration: Enumeration) -> Vec<Vec<u8>> {
    let mut items = Vec::new();
    while let Some(item) = enumeration.next().unwrap() {
        items.push(item.to_vec());
    }
    items
}

fn date(calendar: &mut Calendar) -> [i32; 3] {
    [Field::ExtendedYear, Field::Month, Field::DayOfMonth].map(|it| calendar.get(it).unwrap())
}

#[test]
fn fields() {
    let mut hebrew = Calendar::open(&units("UTC"), c"und@calendar=hebrew").unwrap();
    hebrew.set_millis(0.0).unwrap();
    assert_eq!(date(&mut hebrew), [5730, 3, 23]);
    assert_eq!(
        hebrew.limit(Field::DayOfYear, Limit::ActualMaximum),
        Ok(383)
    );
    assert_eq!(hebrew.limit(Field::DayOfMonth, Limit::LeastMaximum), Ok(29));
    assert_eq!(hebrew.set_gregorian_change(0.0), Err(Error::UNSUPPORTED));

    let mut copy = hebrew.try_clone().unwrap();
    copy.add(Field::DayOfMonth, 7).unwrap();
    assert_eq!(date(&mut copy), [5730, 4, 1]);
    assert_eq!(hebrew.millis(), Ok(0.0));

    copy.clear();
    copy.set(Field::ExtendedYear, 5730);
    copy.set(Field::Month, 3);
    copy.set(Field::DayOfMonth, 24);
    assert_eq!(copy.millis(), Ok(86_400_000.0));
    assert_eq!(copy.set_millis(f64::NAN), Err(Error::ILLEGAL_ARGUMENT));
}

#[test]
fn gregorian_change() {
    let mut calendar = Calendar::open(&units("UTC"), c"").unwrap();
    calendar.set_millis(-12_219_379_200_000.0).unwrap();
    assert_eq!(date(&mut calendar), [1582, 9, 4]);
    calendar.set_gregorian_change(-8.64e15).unwrap();
    calendar.set_millis(-12_219_379_200_000.0).unwrap();
    assert_eq!(date(&mut calendar), [1582, 9, 14]);
}

#[test]
fn zone_rules() {
    let (spring, autumn) = (1_710_064_800_000.0, 1_730_624_400_000.0);
    let offset = |daylight| {
        Ok(ZoneOffset {
            standard: -8 * HOUR,
            daylight,
        })
    };
    let mut calendar = Calendar::open(&units("America/Los_Angeles"), c"").unwrap();
    calendar.set_millis(spring).unwrap();
    assert_eq!(calendar.zone_offset(), offset(HOUR));
    assert_eq!(calendar.zone_transition(Direction::Next), Ok(Some(autumn)));
    assert_eq!(
        calendar.zone_transition(Direction::PreviousInclusive),
        Ok(Some(spring))
    );
    assert_eq!(
        calendar.zone_transition(Direction::Previous),
        Ok(Some(1_699_174_800_000.0))
    );

    calendar.set_millis(spring - 7.5 * f64::from(HOUR)).unwrap();
    assert_eq!(
        calendar.zone_offset_from_local(Side::BeforeTransition),
        offset(0)
    );
    assert_eq!(
        calendar.zone_offset_from_local(Side::AfterTransition),
        offset(HOUR)
    );

    let name = |kind| calendar.zone_name(kind, c"en-US").unwrap().to_vec();
    assert_eq!(name(ZoneName::Standard), units("Pacific Standard Time"));
    assert_eq!(name(ZoneName::Daylight), units("Pacific Daylight Time"));

    calendar.set_time_zone(&units("GMT+0530")).unwrap();
    assert_eq!(calendar.get(Field::ZoneOffset), Ok(11 * HOUR / 2));
    assert_eq!(calendar.zone_transition(Direction::Next), Ok(None));
}

#[test]
fn weeks_and_systems() {
    let calendar = Calendar::open(&units("UTC"), c"he_IL").unwrap();
    assert_eq!(calendar.first_day_of_week(), 1);
    let weekend = (1..=7).filter(|&day| calendar.starts_in_weekend(day).unwrap());
    assert!(weekend.eq([6, 7]));
    assert!(calendar.starts_in_weekend(8).is_err());

    let common = items(calendar::common_systems(c"th_TH").unwrap());
    assert_eq!(common, [&b"buddhist"[..], b"gregorian"]);
    let all = items(calendar::all_systems(c"th_TH").unwrap());
    assert!(all.starts_with(&common) && all.len() > common.len());
}

#[test]
fn zone_identifiers() {
    let canonical = |id| time_zone::canonical_id(&units(id)).map(|it| it.to_vec());
    assert_eq!(canonical("Asia/Kolkata"), Ok(units("Asia/Calcutta")));
    assert_eq!(canonical("GMT+5"), Ok(units("GMT+05:00")));
    assert!(canonical("Nowhere").is_err());
    assert!(canonical("").is_err());
    if let Ok(id) = time_zone::iana_id(&units("Asia/Calcutta")) {
        assert_eq!(id.to_vec(), units("Asia/Kolkata"));
    }

    let japan = items(time_zone::canonical_ids(Some(c"JP")).unwrap());
    assert_eq!(japan, [b"Asia/Tokyo"]);
    let canonical = items(time_zone::canonical_ids(None).unwrap());
    let all = items(time_zone::all_ids().unwrap());
    assert!(canonical.iter().all(|id| all.contains(id)));
    assert!(all.contains(&b"Japan".to_vec()) && !canonical.contains(&b"Japan".to_vec()));

    assert_eq!(time_zone::host(), time_zone::host());
}
