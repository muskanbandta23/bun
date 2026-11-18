//! Blocks and edges that are not needed.

use super::Simplifier;
use crate::ir::{Block, Edge, Entity, Op, Value};

impl Simplifier<'_> {
    /// While control flows as it did.
    pub(super) fn looks_untidy(&self, block: Block) -> bool {
        let mut targets = self
            .function
            .successors(block)
            .map(|edge| self.function.target(edge));
        match (targets.next(), targets.next()) {
            (Some(first), None) => {
                self.function.first(block) == self.function.last(block)
                    || self.cfg.predecessors(first).len() == 1
            }
            (first, second) => first.is_some() && first == second,
        }
    }

    pub(super) fn tidy_blocks(&mut self) {
        let order = self.cfg.order();
        self.tables
            .predecessor_counts
            .refill(self.function.block_count(), 0);
        for &block in order {
            for edge in self.function.successors(block) {
                self.tables.predecessor_counts[self.function.target(edge)] += 1;
            }
        }
        let would_find_more = self.would_find_more;
        for &block in order.iter().rev() {
            self.tidy_end(block);
        }
        self.would_find_more = would_find_more;
        for &block in order {
            while let Some(edge) = self.only_jump(block)
                && self.tables.predecessor_counts[self.function.target(edge)] == 1
                && self.tables.entered.contains(block.index())
            {
                self.hit();
                self.merge(block, edge);
            }
        }
    }

    /// What comes later in the order is tidy already.
    fn tidy_end(&mut self, block: Block) {
        for edge in self.function.successors(block) {
            self.skip_empty_block(edge);
        }
        if self.function.successors(block).len() > 1 && self.goes_one_way(block) {
            self.hit();
            let (kept, count) = (
                self.function.successor(block, 0),
                self.function.successors(block).len(),
            );
            self.tables.predecessor_counts[self.function.target(kept)] -= count as u32 - 1;
            self.jump_along(kept);
        }
    }

    fn only_jump(&self, block: Block) -> Option<Edge> {
        let last = self.function.last(block).expand()?;
        (self.op(last) == Op::Jump).then(|| self.function.successor(block, 0))
    }

    fn goes_one_way(&self, block: Block) -> bool {
        let function = &*self.function;
        let first = function.successor(block, 0);
        let is_same = |edge: Edge| {
            let mut pairs = function.args(edge).iter().zip(function.args(first));
            function.target(edge) == function.target(first)
                && pairs.all(|(&a, &b)| function.resolve(a) == function.resolve(b))
        };
        matches!(
            self.op(function.last(block)),
            Op::Branch(_) | Op::Switch(..) | Op::JumpTable(..)
        ) && function.successors(block).skip(1).all(is_same)
    }

    fn merge(&mut self, block: Block, edge: Edge) {
        let next = self.function.target(edge);
        for position in 0..self.function.params(next).len() {
            let arg = self.function.resolve(self.function.args(edge)[position]);
            self.pass_uses_on(self.function.params(next)[position], arg);
            self.release(arg);
            (self.left_aliases, self.would_find_more) = (true, true);
        }
        self.function.merge_with_successor(block);
        self.changed_control = true;
        self.remove_unused();
    }

    /// Whether only the jump uses the parameters, and copies of its other arguments are few.
    fn can_be_skipped(&self, empty: Block, onward: Edge) -> bool {
        const MOST_COPIES: usize = 8;
        let function = &*self.function;
        let is_own = |&&arg: &&Value| function.block_of(function.resolve(arg)) == empty;
        let passed_on = function.args(onward).iter().filter(is_own).count();
        let uses = function
            .params(empty)
            .iter()
            .map(|&param| self.tables.values[param].uses);
        let (others, copies) = (
            function.args(onward).len() - passed_on,
            self.tables.predecessor_counts[empty] as usize - 1,
        );
        uses.sum::<u32>() as usize == passed_on && others * copies <= MOST_COPIES
    }

    fn skip_empty_block(&mut self, edge: Edge) {
        let empty = self.function.target(edge);
        if self.function.first(empty) != self.function.last(empty)
            || self.cfg.loop_headed_by(empty).is_some()
        {
            return;
        }
        let Some(onward) = self.only_jump(empty) else {
            return;
        };
        if !self.can_be_skipped(empty, onward) {
            return;
        }
        self.hit();
        let mut args = core::mem::take(&mut self.tables.list);
        args.clear();
        args.extend(self.function.args(onward).iter().map(|&arg| {
            let arg = self.function.resolve(arg);
            match self.op(arg) {
                Op::Param(position) if self.function.block_of(arg) == empty => self
                    .function
                    .resolve(self.function.args(edge)[position as usize]),
                _ => arg,
            }
        }));
        self.retain_all(&args);
        for position in 0..self.function.args(edge).len() {
            self.release(self.function.args(edge)[position]);
        }
        let target = self.function.target(onward);
        self.function.redirect(edge, target, &args);
        self.tables.list = args;
        self.tables.predecessor_counts[empty] -= 1;
        if self.tables.predecessor_counts[empty] == 0 {
            self.discard_blocks(&[empty]);
        } else {
            self.tables.predecessor_counts[target] += 1;
        }
        self.changed_control = true;
        self.remove_unused();
    }
}
