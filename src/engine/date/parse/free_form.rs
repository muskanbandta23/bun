//! Numbers and words in almost any order: `Tue Feb 1 2022 1:05 PM GMT+0100 (CET)`, `2/1/22`, `1-Feb-2022 13:05:09.5 EST`.

use bun_engine_text::CodeUnit;

use super::Parsed;
use super::cursor::{Cursor, Number, Spaces, Word, is_letter};
use super::iso::{self, Attempt};
use crate::civil::days_from_civil;

const MAX_YEAR: u32 = 1_000_000;

enum Keyword {
    /// From 1.
    Month(i32),
    /// What it adds to an hour below 12.
    HalfOfDay(i32),
    /// Hours behind UTC.
    Zone(i32),
}

fn keyword(word: &Word) -> Option<Keyword> {
    let keyword = match &word.prefix {
        b"jan" => Keyword::Month(1),
        b"feb" => Keyword::Month(2),
        b"mar" => Keyword::Month(3),
        b"apr" => Keyword::Month(4),
        b"may" => Keyword::Month(5),
        b"jun" => Keyword::Month(6),
        b"jul" => Keyword::Month(7),
        b"aug" => Keyword::Month(8),
        b"sep" => Keyword::Month(9),
        b"oct" => Keyword::Month(10),
        b"nov" => Keyword::Month(11),
        b"dec" => Keyword::Month(12),
        _ if word.is_longer => return None,
        b"am\0" => Keyword::HalfOfDay(0),
        b"pm\0" => Keyword::HalfOfDay(12),
        b"z\0\0" | b"ut\0" | b"utc" | b"gmt" => Keyword::Zone(0),
        b"edt" => Keyword::Zone(4),
        b"est" | b"cdt" => Keyword::Zone(5),
        b"cst" | b"mdt" => Keyword::Zone(6),
        b"mst" | b"pdt" => Keyword::Zone(7),
        b"pst" => Keyword::Zone(8),
        _ => return None,
    };
    Some(keyword)
}

struct Date {
    /// One until they are read.
    numbers: [i32; 3],
    count: usize,
    named_month: Option<i32>,
}

impl Date {
    fn push(&mut self, number: i32) -> Option<()> {
        *self.numbers.get_mut(self.count)? = number;
        self.count += 1;
        Some(())
    }

    fn days(&self) -> Option<i64> {
        let is_day = |number| (1..=31).contains(&number);
        let [first, second, third] = self.numbers;
        let (year, month, day) = match (self.named_month, is_day(first)) {
            (None, false) => (first, second, third),
            (None, true) => (third, first, second),
            (Some(month), false) => (first, month, second),
            (Some(month), true) => (second, month, first),
        };
        let year = match year {
            0..=49 => year + 2000,
            50..=99 => year + 1900,
            _ => year,
        };
        let is_valid = self.count != 0
            && year.unsigned_abs() <= MAX_YEAR
            && (1..=12).contains(&month)
            && is_day(day);
        is_valid.then(|| days_from_civil(year, month as u8, day as u8))
    }
}

#[derive(Default)]
struct Clock {
    /// Hour, minute, second and millisecond.
    numbers: [i32; 4],
    count: usize,
    half_of_day: Option<i32>,
}

impl Clock {
    fn push(&mut self, number: i32) -> Option<()> {
        *self.numbers.get_mut(self.count)? = number;
        self.count += 1;
        Some(())
    }

    /// There need not be room for it.
    fn push_last(&mut self, number: i32) {
        self.push(number);
        self.count = self.numbers.len();
    }

    fn can_go_on_with(&self, number: i32) -> bool {
        match self.count {
            1 | 2 => number <= 59,
            3 => number <= 999,
            _ => false,
        }
    }

    fn milliseconds(&self) -> Option<i64> {
        let [mut hour, minute, second, millisecond] = self.numbers;
        if let Some(half) = self.half_of_day {
            hour = (hour <= 12).then_some(hour % 12 + half)?;
        }
        let is_valid = matches!(
            [hour, minute, second, millisecond],
            [0..=23, 0..=59, 0..=59, 0..=999] | [24, 0, 0, 0]
        );
        is_valid.then(|| i64::from(((hour * 60 + minute) * 60 + second) * 1000 + millisecond))
    }
}

struct Zone {
    sign: i32,
    hours: i32,
    /// `None` after a colon, until they are read.
    minutes: Option<i32>,
}

impl Zone {
    /// From what follows the sign.
    fn read<T: CodeUnit>(sign: i32, cursor: &mut Cursor<'_, T>) -> Option<Zone> {
        let (value, digits) = cursor.number().map_or((0, 0), |it| (it.value, it.digits));
        let (hours, minutes) = match digits {
            _ if cursor.peek() == b':' => (value, None),
            1 | 2 => (value, Some(0)),
            3 | 4 => (value / 100, Some(value % 100)),
            _ => return None,
        };
        Some(Zone {
            sign,
            hours,
            minutes,
        })
    }

    fn is_utc(&self) -> bool {
        self.hours == 0 && self.minutes == Some(0)
    }

    /// The seconds are counted modulo 2^32.
    fn milliseconds(&self) -> Option<i64> {
        let seconds = (self.hours as u32)
            .wrapping_mul(3600)
            .wrapping_add(self.minutes.unwrap_or(0) as u32 * 60);
        Some(i64::from(self.sign * i32::try_from(seconds).ok()?) * 1000)
    }
}

struct Parser<'a, T> {
    cursor: Cursor<'a, T>,
    date: Date,
    clock: Clock,
    zone: Option<Zone>,
    /// Until then, whatever has no meaning is passed over.
    has_number: bool,
}

pub(super) fn parse<T: CodeUnit>(text: &[T], spaces: Spaces) -> Option<Parsed> {
    let mut cursor = Cursor::new(text, spaces);
    match iso::parse(&mut cursor) {
        Attempt::Complete(parsed) => Some(parsed),
        Attempt::Invalid => None,
        Attempt::Unfinished { numbers, count } => Parser {
            cursor,
            date: Date {
                numbers,
                count,
                named_month: None,
            },
            clock: Clock::default(),
            zone: None,
            has_number: count != 0,
        }
        .run(),
    }
}

impl<T: CodeUnit> Parser<'_, T> {
    fn run(mut self) -> Option<Parsed> {
        loop {
            match self.cursor.peek() {
                0 => break,
                b'0'..=b'9' => self.number()?,
                b'+' | b'-' => self.sign()?,
                b'(' => self.cursor.skip_comment(),
                b')' if self.has_number => return None,
                character if is_letter(character) => self.word()?,
                _ => self.cursor.advance(),
            }
        }
        let time = self.date.days()? * 86_400_000 + self.clock.milliseconds()?;
        Some(match self.zone {
            Some(zone) => Parsed::Utc(time - zone.milliseconds()?),
            None => Parsed::Local(time),
        })
    }

    #[inline(always)]
    fn number(&mut self) -> Option<()> {
        let Number { value, .. } = self.cursor.number()?;
        self.has_number = true;
        if self.cursor.skip(b':') {
            if self.cursor.skip(b':') {
                (self.clock.count == 0).then_some(())?;
                self.clock.push(value)?;
                return self.clock.push(0);
            }
            self.cursor.skip(b'.');
            return self.clock.push(value);
        }

        let continues_clock = self.clock.can_go_on_with(value);
        if self.cursor.skip(b'.') && continues_clock {
            self.clock.push(value)?;
            self.clock.push_last(self.cursor.number()?.milliseconds());
        } else if let Some(zone) = &mut self.zone
            && zone.minutes.is_none()
            && value <= 59
        {
            zone.minutes = Some(value);
        } else if continues_clock {
            self.clock.push_last(value);
            let is_set_off = matches!(self.cursor.peek(), 0 | b'\t'..=b'\r' | b' ' | b'+' | b'-')
                || self.cursor.is_at_letter(b'z');
            is_set_off.then_some(())?;
        } else {
            self.date.push(value)?;
            self.cursor.skip(b'-');
        }
        Some(())
    }

    #[inline(always)]
    fn word(&mut self) -> Option<()> {
        match keyword(&self.cursor.word()) {
            Some(Keyword::Month(month)) => {
                self.date.named_month = Some(month);
                self.cursor.skip(b'-');
            }
            Some(Keyword::HalfOfDay(half)) if self.clock.count != 0 => {
                self.clock.half_of_day = Some(half);
            }
            Some(Keyword::Zone(hours)) if self.has_number => {
                self.zone = Some(Zone {
                    sign: -1,
                    hours,
                    minutes: Some(0),
                });
            }
            _ if self.has_number || self.cursor.peek().is_ascii_digit() => return None,
            _ => {}
        }
        Some(())
    }

    #[inline(always)]
    fn sign(&mut self) -> Option<()> {
        let sign = self.cursor.sign()?;
        if self.clock.count != 0 || self.zone.as_ref().is_some_and(Zone::is_utc) {
            self.zone = Some(Zone::read(sign, &mut self.cursor)?);
            return Some(());
        }
        (!self.has_number).then_some(())
    }
}
