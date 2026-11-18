//! How often characters are expected in a subject.

use bun_engine_text::{CodeUnit, Text, with_text};

use crate::set::{CharRange, Latin1Set};

/// In 65536 characters of a mixture of prose, markup, data and source code.
#[rustfmt::skip]
const ASCII: [u16; 128] = [
    8, 8, 8, 8, 8, 8, 8, 8, 8, 200, 1900, 8, 8, 200, 8, 8,
    8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8,
    9600, 64, 1350, 130, 64, 80, 64, 230, 620, 620, 190, 64, 790, 1200, 970, 570,
    480, 380, 250, 240, 160, 190, 160, 140, 140, 130, 850, 540, 120, 540, 160, 64,
    64, 170, 130, 140, 110, 200, 90, 70, 70, 140, 64, 64, 100, 90, 110, 120,
    110, 64, 140, 200, 200, 70, 64, 70, 64, 64, 64, 170, 170, 150, 64, 200,
    300, 2650, 790, 1530, 1360, 4400, 900, 720, 900, 2300, 130, 320, 1780, 1120, 2560, 2670,
    1090, 90, 2750, 2500, 3460, 1220, 430, 400, 350, 520, 70, 420, 130, 420, 64, 8,
];
const OTHER_LATIN1: u16 = 4;
const SCALE: f32 = 1.0 / 65536.0;

const SAMPLE: usize = 128;
const LEAST_SAMPLE: usize = 32;
const TRUST_IN_SAMPLE: f32 = 0.875;
const WIDE_WITHOUT_SAMPLE: f32 = 0.03;

pub(crate) struct Frequencies {
    sampled: Option<Sampled>,
}

struct Sampled {
    latin1: [u8; 256],
    /// By the high byte.
    pages: [u8; 256],
    weight_of_one: f32,
    wide: f32,
}

impl Frequencies {
    pub(crate) fn new(subject: Text<'_>) -> Frequencies {
        let sampled = (subject.len() >= LEAST_SAMPLE).then(|| {
            let start = subject.len().saturating_sub(SAMPLE) / 2;
            let sample = subject.slice(start..subject.len().min(start + SAMPLE));
            let (mut latin1, mut pages, mut wide) = ([0; 256], [0; 256], 0u8);
            with_text!(sample, |units| for unit in units {
                match unit.to_u16().to_be_bytes() {
                    [0, low] => latin1[usize::from(low)] += 1,
                    [high, _] => {
                        pages[usize::from(high)] += 1;
                        wide += 1;
                    }
                }
            });
            let weight_of_one = TRUST_IN_SAMPLE / sample.len() as f32;
            Sampled {
                latin1,
                pages,
                weight_of_one,
                wide: f32::from(wide) * weight_of_one,
            }
        });
        Frequencies { sampled }
    }

    fn of_character(&self, c: u8) -> f32 {
        let usual = ASCII.get(usize::from(c)).copied().unwrap_or(OTHER_LATIN1);
        let usual = f32::from(usual) * SCALE;
        match &self.sampled {
            None => usual,
            Some(sampled) => {
                usual * (1.0 - TRUST_IN_SAMPLE)
                    + f32::from(sampled.latin1[usize::from(c)]) * sampled.weight_of_one
            }
        }
    }

    pub(crate) fn of_latin1(&self, set: &Latin1Set) -> f32 {
        let sum = |set: Latin1Set| set.iter().map(|c| self.of_character(c)).sum::<f32>();
        if set.len() <= 128 {
            return sum(*set);
        }
        let all = 1.0 - self.sampled.as_ref().map_or(0.0, |sampled| sampled.wide);
        (all - sum(!*set)).max(0.0)
    }

    /// The range is above Latin-1.
    pub(crate) fn of_wide(&self, range: CharRange) -> f32 {
        // A script uses a part of its pages, and that unevenly.
        let share = |units: u32, of: u32| (units as f32 * 8.0 / of as f32).min(1.0);
        let last = range.last.min(0xffff);
        let usual = WIDE_WITHOUT_SAMPLE * share(last - range.first + 1, 0x4000);
        let Some(sampled) = &self.sampled else {
            return usual;
        };
        let in_page = |page: u32| {
            let units = last.min(page << 8 | 0xff) - range.first.max(page << 8) + 1;
            f32::from(sampled.pages[page as usize]) * share(units, 0x100)
        };
        let seen = (range.first >> 8..=last >> 8).map(in_page).sum::<f32>();
        usual * (1.0 - TRUST_IN_SAMPLE) + seen * sampled.weight_of_one
    }

    pub(crate) fn of_unit(&self, unit: u16) -> f32 {
        match u8::try_from(unit) {
            Ok(c) => self.of_character(c),
            Err(_) => self.of_wide(CharRange::single(unit.into())),
        }
    }
}
