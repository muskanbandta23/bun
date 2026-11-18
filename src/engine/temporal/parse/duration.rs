use bun_engine_text::{CodeUnit, Text, with_text};

use super::cursor::Cursor;
use crate::Unit;
use crate::unit::NANOSECONDS_PER_SECOND;

type Fields = [f64; Unit::ALL.len()];

/// ParseTemporalDurationString, without the check of the range.
pub(crate) fn duration(text: Text<'_>) -> Option<Fields> {
    with_text!(text, |units| Cursor::parse_all(units, Cursor::duration))
}

impl<T: CodeUnit> Cursor<'_, T> {
    /// Saturates above the greatest count that a duration can have.
    #[inline]
    fn count(&mut self) -> f64 {
        const LIMIT: u64 = 1 << 60;
        let digits = self.take_while(|next| next.is_ascii_digit());
        digits.iter().fold(0, |count, digit| {
            (count * 10 + u64::from(digit.to_u32() & 0xf)).min(LIMIT)
        }) as f64
    }

    fn duration(&mut self) -> Option<Fields> {
        let is_negative = self.sign() == Some(-1);
        let mut fields = Fields::default();
        if !self.eat_letter(b'P') {
            return None;
        }
        let has_date = self.duration_date(&mut fields)?;
        if self.eat_letter(b'T') {
            self.duration_time(&mut fields)?;
        } else if !has_date {
            return None;
        }
        if is_negative {
            fields = fields.map(|field| 0.0 - field);
        }
        Some(fields)
    }

    fn duration_date(&mut self, fields: &mut Fields) -> Option<bool> {
        let mut previous = None;
        while self.peek().is_ascii_digit() {
            let count = self.count();
            let unit = match self.next()?.to_ascii_uppercase() {
                b'Y' => Unit::Year,
                b'M' => Unit::Month,
                b'W' => Unit::Week,
                b'D' => Unit::Day,
                _ => return None,
            };
            if previous.is_some_and(|previous| unit >= previous) {
                return None;
            }
            fields[unit as usize] = count;
            previous = Some(unit);
        }
        Some(previous.is_some())
    }

    fn duration_time(&mut self, fields: &mut Fields) -> Option<()> {
        let mut previous = Unit::Day;
        while self.peek().is_ascii_digit() {
            let count = self.count();
            let fraction = self.fraction()?;
            let unit = match self.next()?.to_ascii_uppercase() {
                b'H' => Unit::Hour,
                b'M' => Unit::Minute,
                b'S' => Unit::Second,
                _ => return None,
            };
            if unit >= previous {
                return None;
            }
            fields[unit as usize] = count;
            previous = unit;
            if let Some(fraction) = fraction {
                spread_fraction(fraction, unit, fields);
                break;
            }
        }
        (previous != Unit::Day).then_some(())
    }
}

/// Over the units below `unit`.
fn spread_fraction(fraction: u32, unit: Unit, fields: &mut Fields) {
    let mut rest = i64::from(fraction) * (unit.nanoseconds() / NANOSECONDS_PER_SECOND);
    for &smaller in &Unit::ALL[unit as usize + 1..] {
        fields[smaller as usize] = (rest / smaller.nanoseconds()) as f64;
        rest %= smaller.nanoseconds();
    }
}
