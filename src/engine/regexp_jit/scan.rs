//! Looks for the start of a match.

use bun_engine_asm::prelude::*;
use bun_engine_regexp::program::Scan;
use smallvec::SmallVec;

use crate::class::Branch;
use crate::frame::POSITION;
use crate::generator::Generator;
use crate::vector::{Lanes, Slack, VectorTest, Vectors};

type Tests = SmallVec<[(i32, VectorTest); 4]>;

impl<'a> Generator<'a> {
    fn plan_scan(&self, scan: &Scan) -> Option<(Vectors, Tests)> {
        let probes = || core::iter::once(scan.first()).chain(scan.others().iter().copied());
        let plan = |class| self.plan_vector(class, Slack::TakesOthers);
        let lanes = match self.unit_size() {
            Size::S8 => Lanes::Bytes,
            _ if !scan.rows().is_empty() => Lanes::Narrowed,
            _ => plan(scan.first().class)?.0,
        };
        let (mut vectors, mut tests) = (Vectors::new(lanes), Tests::new());
        let mut add = |offset: i16, test: VectorTest| {
            if vectors.reserve(&test) {
                tests.push((offset.into(), test));
            }
        };
        if scan.rows().is_empty() {
            for (index, probe) in probes().enumerate() {
                let set = self.program.class(probe.class).set();
                match VectorTest::new(set, lanes, Slack::TakesOthers) {
                    Some(test) => add(probe.offset, test),
                    None if index == 0 => return None,
                    None => {}
                }
            }
        } else {
            for row in scan.rows() {
                add(row.offset, VectorTest::of_rows(row));
            }
        }
        Some((vectors, tests))
    }

    pub(crate) fn scan(&mut self, scan: &'a Scan, fail: Label) {
        let end = 1 - i64::from(scan.need());
        let Some((vectors, tests)) = self.plan_scan(scan) else {
            return self.scan_units(scan, fail);
        };
        let (found, done) = (self.masm.new_label(), self.masm.new_label());
        let start = self.masm.here();
        let few_are_left = self.defer(move |generator| {
            generator.scan_units(scan, fail);
            generator.masm.jump(done);
        });
        let last = end - i64::from(vectors.lanes.units());
        self.masm.branch64(Cond::Gt, POSITION, last, few_are_left);
        vectors.load_constants(&mut self.masm);
        self.vector_loop(vectors.lanes, end, fail, |generator| {
            let mask = match scan.rows().is_empty() {
                true => generator.test_all(&vectors, &tests),
                false => generator.test_rows(&vectors, &tests),
            };
            generator.branch_if_any(mask, true, found);
        });
        self.masm.bind(found);
        let lane = self.first_lane(vectors.lanes);
        self.masm.add64(POSITION, POSITION, lane);
        if scan.rows().is_empty() && !tests[0].1.is_exact {
            let mistaken = self.defer(move |generator| {
                generator.masm.add64(POSITION, POSITION, 1);
                generator.masm.jump(start);
            });
            let probe = scan.first();
            self.test_unit(probe.offset.into(), probe.class, Branch::Unless(mistaken));
        }
        self.masm.bind(done);
    }

    fn scan_units(&mut self, scan: &Scan, fail: Label) {
        let (probe, c) = (scan.first(), self.frame.temporary(0));
        let test = self.plan_for_units(probe.class);
        self.masm.sub64(POSITION, POSITION, 1);
        self.prepare_test(test);
        let again = self.masm.here();
        self.masm.add64(POSITION, POSITION, 1);
        (self.masm).branch64(Cond::Gt, POSITION, -i64::from(scan.need()), fail);
        self.load_unit(c, POSITION, probe.offset.into());
        self.holding_constant(|generator| generator.prepared_test(test, c, Branch::Unless(again)));
    }
}
