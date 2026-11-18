//! Repeated groups that keep a frame for each turn.

use super::Emitter;
use super::output::{Label, Section, between_turns_of};
use super::repeat::{Count, Repeat};
use super::state::{Fail, Undo};
use crate::Error;
use crate::compile::memory::push;
use crate::program::{Loop, LoopId, Op, Room, ScopeKind};

#[derive(Clone, Copy)]
struct Labels {
    turn: Label,
    pop: Label,
    retreat: Label,
    /// Between turns, where the loop can end.
    done: Label,
}

impl Emitter<'_> {
    #[inline(never)]
    fn enter_stacked(&mut self, repeat: Repeat) -> Result<(LoopId, Labels), Error> {
        self.settle()?;
        let id = LoopId(self.output.loops.len() as u32);
        let turns = self.plan_scope(1, ScopeKind::Turns(id))?;
        let in_turn = self.plan_scope(0, ScopeKind::InTurn(id))?.scope;
        let labels = Labels {
            turn: self.output.label()?,
            pop: self.output.label()?,
            retreat: self.output.label()?,
            done: self.output.label()?,
        };
        let it = Loop {
            turns,
            in_turn,
            between_turns: between_turns_of(id),
            end_cell: turns.cell + 1,
            pairs: repeat.pairs,
            room: Room::NONE,
            back_into_turn: labels.pop,
        };
        push(&mut self.output.loops, it)?;
        self.op(Op::Enter { id })?;
        Ok((id, labels))
    }

    fn turns(&self, id: LoopId) -> Count {
        Count::Number(self.output.loops[id.index()].turns)
    }

    #[inline(never)]
    fn start_turn(
        &mut self,
        id: LoopId,
        repeat: Repeat,
        no_turn: Label,
        pop: Label,
    ) -> Result<Fail, Error> {
        let in_turn = self.output.loops[id.index()].in_turn;
        self.state.planned = in_turn;
        self.protect_turn(Op::Push { id }, Undo::pop(id, in_turn))?;
        let turn_fails = self.fail_to_either(no_turn, pop)?;
        self.need_turn(repeat, turn_fails)?;
        self.output.loops[id.index()].room = self.state.room;
        Ok(turn_fails)
    }

    #[inline(never)]
    fn end_turn(&mut self, id: LoopId, back: Fail, to: Label) -> Result<(), Error> {
        if self.state.is_reachable {
            self.settle()?;
            let back_into_turn = self.later_target(back)?;
            let it = &mut self.output.loops[id.index()];
            it.back_into_turn = back_into_turn;
            it.end_cell = self.output.scopes[self.state.planned.index()].height;
            self.op(Op::Turned { id, to })?;
            self.state.is_reachable = false;
        }
        let it = self.output.loops[id.index()];
        let kind = ScopeKind::BetweenTurns(id);
        let between_turns = self.output.add_scope(it.turns.scope, it.end_cell, kind)?;
        self.output.loops[id.index()].between_turns = between_turns.scope;
        for cell in it.turns.cell..it.end_cell.min(it.turns.cell + 4) {
            self.weigh(crate::program::Slot { cell, ..it.turns });
        }
        Ok(())
    }

    fn write_pop(&mut self, id: LoopId, pop: Label, section: Section) -> Result<(), Error> {
        let in_turn = self.output.loops[id.index()].in_turn;
        self.write_undo(pop, section, Undo::pop(id, in_turn))
    }

    fn write_retreat(&mut self, id: LoopId, retreat: Label, none: Label) -> Result<(), Error> {
        self.output
            .bind(retreat, Section::Cold, Room::NONE, between_turns_of(id));
        self.output.op(Section::Cold, Op::Retreat { id, none })
    }

    /// One turn, which goes on at `then`.
    fn stacked_turn(
        &mut self,
        id: LoopId,
        repeat: Repeat,
        no_turn: Label,
        pop: Label,
        then: Label,
    ) -> Result<(), Error> {
        let turns = self.output.loops[id.index()].turns;
        let required = (repeat.min != 0).then(|| (turns, repeat.min.saturating_add(1)));
        let outside = self.enter_loop(required);
        let turn_fails = self.start_turn(id, repeat, no_turn, pop)?;
        let back = self.node(repeat.body, repeat.rest, turn_fails)?;
        self.end_turn(id, back, then)?;
        self.leave_loop(&outside);
        Ok(())
    }

    pub(super) fn stacked_greedy(&mut self, repeat: Repeat, fail: Fail) -> Result<Fail, Error> {
        let (id, labels) = self.enter_stacked(repeat)?;
        let room = self.room_between_turns(repeat);
        self.bind_between_turns(labels.turn, room, id)?;
        self.branch_on_count(self.turns(id), repeat.max, true, labels.done)?;
        self.stacked_turn(id, repeat, labels.done, labels.pop, labels.turn)?;
        self.write_pop(id, labels.pop, Section::Main)?;
        let between_turns = self.output.loops[id.index()].between_turns;
        self.bind_settled(labels.done, room, between_turns)?;
        self.branch_on_count(self.turns(id), repeat.min, false, labels.retreat)?;
        let none = self.later_target(fail)?;
        self.write_retreat(id, labels.retreat, none)?;
        self.fail_to_either(labels.retreat, labels.retreat)
    }

    pub(super) fn stacked_lazy(&mut self, repeat: Repeat, fail: Fail) -> Result<Fail, Error> {
        let (id, labels) = self.enter_stacked(repeat)?;
        let room = self.room_between_turns(repeat);
        if repeat.min == 0 {
            self.jump(labels.done)?;
        }
        self.bind_between_turns(labels.turn, room, id)?;
        self.stacked_turn(id, repeat, labels.retreat, labels.pop, labels.done)?;
        let between_turns = self.output.loops[id.index()].between_turns;
        self.bind_settled(labels.done, room, between_turns)?;
        if repeat.min > 1 {
            self.branch_on_count(self.turns(id), repeat.min, false, labels.turn)?;
        }
        let none = self.later_target(fail)?;
        self.write_pop(id, labels.pop, Section::Cold)?;
        self.write_retreat(id, labels.retreat, none)?;
        let (direct, later) = self.bind_retry()?;
        if let Some(op) = self.compare_count(self.turns(id), repeat.max, true, labels.retreat) {
            self.output.op(Section::Cold, op)?;
        }
        self.output
            .op(Section::Cold, Op::Jump { to: labels.turn })?;
        self.fail_to_either(direct, later)
    }

    fn bind_between_turns(&mut self, label: Label, room: Room, id: LoopId) -> Result<(), Error> {
        self.bind_settled(label, room, self.state.planned)?;
        self.state.scope = between_turns_of(id);
        self.bind(label)
    }
}
