//! Loops over characters.

use bun_engine_asm::prelude::*;
use bun_engine_regexp::program::{ClassId, Direction, Op, Place, Stride, UNBOUNDED};

use crate::class::{Branch, Test};
use crate::frame::{ORIGIN, POSITION};
use crate::generator::Generator;
use crate::vector::{Lanes, Slack, VectorTest, Vectors};

const UNITS_UNROLLED: u32 = 3;

/// Where a run has to stop.
#[derive(Clone, Copy)]
enum Limit {
    Constant(i64),
    Register(Gpr),
}

fn step(direction: Direction) -> i32 {
    match direction {
        Direction::Forward => 1,
        Direction::Backward => -1,
    }
}

/// The offset of the next unit.
fn next_unit(direction: Direction) -> i32 {
    match direction {
        Direction::Forward => 0,
        Direction::Backward => -1,
    }
}

impl Generator<'_> {
    /// Starts `advance` units from the position.
    pub(crate) fn run(&mut self, op: Op, advance: i32) {
        let Op::Run {
            direction,
            code_points,
            is_long,
            class,
            max,
            reserve,
        } = op
        else {
            unreachable!("the operation is a run")
        };
        if code_points {
            self.plus(POSITION, POSITION, advance.into());
            return self.run_of_code_points(direction, class, max, reserve);
        }
        let test = self.plan_for_units(class);
        if max <= UNITS_UNROLLED {
            self.plus(POSITION, POSITION, advance.into());
            return self.short_run(direction, test, max, reserve);
        }
        let done = self.masm.new_label();
        let is_worth_vectors = is_long && max == UNBOUNDED && direction == Direction::Forward;
        let plan = is_worth_vectors.then(|| self.plan_vector(class, Slack::MissesMembers));
        let mut advance = advance;
        if let Some((lanes, vector_test)) = plan.flatten() {
            self.plus(POSITION, POSITION, core::mem::take(&mut advance).into());
            self.run_of_vectors(lanes, vector_test, reserve, done);
        }
        // The loop starts with a step.
        let before_start = i64::from(advance) - i64::from(step(direction));
        self.plus(POSITION, POSITION, before_start);
        let limit = self.limit(direction, max, reserve);
        self.prepare_test(test);
        let again = self.masm.here();
        self.step_unless_at(limit, direction, done);
        let c = self.frame.temporary(0);
        self.load_unit(c, POSITION, next_unit(direction));
        self.holding_constant(|generator| generator.prepared_test(test, c, Branch::If(again)));
        self.masm.bind(done);
    }

    /// Falls through if the rest has to be done unit by unit.
    fn run_of_vectors(&mut self, lanes: Lanes, test: VectorTest, reserve: u32, done: Label) {
        let mut vectors = Vectors::new(lanes);
        if !vectors.reserve(&test) {
            return;
        }
        let (stopped, by_units) = (self.masm.new_label(), self.masm.new_label());
        let (end, is_exact) = (-i64::from(reserve), test.is_exact);
        let last = end - i64::from(lanes.units());
        self.masm.branch64(Cond::Gt, POSITION, last, by_units);
        vectors.load_constants(&mut self.masm);
        self.vector_loop(lanes, end, done, |generator| {
            let mask = generator.test_all(&vectors, &[(0, test)]);
            generator.branch_if_any(mask, false, stopped);
        });
        self.masm.bind(stopped);
        let lane = self.first_lane(lanes);
        self.masm.add64(POSITION, POSITION, lane);
        if is_exact {
            self.masm.jump(done);
        }
        self.masm.bind(by_units);
    }

    /// The position is one step before the start.
    fn limit(&mut self, direction: Direction, max: u32, reserve: u32) -> Limit {
        let (other, limit) = (self.frame.temporary(0), self.frame.temporary(1));
        let farthest = i64::from(max) + 1;
        match (direction, max) {
            (Direction::Forward, UNBOUNDED) => Limit::Constant(-i64::from(reserve)),
            (Direction::Backward, UNBOUNDED) => {
                Limit::Register(self.plus(limit, ORIGIN, reserve.into()))
            }
            (Direction::Forward, _) => {
                self.masm.add64(limit, POSITION, farthest);
                self.masm.move64(other, -i64::from(reserve));
                self.masm
                    .select64(Cond::Gt, limit, limit, other, other, limit);
                Limit::Register(limit)
            }
            (Direction::Backward, _) => {
                self.masm.sub64(limit, POSITION, farthest);
                let edge = self.plus(other, ORIGIN, reserve.into());
                self.masm
                    .select64(Cond::Lt, limit, limit, edge, edge, limit);
                Limit::Register(limit)
            }
        }
    }

    fn step_unless_at(&mut self, limit: Limit, direction: Direction, done: Label) {
        if let Limit::Constant(0) = limit {
            return (self.masm).branch_add64(ResultCond::Zero, POSITION, POSITION, 1, done);
        }
        self.masm.add64(POSITION, POSITION, step(direction));
        match limit {
            Limit::Constant(limit) => self.masm.branch64(Cond::Eq, POSITION, limit, done),
            Limit::Register(limit) => self.masm.branch64(Cond::Eq, POSITION, limit, done),
        }
    }

    fn short_run(&mut self, direction: Direction, test: Test, max: u32, reserve: u32) {
        let (c, done) = (self.frame.temporary(0), self.masm.new_label());
        for _ in 0..max {
            self.branch_on_room(POSITION, direction, reserve + 1, Branch::Unless(done));
            self.load_unit(c, POSITION, next_unit(direction));
            self.test_class(test, c, Branch::Unless(done));
            self.masm.add64(POSITION, POSITION, step(direction));
        }
        self.masm.bind(done);
    }

    fn run_of_code_points(&mut self, direction: Direction, class: ClassId, max: u32, reserve: u32) {
        let test = self.plan_for_code_points(class);
        let done = self.masm.new_label();
        let left = (max != UNBOUNDED).then(|| self.frame.temporary(3));
        if let Some(left) = left {
            self.masm.move32(left, Untrusted(max));
        }
        let again = self.masm.here();
        if let Some(left) = left {
            (self.masm).branch_sub32(ResultCond::Carry, left, left, 1, done);
        }
        self.branch_on_room(POSITION, direction, reserve + 1, Branch::Unless(done));
        self.code_point(direction, test, reserve, done);
        self.masm.jump(again);
        self.masm.bind(done);
    }

    pub(crate) fn class_count(&mut self, offset: i32, count: u32, class: ClassId, fail: Label) {
        let end = i64::from(offset) + i64::from(count);
        if let Ok(end) = i16::try_from(end) {
            return self.test_units_before(end.into(), count, class, fail);
        }
        self.masm.add64(POSITION, POSITION, end);
        let failed = self.defer(move |generator| {
            generator.masm.sub64(POSITION, POSITION, end);
            generator.masm.jump(fail);
        });
        self.test_units_before(0, count, class, failed);
        self.masm.sub64(POSITION, POSITION, end);
    }

    fn test_units_before(&mut self, end: i32, count: u32, class: ClassId, fail: Label) {
        let (c, left) = (self.frame.temporary(0), self.frame.temporary(1));
        let test = self.plan_for_units(class);
        self.masm.move64(left, -i64::from(count));
        self.prepare_test(test);
        let again = self.masm.here();
        self.masm.add64(c, POSITION, left);
        self.load_unit(c, c, end);
        self.holding_constant(|generator| generator.prepared_test(test, c, Branch::Unless(fail)));
        (self.masm).branch_add64(ResultCond::NonZero, left, left, 1, again);
    }

    pub(crate) fn give_back(
        &mut self,
        direction: Direction,
        stride: Stride,
        floor: Place,
        filter: Option<ClassId>,
        fail: Label,
    ) {
        let units = stride.least_units() as i32;
        let back = -step(direction);
        let too_far = match direction {
            Direction::Forward => Cond::Le,
            Direction::Backward => Cond::Ge,
        };
        // From here no stride is left.
        let distance = i64::from(floor.delta) - i64::from(back * (units - 1));
        let Some(filter) = filter else {
            self.compare_position(too_far, floor.slot, distance, fail);
            self.step_back(back * units);
            if stride.is_code_point() {
                let done = self.masm.new_label();
                self.compare_position(too_far, floor.slot, distance, done);
                self.step_out_of_pair(back);
                self.masm.bind(done);
            }
            return;
        };
        let (c, test) = (self.frame.temporary(0), self.plan_for_units(filter));
        let last = self.slot_plus(self.frame.temporary(1), floor.slot, distance);
        let found = self.masm.new_label();
        if direction == Direction::Forward && units == 1 && !stride.is_code_point() {
            self.give_back_vectors(filter, last, found);
        }
        self.prepare_test(test);
        let again = self.masm.here();
        self.masm.branch64(too_far, POSITION, last, fail);
        self.step_back(back * units);
        if stride.is_code_point() {
            let done = self.masm.new_label();
            self.masm.branch64(too_far, POSITION, last, done);
            self.step_out_of_pair(back);
            self.masm.bind(done);
        }
        self.load_unit(c, POSITION, next_unit(direction));
        self.holding_constant(|generator| generator.prepared_test(test, c, Branch::Unless(again)));
        self.masm.bind(found);
    }

    /// Falls through if the rest has to be done unit by unit.
    fn give_back_vectors(&mut self, filter: ClassId, floor: Gpr, found: Label) {
        let plan = self.plan_vector(filter, Slack::TakesOthers);
        let Some((lanes, test)) = plan.filter(|it| it.1.is_exact && it.1.only_compares()) else {
            return;
        };
        let mut vectors = Vectors::new(lanes);
        if !vectors.reserve(&test) {
            return;
        }
        let (left, units) = (self.frame.temporary(0), lanes.units());
        let (stopped, by_units) = (self.masm.new_label(), self.masm.new_label());
        self.masm.sub64(left, POSITION, floor);
        self.masm.branch64(Cond::Lt, left, units, by_units);
        vectors.load_constants(&mut self.masm);
        let again = self.masm.here();
        let mask = self.test_all(&vectors, &[(-units, test)]);
        self.step_back(-units);
        self.branch_if_any_passes(mask, stopped);
        self.masm.sub64(left, POSITION, floor);
        self.masm.branch64(Cond::Ge, left, units, again);
        self.masm.jump(by_units);
        self.masm.bind(stopped);
        let lane = self.last_lane(lanes);
        self.masm.add64(POSITION, POSITION, lane);
        self.masm.jump(found);
        self.masm.bind(by_units);
    }

    fn step_back(&mut self, distance: i32) {
        self.masm.add64(POSITION, POSITION, distance);
        if let Some(fuel) = self.frame.fuel {
            self.masm.sub64(fuel, fuel, distance.abs());
        }
    }
}
