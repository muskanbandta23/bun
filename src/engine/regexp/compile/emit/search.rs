//! The attempts at one position after the other.

use super::output::{Output, ROOT};
use super::state::{Fail, Spot, State};
use super::{Captures, Emitter, Turn};
use crate::Error;
use crate::compile::Mode;
use crate::compile::Options;
use crate::compile::graph::{Id, Kind};
use crate::compile::lower::Lowered;
use crate::compile::memory::StackLimit;
use crate::program::{Class, Direction, Op, Program, Room, Width};
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

impl Emitter<'_> {
    fn search(&mut self, root: Id, is_sticky: bool, reads_pairs: bool) -> Result<(), Error> {
        let (give_up, attempt) = (self.output.label()?, self.output.label()?);
        let no_match = self.fail_to_either(give_up, give_up)?;
        let min = self.graph[root].min;
        if reads_pairs {
            self.op(Op::LeavePair)?;
        }
        if is_sticky || self.starts_at_input_start(root) {
            self.need(min, no_match)?;
            self.match_start = self.spot()?;
            self.node(root, 0, no_match)?;
        } else {
            self.repeats = true;
            self.bind_settled(attempt, Room::NONE, ROOT)?;
            self.need(min, no_match)?;
            let next = self.output.label()?;
            let undo = self.plan_undo(true)?;
            let attempt_fails = self.fail_to(next, undo)?;
            let before = self.snapshot()?;
            self.match_start = self.spot()?;
            if reads_pairs && self.graph.first(root).intersects_wide(TRAILS) {
                let fail = self.target(attempt_fails)?;
                self.op(Op::NotInPair { offset: 0, fail })?;
            }
            self.node(root, 0, attempt_fails)?;
            self.bind_settled(next, before.state.room, ROOT)?;
            self.need(1, no_match)?;
            self.advance(1)?;
            self.jump(attempt)?;
        }
        self.bind_settled(give_up, Room::NONE, ROOT)?;
        self.op(Op::NoMatch)
    }

    fn starts_at_input_start(&self, root: Id) -> bool {
        let terms = self.graph.terms(&root).iter().map(|&it| &self.graph[it]);
        let mut empty = terms.take_while(|it| it.max == 0);
        empty.any(|it| it.kind == Kind::Edge(Direction::Backward))
    }
}
