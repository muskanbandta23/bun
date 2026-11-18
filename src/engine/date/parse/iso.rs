//! ECMA-262, 21.4.1.32. Beyond it, a fraction can have any number of digits, an offset can lack its colon, and every month
//! has 31 days.

use bun_engine_text::CodeUnit;

use super::Parsed;
use super::cursor::Cursor;
use crate::civil::days_from_civil;

pub(super) enum Attempt {
    Complete(Parsed),
    Invalid,
    /// The text goes on in another format after these numbers.
    Unfinished {
        numbers: [i32; 3],
        count: usize,
    },
}

#[inline(always)]
pub(super) fn parse<T: CodeUnit>(cursor: &mut Cursor<'_, T>) -> Attempt {
    let mut numbers = [1; 3];
    let unfinished = |numbers, count| Attempt::Unfinished { numbers, count };

    let year = match cursor.sign() {
        None => cursor.fixed::<4>(0, 9999),
        Some(sign) => (cursor.fixed::<6>(0, 999_999))
            .filter(|&year| sign > 0 || year > 0)
            .map(|year| sign * year),
    };
    let Some(year) = year else {
        return unfinished(numbers, 0);
    };
    numbers[0] = year;
    let mut count = 1;
    for max in [12, 31] {
        if !cursor.skip(b'-') {
            break;
        }
        let Some(number) = cursor.fixed::<2>(1, max) else {
            return unfinished(numbers, count);
        };
        numbers[count] = number;
        count += 1;
    }

    let midnight = days_from_civil(year, numbers[1] as u8, numbers[2] as u8) * 86_400_000;
    if cursor.is_at_end() {
        return Attempt::Complete(Parsed::Utc(midnight));
    }
    if !cursor.skip_letter(b't') {
        return unfinished(numbers, count);
    }
    time_and_zone(cursor, midnight).map_or(Attempt::Invalid, Attempt::Complete)
}

#[inline(always)]
fn time_and_zone<T: CodeUnit>(cursor: &mut Cursor<'_, T>, midnight: i64) -> Option<Parsed> {
    let hour = cursor.fixed::<2>(0, 24)?;
    let max = if hour == 24 { 0 } else { 59 };
    cursor.skip(b':').then_some(())?;
    let minute = cursor.fixed::<2>(0, max)?;
    let (mut second, mut millisecond) = (0, 0);
    if cursor.skip(b':') {
        second = cursor.fixed::<2>(0, max)?;
        if cursor.skip(b'.') {
            let fraction = cursor.number().filter(|it| hour < 24 || it.value == 0)?;
            millisecond = fraction.milliseconds();
        }
    }
    let time = midnight + i64::from(((hour * 60 + minute) * 60 + second) * 1000 + millisecond);

    let parsed = if cursor.skip_letter(b'z') {
        Parsed::Utc(time)
    } else if let Some(sign) = cursor.sign() {
        Parsed::Utc(time - i64::from(sign * offset_minutes(cursor)? * 60_000))
    } else {
        Parsed::Local(time)
    };
    cursor.is_at_end().then_some(parsed)
}

#[inline(always)]
fn offset_minutes<T: CodeUnit>(cursor: &mut Cursor<'_, T>) -> Option<i32> {
    if let Some(both) = cursor.fixed::<4>(0, 2359) {
        return (both % 100 < 60).then_some(both / 100 * 60 + both % 100);
    }
    let hours = cursor.fixed::<2>(0, 23)?;
    cursor.skip(b':').then_some(())?;
    Some(hours * 60 + cursor.fixed::<2>(0, 59)?)
}
