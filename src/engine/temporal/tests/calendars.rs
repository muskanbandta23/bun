mod support;

use bun_engine_temporal::{CalendarId, FieldSet, Overflow, PlainDate};
use support::{date, date_string, fields, text};

fn describe(date: PlainDate) -> String {
    let fields = date.calendar().date(date.iso_date()).unwrap();
    let era = (fields.era).map_or(String::new(), |it| {
        format!(" {} {}", it.era.code(), it.year)
    });
    let (year, month, day) = (fields.year, fields.month, fields.day);
    format!("{year} {month} {} {day}{era}", fields.month_code())
}

#[test]
fn identifiers() {
    assert!(CalendarId::ALL.is_sorted_by_key(|it| it.identifier()));
    for &calendar in CalendarId::ALL {
        let upper = calendar.identifier().to_ascii_uppercase();
        assert_eq!(CalendarId::find(text(&upper)), Some(calendar));
    }
    assert_eq!(
        CalendarId::find(text("islamicc")),
        Some(CalendarId::IslamicCivil)
    );
    assert_eq!(CalendarId::find(text("islamic")), None);
}

#[test]
fn first_and_last_dates() {
    for (calendar, first, last) in [
        (
            "buddhist",
            "-271278 4 M04 19 be -271278",
            "276303 9 M09 13 be 276303",
        ),
        (
            "coptic",
            "-272099 3 M03 23 am -272099",
            "275471 5 M05 22 am 275471",
        ),
        (
            "ethioaa",
            "-266323 3 M03 23 aa -266323",
            "281247 5 M05 22 aa 281247",
        ),
        (
            "ethiopic",
            "-271823 3 M03 23 aa -266323",
            "275747 5 M05 22 am 275747",
        ),
        (
            "gregory",
            "-271821 4 M04 19 bce 271822",
            "275760 9 M09 13 ce 275760",
        ),
        (
            "hebrew",
            "-268058 11 M11 4 am -268058",
            "279517 10 M09 11 am 279517",
        ),
        (
            "indian",
            "-271899 1 M01 29 shaka -271899",
            "275682 6 M06 22 shaka 275682",
        ),
        (
            "islamic-civil",
            "-280804 3 M03 21 bh 280805",
            "283583 5 M05 23 ah 283583",
        ),
        (
            "islamic-tbla",
            "-280804 3 M03 22 bh 280805",
            "283583 5 M05 24 ah 283583",
        ),
        (
            "islamic-umalqura",
            "-280804 3 M03 21 bh 280805",
            "283583 5 M05 23 ah 283583",
        ),
        (
            "japanese",
            "-271821 4 M04 19 bce 271822",
            "275760 9 M09 13 reiwa 273742",
        ),
        (
            "persian",
            "-272442 1 M01 9 ap -272442",
            "275139 7 M07 12 ap 275139",
        ),
        (
            "roc",
            "-273732 4 M04 19 broc 273733",
            "273849 9 M09 13 roc 273849",
        ),
    ] {
        let calendar = CalendarId::find(text(calendar)).unwrap();
        assert_eq!(
            describe(date("-271821-04-19").with_calendar(calendar)),
            first
        );
        assert_eq!(
            describe(date("+275760-09-13").with_calendar(calendar)),
            last
        );
    }
}

#[test]
fn leap_months() {
    for (calendar, given, expected) in [
        (
            CalendarId::Chinese,
            "year=2023 monthCode=M02L day=1",
            "2023-03-22",
        ),
        (
            CalendarId::Chinese,
            "year=2024 monthCode=M02L day=1",
            "2024-03-10",
        ),
        (
            CalendarId::Dangi,
            "year=2012 monthCode=M03L day=1",
            "2012-04-21",
        ),
        (
            CalendarId::Hebrew,
            "year=5784 monthCode=M05L day=30",
            "2024-03-10",
        ),
        (
            CalendarId::Hebrew,
            "year=5785 monthCode=M05L day=30",
            "2025-03-29",
        ),
    ] {
        let fields = fields(FieldSet::Date, calendar, given);
        let date = PlainDate::from_fields(&fields, Overflow::Constrain).unwrap();
        assert_eq!(date_string(date), expected);
    }
}
