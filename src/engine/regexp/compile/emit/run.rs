//! Repeated characters.

use super::Emitter;
use super::output::{Label, Section};
use super::state::Fail;
use crate::Error;
use crate::compile::graph::{Id, Kind, Traits};
use crate::program::{ClassId, Comparison, Direction, Op, Place, Stride, UNBOUNDED};
use crate::set::CharRange;

impl Emitter<'_> {
    #[inline(never)]
    pub(super) fn run(&mut self, id: Id, rest: u32, fail: Fail) -> Result<Fail, Error> {
        let node = self.graph[id];
        let Kind::Run {
            class,
            max,
            code_points,
            greedy,
            filter,
        } = node.kind
        else {
            unreachable!("the node is a run")
        };
        if !greedy {
            return self.lazy_run(class, max, code_points, filter, rest, fail);
        }
        if !node.is(Traits::REENTERED) {
            self.settle()?;
            self.pass(class, max, code_points, rest)?;
            return Ok(fail);
        }
        let floor = self.settle_at_anchor()?;
        self.pass(class, max, code_points, rest)?;
        let again = self.output.label()?;
        self.bind_after_giving_back(again, floor, rest)?;
        let stride = if code_points {
            Stride::CODE_POINT
        } else {
            Stride::UNIT
        };
        self.give_back(again, floor, stride, filter, fail)
    }

    /// Returns a place that is equal to the position.
    pub(super) fn settle_at_anchor(&mut self) -> Result<Place, Error> {
        let (anchor, offset) = (self.anchor(self.state.room)?, self.state.offset);
        self.settle()?;
        Ok(Place {
            delta: anchor.delta + offset,
            ..anchor
        })
    }

    pub(super) fn pass(
        &mut self,
        class: ClassId,
        max: u32,
        code_points: bool,
        rest: u32,
    ) -> Result<(), Error> {
        self.op(Op::Run {
            direction: self.direction,
            code_points,
            class,
            max,
            reserve: rest,
        })?;
        self.moved(0, rest);
        Ok(())
    }

    pub(super) fn bind_after_giving_back(
        &mut self,
        again: Label,
        floor: Place,
        rest: u32,
    ) -> Result<(), Error> {
        let at_floor = self.room_at(floor).unwrap_or_default();
        self.bind_settled(
            again,
            at_floor.with(self.direction, rest),
            self.state.planned,
        )
    }

    /// Later failures move back toward the floor and go on at `again`.
    pub(super) fn give_back(
        &mut self,
        again: Label,
        floor: Place,
        stride: Stride,
        filter: Option<ClassId>,
        fail: Fail,
    ) -> Result<Fail, Error> {
        let op = Op::GiveBack {
            direction: self.direction,
            stride,
            floor: self.weigh_place(floor),
            filter: filter.into(),
            fail: self.later_target(fail)?,
        };
        let (direct, later) = self.bind_retry()?;
        self.output.op(Section::Cold, op)?;
        self.output.op(Section::Cold, Op::Jump { to: again })?;
        self.repeats = true;
        self.fail_to_either(direct, later)
    }

    /// Starts cold code that failures come to with the present position.
    pub(super) fn bind_retry(&mut self) -> Result<(Label, Label), Error> {
        let (direct, later) = (self.output.label()?, self.output.label()?);
        let (room, scope) = (self.state.room, self.state.scope);
        let undo = self.plan_undo(true)?;
        self.write_undo(later, Section::Cold, undo)?;
        self.output.bind(direct, Section::Cold, room, scope);
        Ok((direct, later))
    }

    /// Goes to `to` if `units` have been passed since `start`.
    pub(super) fn compare_distance(
        &mut self,
        start: Place,
        units: u32,
        has_passed: bool,
        to: Label,
    ) -> Option<Op> {
        let distance = self.direction.signed(i32::try_from(units).ok()?);
        Some(Op::ComparePlace {
            when: match (self.direction, has_passed) {
                (Direction::Forward, true) => Comparison::AtLeast,
                (Direction::Forward, false) => Comparison::Below,
                (Direction::Backward, true) => Comparison::AtMost,
                (Direction::Backward, false) => Comparison::Above,
            },
            offset: 0,
            place: self.weigh_place(Place {
                delta: start.delta.checked_add(distance)?,
                ..start
            }),
            to,
        })
    }

    fn lazy_run(
        &mut self,
        class: ClassId,
        max: u32,
        code_points: bool,
        passed_at_once: Option<ClassId>,
        rest: u32,
        fail: Fail,
    ) -> Result<Fail, Error> {
        let start = match max {
            UNBOUNDED => {
                self.settle()?;
                None
            }
            _ => Some(self.settle_at_anchor()?),
        };
        let exhausted = self.later_target(fail)?;
        let (again, go_on) = (self.output.label()?, self.output.label()?);
        let (room, scope) = (
            self.state.room.with(self.direction, rest),
            self.state.planned,
        );
        self.bind_settled(again, room, scope)?;
        if let Some(class) = passed_at_once {
            self.pass(class, UNBOUNDED, code_points, rest)?;
        }
        self.bind_settled(go_on, room, scope)?;

        let (direct, later) = self.bind_retry()?;
        if let Some(op) = start.and_then(|start| self.compare_distance(start, max, true, exhausted))
        {
            self.output.op(Section::Cold, op)?;
        }
        self.write_cold_step(class, code_points, rest, exhausted)?;
        self.output.op(Section::Cold, Op::Jump { to: again })?;
        self.repeats = true;
        self.fail_to_either(direct, later)
    }

    fn write_cold_step(
        &mut self,
        class: ClassId,
        code_points: bool,
        rest: u32,
        fail: Label,
    ) -> Result<(), Error> {
        let direction = self.direction;
        let mut cold = |op| self.output.op(Section::Cold, op);
        cold(Op::Need {
            direction,
            count: rest + 1,
            fail,
        })?;
        if code_points {
            return cold(Op::CodePoint {
                direction,
                class,
                reserve: rest,
                fail,
            });
        }
        let all_units = CharRange::new(0, self.graph.width.max_unit().into());
        if !self.graph[class].contains_range(all_units) {
            cold(Op::Class {
                offset: if direction == Direction::Forward {
                    0
                } else {
                    -1
                },
                class,
                fail,
            })?;
        }
        cold(Op::Advance {
            by: direction.signed(1),
        })
    }
}
