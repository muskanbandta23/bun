//! Copies small blocks in which ways meet into the ways, above all where that settles what the block tests.

use super::Analyses;
use super::bounds::Bounds;
use super::copier::{Copier, as_jump};
use super::orderings::Orderings;
use crate::analysis::{Cfg, Dominators, Frequencies};
use crate::ir::{Block, Edge, Function, Op, Table, Value};
use crate::target::Target;

const GREATEST_COST_OF_TAIL: u32 = 2;
const MOST_PARAMS_OF_TAIL: usize = 2;
const MOST_SUCCESSORS_OF_TAIL: usize = 3;
const MOST_PREDECESSORS_OF_TAIL: usize = 16;
const GREATEST_COST_OF_SETTLED_TEST: u32 = 4;
const GREATEST_COST_OF_PATH: u32 = 8;
const MOST_BLOCKS_ON_PATH: usize = 8;
const MOST_DOMINATORS_SEARCHED: usize = 8;
/// Values that outlive a copied block need parameters up to the next block that all ways go through.
const MOST_COPIED_BLOCKS_IN_A_ROW: u8 = 6;
const LEAST_BUDGET: usize = 64;
const VALUES_PER_UNIT_OF_BUDGET: usize = 4;

#[derive(Clone, Copy)]
struct BlockState {
    ways_in: u32,
    is_copied: bool,
    is_visited: bool,
    /// Once visited: among the block and its dominators, up to one in which ways meet and which is not copied.
    copied_in_a_row: u8,
}

struct Plan {
    /// The edge that the block is certain to take.
    taken: Option<Edge>,
}

struct Duplication<'a> {
    cfg: &'a Cfg,
    dominators: &'a Dominators,
    frequencies: &'a Frequencies,
    copier: Copier,
    blocks: Table<Block, BlockState>,
    budget: usize,
}

pub(super) fn run(function: &mut Function, analyses: &mut Analyses, _: Target) {
    let (cfg, dominators, frequencies) = analyses.all(function);
    let is_join = |&block: &Block| cfg.predecessors(block).len() > 1;
    if !cfg.order().iter().any(is_join) {
        return;
    }
    let mut blocks = function.block_table(BlockState {
        ways_in: 1,
        is_copied: false,
        is_visited: false,
        copied_in_a_row: 0,
    });
    for &block in cfg.order() {
        blocks[block].ways_in = cfg.predecessors(block).len().max(1) as u32;
    }
    let mut duplication = Duplication {
        cfg,
        dominators,
        frequencies,
        copier: Copier::new(function),
        blocks,
        budget: LEAST_BUDGET + function.value_count() / VALUES_PER_UNIT_OF_BUDGET,
    };
    let mut changed = false;
    for &block in cfg.order() {
        changed |= duplication.visit(function, block);
    }
    if changed {
        duplication.copier.repair_uses(function, analyses);
    }
}

impl Duplication<'_> {
    fn visit(&mut self, function: &mut Function, block: Block) -> bool {
        let is_join = self.cfg.predecessors(block).len() > 1;
        self.blocks[block].copied_in_a_row = match (self.blocks[block].is_copied, is_join) {
            (false, true) => 0,
            (is_copied, _) => self.copied_above(block) + u8::from(is_copied),
        };
        self.blocks[block].is_visited = true;
        if self.blocks[block].ways_in == 0 {
            for edge in function.successors(block) {
                self.blocks[function.target(edge)].ways_in -= 1;
            }
            return false;
        }
        if matches!(function.op(function.last(block)), Op::Patchpoint(_)) {
            return false;
        }
        let mut changed = false;
        for edge in function.successors(block) {
            if self.cfg.predecessors(function.target(edge)).len() > 1 {
                changed |= self.follow(function, edge);
            }
        }
        changed
    }

    /// The dominators that have not been visited are on the path that is being copied.
    fn copied_above(&self, block: Block) -> u8 {
        let (mut count, mut above) = (0, self.dominators.parent(block));
        while let Some(dominator) = above {
            if self.blocks[dominator].is_visited {
                return count + self.blocks[dominator].copied_in_a_row;
            }
            (count, above) = (count + 1, self.dominators.parent(dominator));
        }
        count
    }

    fn follow(&mut self, function: &mut Function, first: Edge) -> bool {
        let is_cold = function.is_rare(first) || self.frequencies.is_cold(function.source(first));
        let mut allowance = GREATEST_COST_OF_PATH;
        let mut plan = self.plan(function, first, first, is_cold, &mut allowance);
        if plan.is_none() {
            return false;
        }
        let mut edge = as_jump(function, first);
        let into = function.source(edge);
        for copied in 1.. {
            let Some(Plan { taken }) = plan else {
                break;
            };
            let tail = function.target(edge);
            self.blocks[tail].is_copied = true;
            self.blocks[tail].ways_in -= 1;
            if taken.is_some_and(|taken| function.is_rare(taken)) {
                self.make_rare(function, first);
            }
            self.copier.copy_body(function, edge);
            self.copier.copy_end(function, tail, into, taken);
            for new in function.successors(into) {
                self.blocks[function.target(new)].ways_in += 1;
                self.budget = self.budget.saturating_sub(function.args(new).len());
            }
            if taken.is_none() || copied == MOST_BLOCKS_ON_PATH {
                break;
            }
            edge = function.successor(into, 0);
            plan = self.plan(function, first, edge, is_cold, &mut allowance);
        }
        self.copier.end_path(into);
        true
    }

    /// For the target of `edge`, which is reached by way of `first`.
    fn plan(
        &mut self,
        function: &Function,
        first: Edge,
        edge: Edge,
        is_cold: bool,
        allowance: &mut u32,
    ) -> Option<Plan> {
        let tail = function.target(edge);
        if self.cfg.loop_headed_by(tail).is_some() {
            return None;
        }
        let preview = self.copier.preview(function, edge)?;
        let end = *function.op(function.last(tail));
        let (mut cost, params) = (preview.cost, function.params(tail).len());
        let taken = preview.taken.or_else(|| {
            let holds = self.known_on_the_way(function, first, edge)?;
            let is_computed_here = function.previous(function.last(tail)) != Value::NONE
                && end == Op::Branch(function.previous(function.last(tail)));
            cost -= u32::from(is_computed_here).min(cost);
            Some(function.successor(tail, usize::from(!holds)))
        });
        let is_last_way_in = self.blocks[tail].ways_in == 1;
        let is_settled =
            taken.is_some() && end != Op::Jump || preview.settled_check_fails.is_some();
        let pays = if is_settled {
            cost <= GREATEST_COST_OF_SETTLED_TEST && (cost == 0 || !is_cold || is_last_way_in)
        } else if is_cold || is_last_way_in || cost > GREATEST_COST_OF_TAIL {
            false
        } else if let Op::Branch(condition) = end
            && function.block_of(condition) == tail
            && let stand_in = self.copier.stand_in_along(function, edge, condition)
            && matches!(
                function.op(stand_in),
                Op::Compare(..) | Op::FloatCompare(..)
            )
            && stand_in != condition
        {
            true
        } else {
            *function.op(function.last(function.source(edge))) == Op::Jump
                && self.cfg.predecessors(tail).len() <= MOST_PREDECESSORS_OF_TAIL
                && (params <= MOST_PARAMS_OF_TAIL || matches!(end, Op::Jump | Op::Return(_)))
        };
        let can_end = taken.is_some()
            || function.successors(tail).len() <= MOST_SUCCESSORS_OF_TAIL
                && !matches!(end, Op::Patchpoint(_) | Op::Unreachable);
        if !pays || !can_end || self.copied_above(tail) >= MOST_COPIED_BLOCKS_IN_A_ROW {
            return None;
        }
        *allowance = allowance.checked_sub(cost)?;
        self.budget = self.budget.checked_sub(1 + cost as usize + params)?;
        Some(Plan { taken })
    }

    /// Whether the target of `edge` takes its first edge, as far as the branches that lead to `first` tell.
    fn known_on_the_way(&self, function: &Function, first: Edge, edge: Edge) -> Option<bool> {
        let tail = function.target(edge);
        let Op::Branch(condition) = *function.op(function.last(tail)) else {
            return None;
        };
        let along = |value| self.copier.stand_in_along(function, edge, value);
        let condition = along(condition);
        let comparison = match *function.op(condition) {
            Op::Compare(cond, operands) => Some((cond, operands.map(along))),
            _ => None,
        };
        let decide = |known: Value, holds: bool| {
            if known == condition {
                return Some(holds);
            }
            let (wanted, [a, b]) = comparison?;
            let Op::Compare(cond, [c, d]) = *function.op(known) else {
                return None;
            };
            let cond = if holds { cond } else { cond.inverted() };
            if (a, b) == (c, d) {
                Orderings::from(cond).decide(wanted)
            } else if (a, b) == (d, c) {
                Orderings::from(cond.commuted()).decide(wanted)
            } else if a == c && function.op(b).is_constant() && function.op(d).is_constant() {
                let [a, b, d] = [a, b, d].map(|value| Bounds::evident(function, value));
                Orderings::between(&a.where_holds(cond, &d), &b).decide(wanted)
            } else {
                None
            }
        };
        let (mut way, mut block) = (Some(first), function.source(first));
        for _ in 0..MOST_DOMINATORS_SEARCHED {
            if let Some((known, holds)) = way.and_then(|way| condition_for(function, way))
                && let Some(result) = decide(known, holds)
            {
                return Some(result);
            }
            way = match *self.cfg.predecessors(block) {
                [only] => Some(only),
                _ => None,
            };
            block = self.dominators.parent(block)?;
        }
        None
    }

    fn make_rare(&self, function: &mut Function, mut edge: Edge) {
        if function.successors(function.source(edge)).len() == 1 {
            let [only] = *self.cfg.predecessors(function.source(edge)) else {
                return;
            };
            edge = only;
        }
        let mut both = function.successors(function.source(edge));
        if condition_for(function, edge).is_some() && !both.any(|edge| function.is_rare(edge)) {
            function.set_rare(edge, true);
        }
    }
}

/// The value that the source of the edge branches on, and whether it is not zero if the edge is taken.
fn condition_for(function: &Function, edge: Edge) -> Option<(Value, bool)> {
    let source = function.source(edge);
    let Op::Branch(condition) = *function.op(function.last(source)) else {
        return None;
    };
    let position = function.successors(source).position(|own| own == edge)?;
    Some((condition, position == 0))
}
