//! From the graph to operations.
//!
//! The code of a node falls through when it succeeds. What has to be done before the position, a cell or a capture
//! changes is put off until then, so that a failure among the first tests has nothing to undo.

mod alternation;
mod leaf;
mod look;
mod output;
mod repeat;
mod run;
mod search;
mod stacked;
mod state;
mod stepped;
mod tests;

use super::graph::{Graph, Id, Kind, Span, Traits};
use super::memory::{StackLimit, push};
use crate::Error;
use crate::analysis::Frequencies;
use crate::program::{Direction, Op, Pairs, Place, Probe, ScopeKind, Slot, UNBOUNDED};
use output::Output;
use search::FirstRun;
pub(super) use search::emit;
use state::{Action, Alternative, Fail, Spot, State, Undo};
use tests::Test;

struct Emitter<'a> {
    graph: &'a mut Graph,
    output: Output,
    state: State,
    direction: Direction,
    actions: Vec<Action>,
    alternatives: Vec<Alternative>,
    /// Not yet written. They all fail to `tests_fail`.
    tests: Vec<Test>,
    tests_fail: Fail,
    epochs: u32,
    match_start: Spot,
    /// To be cleared when the match is accepted.
    owed: Vec<Pairs>,
    turn: Turn,
    /// For the next sequence.
    captures: Captures,
    held: Vec<Spot>,
    /// How often the code at hand is expected to run.
    weight: u32,
    /// Whether the code at hand can run more than once on the same part of the subject.
    repeats: bool,
    /// The most units that can be passed after the node at hand.
    following: u32,
    frequencies: Frequencies,
    first_run: Option<FirstRun>,
    /// In which the position is the start of the subject.
    epoch_at_start: Option<u32>,
    /// A test that the search has done, and the epoch in which that holds.
    scanned: Option<(Probe, u32)>,
    stack: StackLimit,
}

/// What becomes of the groups whose parentheses are terms of a sequence.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Captures {
    Written,
    /// Nothing is written before the group ends.
    Held,
    Skipped,
}

/// The innermost turn of a loop.
#[derive(Clone, Copy)]
struct Turn {
    start: Spot,
    /// The turn can be empty while the number in the slot is below the value.
    required: Option<(Slot, u32)>,
    undo: Option<Undo>,
}

impl Emitter<'_> {
    /// At least the minimum of the node and `rest` more units are known to follow. Returns where later failures go.
    fn node(&mut self, id: Id, rest: u32, fail: Fail) -> Result<Fail, Error> {
        self.stack.check()?;
        if !self.state.is_reachable {
            return Ok(fail);
        }
        let node = &self.graph[id];
        let (kind, traits) = (node.kind, node.traits);
        let entry = self.entry();
        let mark = match traits.contains(Traits::CUTS) {
            true => Some(self.plan_mark()?),
            false => None,
        };
        let back = match kind {
            Kind::Sequence(children) => {
                self.sequence(children, traits.contains(Traits::RIGID), rest, fail)?
            }
            Kind::Alternation(children) => self.alternation(id, children, rest, fail)?,
            Kind::Repeat { .. } => self.repeat(id, rest, fail)?,
            Kind::Turn(body) => self.turn(id, body, rest, fail)?,
            Kind::Look {
                body,
                direction,
                negated,
            } => self.look(body, direction, negated, fail)?,
            Kind::Run { .. } => self.run(id, rest, fail)?,
            _ => return self.leaf(id, rest, fail).map(|()| fail),
        };
        if traits.contains(Traits::REENTERED) || self.first_run.is_some_and(|it| it.node == id) {
            return Ok(back);
        }
        if let Some(slot) = mark
            && self.state.is_reachable
            && !self.is_unchanged_since(entry)
        {
            self.commit()?;
            self.op(Op::Cut { slot })?;
        }
        self.release(entry)?;
        Ok(fail)
    }

    fn sequence(
        &mut self,
        children: Span,
        is_rigid: bool,
        rest: u32,
        mut fail: Fail,
    ) -> Result<Fail, Error> {
        let captures = core::mem::replace(&mut self.captures, Captures::Written);
        let mins = self.graph.children_of(children).iter();
        let mut needed = mins.fold(rest, |sum, &child| sum + self.graph[child].min);
        let following = self.following;
        let (mut bounded, mut unbounded) = (u64::from(following), 0);
        for &child in self.graph.children_of(children) {
            match self.graph[child].max {
                UNBOUNDED => unbounded += 1,
                max => bounded += u64::from(max),
            }
        }
        for index in children.range() {
            let child = self.graph.children[index];
            needed -= self.graph[child].min;
            match self.graph[child].max {
                UNBOUNDED => unbounded -= 1,
                max => bounded -= u64::from(max),
            }
            self.following = match unbounded {
                0 => u32::try_from(bounded).unwrap_or(UNBOUNDED),
                _ => UNBOUNDED,
            };
            if !is_rigid {
                self.keep_offsets_short()?;
            }
            match (captures, self.graph[child].kind) {
                (Captures::Skipped, Kind::Open(_) | Kind::Close(_)) => {}
                (Captures::Held, Kind::Open(_)) => self.hold_capture()?,
                (Captures::Held, Kind::Close(pair)) => self.write_held_capture(pair)?,
                _ => fail = self.node(child, needed, fail)?,
            }
        }
        self.following = following;
        Ok(fail)
    }

    fn turn(&mut self, id: Id, body: Id, rest: u32, fail: Fail) -> Result<Fail, Error> {
        let outer = (self.turn.start, self.turn.required);
        self.turn.start = self.spot()?;
        if self.graph[id].min != 0 {
            self.turn.required = None;
        }
        let back = self.node(body, rest, fail)?;
        (self.turn.start, self.turn.required) = outer;
        Ok(back)
    }

    fn plan_mark(&mut self) -> Result<Slot, Error> {
        let slot = self.plan_scope(1, ScopeKind::Mark)?;
        push(&mut self.actions, Action::Protect(Op::Mark { slot }))?;
        Ok(slot)
    }

    fn weigh(&mut self, slot: Slot) {
        let weight = &mut self.output.cell_weights[slot.cell as usize];
        *weight = weight.saturating_add(self.weight);
    }

    fn weigh_place(&mut self, place: Place) -> Place {
        self.weigh(place.slot);
        place
    }
}
