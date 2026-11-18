//! Tests of units at known offsets, several at once where they can be.

use super::Emitter;
use super::output::{Label, Section};
use super::state::Fail;
use crate::Error;
use crate::compile::graph::Step;
use crate::compile::memory::push;
use crate::program::{Direction, Op, split_halves};

#[derive(Clone, Copy)]
pub(super) struct Test {
    offset: i32,
    step: Step,
}

impl Emitter<'_> {
    /// The offset of the first of the next `units` units in the direction.
    pub(super) fn offset_toward(&self, direction: Direction, units: u32) -> i32 {
        match direction {
            Direction::Forward => self.state.offset,
            Direction::Backward => self.state.offset - units as i32,
        }
    }

    pub(super) fn test(&mut self, offset: i32, step: Step, fail: Fail) -> Result<(), Error> {
        if fail != self.tests_fail {
            self.write_tests()?;
            self.tests_fail = fail;
        }
        push(&mut self.tests, Test { offset, step })
    }

    #[inline]
    pub(super) fn write_tests(&mut self) -> Result<(), Error> {
        match self.tests.is_empty() {
            true => Ok(()),
            false => self.write_some_tests(),
        }
    }

    fn write_some_tests(&mut self) -> Result<(), Error> {
        let fail = self.target(self.tests_fail)?;
        let mut tests = core::mem::take(&mut self.tests);
        tests.sort_by_key(|test| (matches!(test.step, Step::Class(_)), test.offset));
        let classes = tests.partition_point(|test| matches!(test.step, Step::Unit { .. }));
        for adjacent in tests[..classes].chunk_by(|a, b| a.offset + 1 == b.offset) {
            self.write_units(adjacent, fail)?;
        }
        for test in &tests[classes..] {
            if let Step::Class(class) = test.step {
                let op = Op::Class {
                    offset: test.offset as i16,
                    class,
                    fail,
                };
                self.output.op(Section::Main, op)?;
            }
        }
        tests.clear();
        self.tests = tests;
        Ok(())
    }

    /// Covers the units with few words, which can overlap.
    fn write_units(&mut self, tests: &[Test], fail: Label) -> Result<(), Error> {
        let width = self.graph.width;
        let most = 8 / width.bytes_per_unit() as usize;
        let passes_all =
            |test: &Test| matches!(test.step, Step::Unit { mask, .. } if mask == width.max_unit());
        let mut done = 0;
        while done < tests.len() {
            if passes_all(&tests[done]) {
                done += 1;
                continue;
            }
            let left = tests.len() - done;
            let covering = left.next_power_of_two();
            let (first, count) = if covering <= most && covering <= tests.len() {
                (tests.len() - covering, covering)
            } else {
                (done, most.min(1 << left.ilog2()))
            };
            self.write_word(&tests[first..first + count], fail)?;
            done = first + count;
        }
        Ok(())
    }

    fn write_word(&mut self, tests: &[Test], fail: Label) -> Result<(), Error> {
        let bytes_per_unit = self.graph.width.bytes_per_unit();
        let (mut value, mut mask) = (0u64, 0u64);
        for test in tests.iter().rev() {
            if let Step::Unit {
                value: unit_value,
                mask: unit_mask,
            } = test.step
            {
                value = value << (8 * bytes_per_unit) | u64::from(unit_value);
                mask = mask << (8 * bytes_per_unit) | u64::from(unit_mask);
            }
        }
        let offset = tests[0].offset as i16;
        let op = match tests.len() {
            1 => Op::Unit {
                offset,
                value: value as u16,
                mask: mask as u16,
                fail,
            },
            count => Op::Word {
                bytes: count as u8 * bytes_per_unit as u8,
                offset,
                value: split_halves(value),
                mask: split_halves(mask),
                fail,
            },
        };
        self.output.op(Section::Main, op)
    }
}
