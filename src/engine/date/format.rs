//! The strings of ECMA-262, 21.4.4.

use crate::ascii_buffer::{AsciiBuffer, two_digits};
use crate::fields::DateFields;
use crate::time_value::{MAX_TIME_VALUE, ms_from_time};

/// As many as the bits of the field can name.
const WEEK_DAYS: [[u8; 3]; 8] = [
    *b"Sun", *b"Mon", *b"Tue", *b"Wed", *b"Thu", *b"Fri", *b"Sat", *b"   ",
];
const MONTHS: [[u8; 3]; 16] = [
    *b"Jan", *b"Feb", *b"Mar", *b"Apr", *b"May", *b"Jun", *b"Jul", *b"Aug", *b"Sep", *b"Oct",
    *b"Nov", *b"Dec", *b"   ", *b"   ", *b"   ", *b"   ",
];

/// Three numbers below 100, as two digits each in the places of the zeros of `pattern`.
#[inline(always)]
fn three_pairs(numbers: [u32; 3], pattern: &[u8; 8]) -> [u8; 8] {
    let [first, second, third] = numbers.map(u64::from);
    let lanes = first | second << 24 | third << 48;
    let tens = (lanes * 103) >> 10 & 0x000f_0000_0f00_000f;
    let ones = lanes - tens * 10;
    (tens | ones << 8 | u64::from_le_bytes(*pattern)).to_le_bytes()
}

#[inline(always)]
fn clock(fields: DateFields) -> [u8; 8] {
    let numbers = [fields.hour(), fields.minute(), fields.second()];
    three_pairs(numbers.map(u32::from), b"00:00:00")
}

impl<const N: usize> AsciiBuffer<N> {
    /// At least four digits.
    #[inline(always)]
    fn push_year(&mut self, year: i32) {
        let mut digits = year.unsigned_abs();
        if !(0..=9999).contains(&year) {
            if year < 0 {
                self.push(b"-");
            }
            let [first, second] = two_digits(digits / 10_000);
            if digits >= 100_000 {
                self.push(&[first]);
            }
            if digits >= 10_000 {
                self.push(&[second]);
            }
            digits %= 10_000;
        }
        self.push(&two_digits(digits / 100));
        self.push(&two_digits(digits % 100));
    }

    /// DateString.
    #[inline(always)]
    pub(crate) fn push_date(&mut self, fields: DateFields) {
        self.push(&WEEK_DAYS[usize::from(fields.week_day())]);
        self.push(b" ");
        self.push(&MONTHS[usize::from(fields.month())]);
        self.push(b" ");
        self.push(&two_digits(fields.day().into()));
        self.push(b" ");
        self.push_year(fields.year());
    }

    /// TimeString, and TimeZoneString up to the name.
    #[inline(always)]
    pub(crate) fn push_time_and_offset(&mut self, fields: DateFields) {
        self.push(&clock(fields));
        self.push(b" GMT");
        self.push_offset(fields.offset_minutes() as i16, b"");
    }
}

/// `Tue Feb 01 2022`
pub fn date_string(fields: DateFields) -> AsciiBuffer<18> {
    let mut text = AsciiBuffer::new();
    text.push_date(fields);
    text
}

/// `Tue, 01 Feb 2022 13:05:09 GMT`, which for a year of four digits is the IMF-fixdate of RFC 9110.
pub fn utc_string(fields: DateFields) -> AsciiBuffer<32> {
    let mut text = AsciiBuffer::new();
    text.push(&WEEK_DAYS[usize::from(fields.week_day())]);
    text.push(b", ");
    text.push(&two_digits(fields.day().into()));
    text.push(b" ");
    text.push(&MONTHS[usize::from(fields.month())]);
    text.push(b" ");
    text.push_year(fields.year());
    text.push(b" ");
    text.push(&clock(fields));
    text.push(b" GMT");
    text
}

/// `2022-02-01T13:05:09.123Z`. `None` if `time` is not in the range of time values.
pub fn iso_string(time: f64) -> Option<AsciiBuffer<27>> {
    let fields = DateFields::utc(time).filter(|_| time.abs() <= MAX_TIME_VALUE)?;
    let mut text = AsciiBuffer::new();
    let mut year = fields.year().unsigned_abs();
    if !(0..=9999).contains(&fields.year()) {
        text.push(if fields.year() < 0 { b"-" } else { b"+" });
        text.push(&two_digits(year / 10_000));
        year %= 10_000;
    }
    text.push(&two_digits(year / 100));
    let numbers = [
        year % 100,
        u32::from(fields.month()) + 1,
        fields.day().into(),
    ];
    text.push(&three_pairs(numbers, b"00-00-00"));
    text.push(b"T");
    text.push(&clock(fields));
    let millisecond = u32::from(ms_from_time(time));
    let [tens, ones] = two_digits(millisecond % 100);
    text.push(&[b'.', b'0' + (millisecond / 100) as u8, tens, ones, b'Z']);
    Some(text)
}
