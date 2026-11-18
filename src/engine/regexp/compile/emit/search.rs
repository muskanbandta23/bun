//! The attempts at one position after the other.

use super::output::{Label, Output, ROOT, Section};
use super::state::{Fail, Spot, State};
use super::{Captures, Emitter, Turn};
use crate::Error;
use crate::compile::Mode;
use crate::compile::Options;
use crate::analysis::{Frequencies, Prefixes, plan_scan};
use crate::compile::graph::{Form, Id, Kind, MAX_REACH, Node, Traits};
use crate::compile::lower::Lowered;
use crate::compile::memory::{StackLimit, push};
use crate::program::{
    Class, Direction, MAX_LENGTH, Op, Place, Probe, Program, Room, Scan, ScanId, ScopeKind,
    UNBOUNDED, Width,
};
use crate::surrogate::TRAILS;
use crate::tree::Tree;

pub(in crate::compile) fn emit(
    tree: &Tree,
    lowered: Lowered,
    options: &Options<'_>,
) -> Result<Program, Error> {
    let Lowered {
        mut graph,
        root,
        pair_count,
    } = lowered;
    let mut emitter = Emitter {
        output: Output::new(graph.node_count() * 2)?,
        graph: &mut graph,
        state: State::INITIAL,
        direction: Direction::Forward,
        actions: Vec::new(),
        alternatives: Vec::new(),
        tests: Vec::new(),
        tests_fail: Fail::NO_MATCH,
        epochs: 0,
        match_start: Spot::NOWHERE,
        owed: Vec::new(),
        turn: Turn {
            start: Spot::NOWHERE,
            required: None,
            undo: None,
        },
        captures: Captures::Written,
        held: Vec::new(),
        weight: 1,
        repeats: false,
        following: 0,
        frequencies: Frequencies::new(options.sample),
        first_run: None,
        epoch_at_start: None,
        scanned: None,
        stack: StackLimit(options.stack_limit),
    };
    let reads_pairs = tree.is_unicode_aware() && options.width == Width::Utf16;
    emitter.search(root, tree.is_sticky(), reads_pairs)?;
    let mut output = emitter.output;
    let (ops, joins) = output.lay_out()?;
    let min_length = graph[root].min;
    let mut classes = Vec::new();
    crate::compile::memory::reserve(&mut classes, graph.classes.len())?;
    classes.extend(graph.classes.into_iter().map(Class::new));
    Ok(Program {
        ops,
        joins,
        scopes: output.scopes,
        loops: output.loops,
        choice_targets: output.choice_targets,
        switches: output.switches,
        scans: output.scans,
        classes,
        referenced_pairs: graph.referenced_pairs,
        cell_weights: output.cell_weights,
        pair_count,
        reports_captures: options.mode == Mode::Captures,
        width: options.width,
        is_unicode_aware: tree.is_unicode_aware(),
        min_length,
    })
}

/// The run that every attempt enters at the same distance from its start, before it has had a choice.
///
/// Once the rest of the pattern has failed wherever the run can end, it fails for all that enter the run further on.
#[derive(Clone, Copy)]
pub(super) struct FirstRun {
    pub(super) node: Id,
    /// Expects the position to be the end of the run.
    pub(super) restart: Label,
}

#[derive(Clone, Copy)]
struct Labels {
    give_up: Label,
    /// One unit further.
    next: Label,
    search: Label,
    attempt: Label,
}

impl Emitter<'_> {
    fn search(&mut self, root: Id, is_sticky: bool, reads_pairs: bool) -> Result<(), Error> {
        let give_up = self.output.label()?;
        let no_match = self.fail_to_either(give_up, give_up)?;
        if is_sticky {
            self.leave_pair(reads_pairs)?;
            self.need(self.graph[root].min, no_match)?;
            self.attempt(root, no_match)?;
        } else {
            let prefixes = Prefixes::new(self.graph, root);
            if prefixes.rows.is_empty() && prefixes.longest_at_end.is_none() {
                self.attempt_at_start(root, reads_pairs, no_match)?;
            } else {
                self.leave_pair(reads_pairs)?;
                self.attempt_everywhere(root, &prefixes, reads_pairs, give_up)?;
            }
        }
        self.bind_settled(give_up, Room::NONE, ROOT)?;
        self.op(Op::NoMatch)
    }

    fn leave_pair(&mut self, reads_pairs: bool) -> Result<(), Error> {
        match reads_pairs {
            true => self.op(Op::LeavePair),
            false => Ok(()),
        }
    }

    fn attempt(&mut self, root: Id, fail: Fail) -> Result<(), Error> {
        self.match_start = self.spot()?;
        self.node(root, 0, fail).map(drop)
    }

    fn attempt_at_start(&mut self, root: Id, reads_pairs: bool, no_match: Fail) -> Result<(), Error> {
        self.leave_pair(reads_pairs)?;
        self.need(self.graph[root].min, no_match)?;
        let slot = self.plan_scope(1, ScopeKind::Position(self.state.room))?;
        let fail = self.target(no_match)?;
        self.op(Op::SaveStart { slot, fail })?;
        self.state.scope = slot.scope;
        self.state.anchor = Some(Place { slot, delta: 0 });
        self.epoch_at_start = Some(self.state.epoch);
        self.attempt(root, no_match)
    }

    fn attempt_everywhere(
        &mut self,
        root: Id,
        prefixes: &Prefixes,
        reads_pairs: bool,
        give_up: Label,
    ) -> Result<(), Error> {
        let labels = Labels {
            give_up,
            next: self.output.label()?,
            search: self.output.label()?,
            attempt: self.output.label()?,
        };
        let Node { min, max, .. } = self.graph[root];
        let room = Room::NONE.with(Direction::Forward, min);
        let is_near_end = max <= MAX_LENGTH && self.ends_at_end(root, 0);
        if is_near_end {
            self.op(Op::SkipToTail { length: max })?;
        }
        let scan = match is_near_end && max - min < POSITIONS_WORTH_A_SCAN {
            true => None,
            false => plan_scan(self.graph, prefixes, &self.frequencies)?,
        };
        let looks_behind = scan.as_ref().is_some_and(|it| it.offsets().any(|it| it < 0));
        let skips = scan.is_some() || prefixes.rows.is_empty();
        let unscanned = self.output.label()?;
        let has_unscanned_start = skips && (prefixes.some_are_at_start || looks_behind);
        if has_unscanned_start {
            self.op(Op::Need {
                direction: Direction::Backward,
                count: 1,
                fail: unscanned,
            })?;
        }
        self.jump(labels.search)?;

        self.bind_settled(labels.next, room, ROOT)?;
        self.need(1, Fail::NO_MATCH)?;
        self.advance(1)?;
        let behind = Room::NONE.with(Direction::Backward, has_unscanned_start.into());
        self.bind_settled(labels.search, behind, ROOT)?;
        let probe = match scan {
            Some(scan) => Some(self.scan(scan, min, prefixes.longest_at_end, unscanned, give_up)?),
            None => {
                if let Some(length) = prefixes.longest_at_end.filter(|_| skips) {
                    self.op(Op::SkipToTail { length })?;
                }
                self.need(min, Fail::NO_MATCH)?;
                None
            }
        };
        self.output.bind(unscanned, Section::Cold, Room::NONE, ROOT);
        let check = Op::Need {
            direction: Direction::Forward,
            count: min,
            fail: give_up,
        };
        self.output.op(Section::Cold, check)?;
        self.output.op(Section::Cold, Op::Jump { to: labels.attempt })?;

        let always_scans = !has_unscanned_start && prefixes.longest_at_end.is_none();
        match probe.filter(|_| always_scans) {
            Some(probe) => {
                self.bind(labels.attempt)?;
                self.scanned = Some((probe, self.state.epoch));
            }
            None => self.bind_settled(labels.attempt, room, ROOT)?,
        }
        self.repeats = true;
        self.find_first_run(root, labels)?;
        let is_costly = max > MAX_REACH || self.graph.node_count() > NODES_OF_CHEAP_ATTEMPT;
        let undo = self.plan_undo(is_costly)?;
        let attempt_fails = self.fail_to(labels.next, undo)?;
        if reads_pairs && self.graph.first(root).intersects_wide(TRAILS) {
            let fail = labels.next;
            self.op(Op::NotInPair { offset: 0, fail })?;
        }
        self.attempt(root, attempt_fails)
    }

    /// Returns the probe that holds afterwards.
    fn scan(
        &mut self,
        mut scan: Scan,
        min: u32,
        longest_at_end: Option<u32>,
        unscanned: Label,
        give_up: Label,
    ) -> Result<Probe, Error> {
        scan.need = scan.need.max(min);
        let mut fail = give_up;
        if let Some(length) = longest_at_end {
            scan.need = scan.need.max(length + 1);
            fail = self.output.label()?;
            self.output.bind(fail, Section::Cold, Room::NONE, ROOT);
            self.output.op(Section::Cold, Op::SkipToTail { length })?;
            self.output.op(Section::Cold, Op::Jump { to: unscanned })?;
        }
        let (id, first) = (ScanId(self.output.scans.len() as u32), scan.first);
        self.state.room.ahead = scan.need;
        push(&mut self.output.scans, scan)?;
        self.op(Op::Scan { id, fail })?;
        Ok(first)
    }

    fn ends_at_end(&self, id: Id, depth: u32) -> bool {
        const MAX_DEPTH: u32 = 8;
        let mut terms = self.graph.terms(&id).iter().rev();
        let last = terms.find(|&&it| self.graph[it].max != 0 || self.is_end(it));
        last.is_some_and(|&last| match self.graph[last].kind {
            Kind::Alternation(children) if depth < MAX_DEPTH => {
                let mut alternatives = self.graph.children_of(children).iter();
                alternatives.all(|&it| self.ends_at_end(it, depth + 1))
            }
            _ => self.is_end(last),
        })
    }

    fn is_end(&self, id: Id) -> bool {
        self.graph[id].kind == Kind::Edge(Direction::Forward)
    }

    fn starts_at_start(&self, id: Id) -> bool {
        let terms = self.graph.terms(&id).iter().map(|&it| &self.graph[it]);
        let mut empty = terms.take_while(|it| it.max == 0);
        empty.any(|it| it.kind == Kind::Edge(Direction::Backward))
    }

    fn find_first_run(&mut self, root: Id, labels: Labels) -> Result<(), Error> {
        if !self.graph.referenced_pairs.is_empty() {
            return Ok(());
        }
        let mut repeated = root;
        if let Kind::Alternation(children) = self.graph[root].kind {
            match self.graph.children_of(children) {
                [before @ .., last] if before.iter().all(|&it| self.starts_at_start(it)) => {
                    repeated = *last;
                }
                _ => return Ok(()),
            }
        }
        let terms = self.graph.terms(&repeated);
        let before = terms.iter().take_while(|&&it| self.graph[it].is(Traits::RIGID));
        let (count, mut distance) = before.fold((0, 0u32), |(count, sum), &it| {
            (count + 1, sum.saturating_add(self.graph[it].min))
        });
        let Some((&node, after)) = terms[count..].split_first() else {
            return Ok(());
        };
        match self.graph[node].kind {
            Kind::Run { max: UNBOUNDED, .. } => {}
            Kind::Repeat {
                body,
                min,
                max: UNBOUNDED,
                form: Form::Possessive | Form::Stepped,
                ..
            } if self.graph.turns_are_alike(body) && self.only_step(body).is_some() => {
                distance = distance.saturating_add(min);
            }
            _ => return Ok(()),
        }
        if distance > MAX_REACH || after.iter().all(|&it| self.graph[it].is(Traits::CANNOT_FAIL)) {
            return Ok(());
        }
        let restart = match distance {
            1 => labels.search,
            _ => self.write_restart(distance, labels)?,
        };
        self.first_run = Some(FirstRun { node, restart });
        Ok(())
    }

    /// Moves to the first start from which the run is entered after its end.
    fn write_restart(&mut self, distance: u32, labels: Labels) -> Result<Label, Error> {
        let restart = self.output.label()?;
        let room = Room::NONE.with(Direction::Backward, distance);
        self.output.bind(restart, Section::Cold, room, ROOT);
        if distance == 0 {
            let check = Op::Need {
                direction: Direction::Forward,
                count: 1,
                fail: labels.give_up,
            };
            self.output.op(Section::Cold, check)?;
        }
        let by = 1 - distance as i32;
        self.output.op(Section::Cold, Op::Advance { by })?;
        self.output.op(Section::Cold, Op::Jump { to: labels.search })?;
        Ok(restart)
    }
}

const POSITIONS_WORTH_A_SCAN: u32 = 8;
const NODES_OF_CHEAP_ATTEMPT: usize = 64;
