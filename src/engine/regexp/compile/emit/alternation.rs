//! Alternatives.

use smallvec::SmallVec;

use super::Emitter;
use super::output::{Label, Section};
use super::state::Fail;
use crate::Error;
use crate::compile::graph::{Id, Kind, Span, Traits};
use crate::compile::memory::{push, reserve};
use crate::program::{Op, Pairs, Room, ScopeKind, Slot};

/// Beyond this it costs too much code to do what has been put off in each alternative.
const ACTIONS_REPEATED: usize = 32;

impl Emitter<'_> {
    pub(super) fn alternation(
        &mut self,
        id: Id,
        children: Span,
        rest: u32,
        fail: Fail,
    ) -> Result<Fail, Error> {
        let node = self.graph[id];
        let is_rigid = node.is(Traits::RIGID);
        if !is_rigid && self.pending_actions() * children.len() > ACTIONS_REPEATED {
            self.commit()?;
        }
        let metered = self.is_costly(id) || node.is(Traits::REENTERED);
        let (_, tried_first) = self
            .graph
            .children_of(children)
            .split_last()
            .expect("an alternative");
        let undo = match is_rigid || tried_first.iter().all(|&it| self.rules_out_later(it)) {
            true => None,
            false => Some(self.plan_undo(metered)?),
        };
        let reserved = match node.is(Traits::REENTERED) {
            true => Some(self.plan_scope(1, ScopeKind::Reserved)?),
            false => None,
        };
        let before = self.snapshot()?;
        let joined = self.output.label()?;
        let mut ends = Ends::default();

        for index in children.range() {
            let child = self.graph.children[index];
            let is_last = index + 1 == children.range().end;
            let (next, tries_next) = match is_last {
                true => (joined, fail),
                false => {
                    let next = self.output.label()?;
                    let tries_next = match undo {
                        _ if !is_rigid && self.rules_out_later(child) => {
                            let later = self.later_target(fail)?;
                            self.fail_to_either(next, later)?
                        }
                        Some(undo) => self.fail_to(next, undo)?,
                        None => self.fail_directly_to(next)?,
                    };
                    (next, tries_next)
                }
            };
            let back = self.alternative(child, node.pairs, rest, tries_next)?;
            if self.state.is_reachable {
                if !is_rigid {
                    self.settle()?;
                }
                if let Some(reserved) = reserved {
                    let goes_on = self.graph[child]
                        .traits
                        .intersects(Traits::REENTERED | Traits::CONTINUED);
                    let target = self.later_target(if goes_on { back } else { fail })?;
                    self.choose(&mut ends, reserved, target, child)?;
                }
                ends.room = Some(
                    ends.room
                        .map_or(self.state.room, |it| it.min(self.state.room)),
                );
                ends.offset = self.state.offset;
            }
            if !is_last {
                self.jump(joined)?;
                self.restore(&before)?;
                self.bind(next)?;
            }
        }

        let Some(room) = ends.room else {
            return Ok(fail);
        };
        if is_rigid {
            self.restore(&before)?;
            self.state.room = room;
            self.state.offset = ends.offset;
            self.bind(joined)?;
            return Ok(fail);
        }
        let Some(reserved) = reserved else {
            self.bind_settled(joined, room, before.state.planned)?;
            return Ok(fail);
        };
        let slot = self.finish_choice(&ends, reserved)?;
        self.bind_settled(joined, room, slot.scope)?;
        let resume = self.output.label()?;
        self.output
            .bind(resume, Section::Cold, Room::NONE, slot.scope);
        self.output.op(Section::Cold, Op::Resume { slot })?;
        self.repeats = true;
        self.fail_to_either(resume, resume)
    }

    /// Whether a failure after the first tests of the alternative is one of all that follow it.
    fn rules_out_later(&self, alternative: Id) -> bool {
        let terms = self.graph.terms(&alternative).iter();
        let first = terms
            .map(|&it| &self.graph[it])
            .find(|it| !it.is_bookkeeping());
        self.graph[alternative].is(Traits::EXCLUSIVE)
            && first.is_some_and(|it| matches!(it.kind, Kind::Steps(_)) && it.is(Traits::RIGID))
    }

    fn alternative(&mut self, id: Id, all: Pairs, rest: u32, fail: Fail) -> Result<Fail, Error> {
        let node = &self.graph[id];
        let (own, min) = (node.pairs, node.min);
        let others = match own.is_empty() {
            true => [all, Pairs::NONE],
            false => [
                Pairs {
                    first: all.first,
                    end: own.first,
                },
                Pairs {
                    first: own.end,
                    end: all.end,
                },
            ],
        };
        let owed = self.owed.len();
        for pairs in others.into_iter().filter(|it| !it.is_empty()) {
            push(&mut self.owed, pairs)?;
        }
        self.need(min.saturating_add(rest), fail)?;
        let back = self.node(id, rest, fail)?;
        while self.owed.len() > owed
            && let Some(pairs) = self.owed.pop()
        {
            self.record(Op::Clear { pairs })?;
        }
        Ok(back)
    }

    #[inline(never)]
    fn choose(
        &mut self,
        ends: &mut Ends,
        reserved: Slot,
        target: Label,
        child: Id,
    ) -> Result<(), Error> {
        const COMPARED: usize = 8;
        let known = ends
            .targets
            .iter()
            .take(COMPARED)
            .position(|&it| it == target);
        let index = known.unwrap_or(ends.targets.len());
        if known.is_none() {
            ends.targets
                .try_reserve(1)
                .map_err(|_| Error::OutOfMemory)?;
            ends.targets.push(target);
        }
        ends.height = ends.height.max(self.output.height(self.state.planned));
        ends.has_frames |= self.graph[child].is(Traits::FRAMES);
        ends.choosing
            .try_reserve(1)
            .map_err(|_| Error::OutOfMemory)?;
        ends.choosing.push(self.output.main_length());
        self.weigh(reserved);
        self.op(Op::Choose {
            slot: reserved,
            index: index as u32,
            target,
        })
    }

    #[inline(never)]
    fn finish_choice(&mut self, ends: &Ends, reserved: Slot) -> Result<Slot, Error> {
        let targets = &mut self.output.choice_targets;
        let first_target = targets.len() as u32;
        reserve(targets, ends.targets.len())?;
        targets.extend_from_slice(&ends.targets);
        let kind = ScopeKind::Choice {
            first_target,
            end_target: targets.len() as u32,
            has_frames: ends.has_frames,
        };
        let slot = Slot {
            scope: self
                .output
                .add_scope(reserved.scope, ends.height, kind)?
                .scope,
            ..reserved
        };
        for &index in &ends.choosing {
            self.output.name_scope_of_choice(index, slot);
        }
        Ok(slot)
    }
}

/// What the alternatives that have succeeded leave.
#[derive(Default)]
struct Ends {
    room: Option<Room>,
    offset: i32,
    height: u32,
    has_frames: bool,
    targets: SmallVec<[Label; 2]>,
    /// The operations that select a target.
    choosing: SmallVec<[u32; 2]>,
}
