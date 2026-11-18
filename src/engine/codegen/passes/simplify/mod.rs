//! Replaces operations by cheaper ones, and values by others that are known to be equal.
//!
//! One walk over the tree of dominators comes to definitions before their uses and, where loops have one entry, to a
//! block after all its predecessors but those in the loop that it heads. Uses are counted, so what loses its last one
//! goes at once, and rules can tell whether an operand goes away with the instruction that they rewrite.

mod arithmetic;
mod bitwise;
mod blocks;
mod checked;
mod comparisons;
mod conditions;
mod control;
mod conversions;
mod demanded;
mod division;
mod facts;
mod float_comparisons;
mod floats;
mod knowledge;
mod memory;
mod numbering;
mod params;
mod rewrite;
mod selects;
mod shifts;
mod shuffles;
mod vectors;

use self::facts::Facts;
use self::numbering::Numbering;
use self::rewrite::Step;
use super::Analyses;
use crate::analysis::{Cfg, Dominators};
use crate::bitset::BitSet;
use crate::ir::{Block, Edge, Entity, Function, Op, State, Table, Type, Value};
use crate::target::Target;

#[derive(Clone, Copy, Default)]
struct ValueInfo {
    uses: u32,
    /// The position in `Tables::facts` plus one.
    facts: u32,
    /// The position of the walk up to which a branch or check has decided whether the value is zero.
    decided_until: u32,
    is_zero: bool,
}

/// Kept from one function to the next for their allocations.
#[derive(Default)]
pub(super) struct Tables {
    values: Table<Value, ValueInfo>,
    facts: Vec<Facts>,
    state_users: Table<State, u32>,
    resolved_states: BitSet,
    numbering: Numbering,
    /// An edge from a block that can be reached leads here.
    entered: BitSet,
    /// May have no use left.
    doomed: Vec<Value>,
    edges: Vec<Edge>,
    list: Vec<Value>,
    params: params::Tables,
    predecessor_counts: Table<Block, u32>,
    #[cfg(debug_assertions)]
    pub(super) fired_rules: Vec<&'static core::panic::Location<'static>>,
}

struct Simplifier<'a> {
    function: &'a mut Function,
    cfg: &'a Cfg,
    dominators: &'a Dominators,
    target: Target,
    tables: &'a mut Tables,
    block: Block,
    /// Of the block in the walk.
    position: u32,
    end_of_subtree: u32,
    /// New instructions go before it.
    cursor: Value,
    /// How many more rules may be applied for the sake of the instruction at the cursor.
    budget: u32,
    changed_control: bool,
    /// A value has been replaced after some of its uses were visited.
    left_aliases: bool,
    would_find_more: bool,
}

pub(super) fn run(function: &mut Function, analyses: &mut Analyses, target: Target) {
    const MOST_WALKS: usize = 3;
    let mut tables = core::mem::take(&mut analyses.simplify);
    #[cfg(debug_assertions)]
    tables.fired_rules.clear();
    for _ in 0..MOST_WALKS {
        let (cfg, dominators) = analyses.dominators(function);
        let mut simplifier = Simplifier {
            function,
            cfg,
            dominators,
            target,
            tables: &mut tables,
            block: Block::NONE,
            position: 0,
            end_of_subtree: 0,
            cursor: Value::NONE,
            budget: 0,
            changed_control: false,
            left_aliases: false,
            would_find_more: false,
        };
        simplifier.count_uses();
        simplifier.walk();
        simplifier.replace_cycles_of_params();
        simplifier.tidy_blocks();
        let Simplifier {
            changed_control,
            left_aliases,
            would_find_more,
            ..
        } = simplifier;
        if left_aliases {
            function.resolve_aliases();
        }
        if changed_control {
            analyses.invalidate();
        }
        if !would_find_more {
            break;
        }
    }
    analyses.simplify = tables;
}

impl Simplifier<'_> {
    /// Also removes what is not used, so that it does not stand in for a later instruction that computes the same.
    fn count_uses(&mut self) {
        let (function, tables, cfg) = (&mut *self.function, &mut *self.tables, self.cfg);
        tables
            .values
            .refill(function.value_count(), ValueInfo::default());
        tables.facts.clear();
        tables.state_users.refill(function.state_count(), 0);
        tables.resolved_states.reset(function.state_count());
        tables.numbering.reset();
        tables.entered.reset(function.block_count());
        for &block in cfg.order().iter().rev() {
            for edge in function.successors(block) {
                for &arg in function.args(edge) {
                    tables.values[arg].uses += 1;
                }
            }
            let mut value = function.last(block);
            while !value.is_none() {
                let previous = function.previous(value);
                if tables.values[value].uses == 0 && !function.effects(value).must_execute() {
                    function.remove(value);
                } else {
                    for &operand in function.operands(value) {
                        tables.values[operand].uses += 1;
                    }
                    if let Some(state) = function.state_of(value).expand() {
                        tables.state_users[state] += 1;
                        if tables.state_users[state] == 1 {
                            for &kept in function.state(state) {
                                tables.values[kept].uses += 1;
                            }
                        }
                    }
                }
                value = previous;
            }
            if cfg.loop_headed_by(block).is_none() {
                drop_unused_params(function, tables, cfg, block);
            }
        }
    }

    fn walk(&mut self) {
        let order = self.dominators.pre_order();
        while let Some(&block) = order.get(self.position as usize) {
            (self.block, self.end_of_subtree) = (block, self.dominators.end_of_subtree(block));
            if !self.enter_block() {
                let subtree = &order[self.position as usize..self.end_of_subtree as usize];
                self.discard_blocks(subtree);
                self.position = self.end_of_subtree;
                continue;
            }
            let mut value = self.function.first(block);
            while value != self.function.last(block) {
                self.visit(value);
                value = self.function.next(value);
            }
            self.visit_end(value);
            self.position += 1;
        }
    }

    #[inline]
    fn point_at(&mut self, value: Value) {
        const BUDGET: u32 = 40;
        (self.cursor, self.budget) = (value, BUDGET);
        self.function.resolve_operands(value);
        if let Some(state) = self.function.state_of(value).expand()
            && self.tables.resolved_states.insert(state.index())
        {
            self.function.resolve_state(state);
        }
    }

    fn visit(&mut self, value: Value) {
        self.point_at(value);
        // A loop may have asked before the operands were visited.
        self.tables.values[value].facts = 0;
        let (mut op, ty) = (*self.function.op(value), self.type_of(value));
        loop {
            if self.order_operands(&mut op) {
                *self.function.op_mut(value) = op;
            }
            match self.step(op, ty) {
                Step::Keep => break,
                Step::Is(other) => return self.replace(value, other),
                Step::To(new) => {
                    self.change(value, new);
                    op = new;
                }
                Step::Gone => return self.discard(value),
                Step::Exits(exit) => return self.end_block_at(value, exit),
            }
        }
        if let Op::Check(condition, _) = op {
            self.decide(condition, true);
        } else if let Some(key) = numbering::key(op) {
            self.number(value, key, ty);
        }
        self.remove_unused();
    }

    #[inline]
    fn number(&mut self, value: Value, key: Op, ty: Type) {
        let (position, end) = (self.position, self.end_of_subtree);
        match self.tables.numbering.find(self.function, key, ty, position) {
            Ok(known) => {
                self.hit();
                return self.replace(value, known);
            }
            Err(vacancy) => self.tables.numbering.add(&vacancy, value, position, end),
        }
        if let Op::Checked(op, args, _) = key {
            let plain = Op::Binary(numbering::unchecked(op), args);
            if let Err(vacancy) = self
                .tables
                .numbering
                .find(self.function, plain, ty, position)
            {
                self.tables.numbering.add(&vacancy, value, position, end);
            }
        }
    }

    /// Applies one rule. Operands are in their order.
    fn step(&mut self, op: Op, ty: Type) -> Step {
        if self.budget == 0 {
            return Step::Keep;
        }
        self.budget -= 1;
        match op {
            Op::Unary(op, a) => self.unary(op, ty, a),
            Op::Binary(op, [a, b]) => self.binary(op, ty, a, b),
            Op::Compare(cond, [a, b]) => self.compare(cond, a, b),
            Op::FloatCompare(cond, [a, b]) => self.float_compare(cond, a, b),
            Op::Select([condition, then, or]) => self.select(ty, condition, then, or),
            Op::Extract(tuple, position) => self.extract(tuple, position),
            Op::Load { .. }
            | Op::Store { .. }
            | Op::LoadLane { .. }
            | Op::StoreLane { .. }
            | Op::AtomicUpdate { .. }
            | Op::AtomicCompareExchange { .. } => self.access(op),
            Op::Check(condition, exit) => self.check(condition, exit),
            Op::Checked(op, [a, b], exit) => self.checked(op, ty, a, b, exit),
            Op::VectorUnary(op, a) => self.vector_unary(op, a),
            Op::VectorBinary(op, [a, b]) => self.vector_binary(op, a, b),
            Op::VectorTernary(op, args) => self.vector_ternary(op, args),
            Op::VectorShift(op, [a, amount]) => self.vector_shift(op, a, amount),
            Op::VectorReduce(op, a) => self.vector_reduce(op, a),
            Op::Shuffle([a, b], pattern) => self.shuffle(a, b, pattern),
            Op::Splat(lanes, scalar) => self.splat(lanes, scalar),
            Op::ExtractLane {
                lanes,
                sign,
                lane,
                vector,
            } => self.extract_lane(lanes, sign, lane, vector),
            Op::ReplaceLane {
                lanes,
                lane,
                args: [vector, scalar],
            } => self.replace_lane(lanes, lane, vector, scalar),
            _ => Step::Keep,
        }
    }
}

/// The arguments have not been counted yet.
fn drop_unused_params(function: &mut Function, tables: &Tables, cfg: &Cfg, block: Block) {
    let incoming = cfg.predecessors(block);
    let is_used = |param: Value| tables.values[param].uses != 0;
    if incoming.is_empty() || function.params(block).iter().all(|&param| is_used(param)) {
        return;
    }
    for position in 0..function.params(block).len() {
        let param = function.params(block)[position];
        if !is_used(param) {
            function.remove_param(param);
        }
    }
    function.drop_dead_params(block, incoming);
}
