//! Parameters that always get the same value.

use super::Simplifier;
use crate::ir::{Block, Edge, Op, Table, Value};

#[derive(Default)]
pub(super) struct Tables {
    candidates: Vec<Value>,
    /// For a candidate: the value that it stands for, or `NONE` as long as none has reached it.
    source: Table<Value, Value>,
    signatures: Vec<(u32, u32)>,
}

impl Simplifier<'_> {
    fn arg(&self, edge: Edge, position: usize) -> Value {
        self.function.resolve(self.function.args(edge)[position])
    }

    fn only_arg(&self, param: Value, position: usize, edges: &[Edge]) -> Option<Value> {
        let mut args = edges.iter().map(|&edge| self.arg(edge, position));
        let first = args.find(|&arg| arg != param)?;
        args.all(|arg| arg == first || arg == param)
            .then_some(first)
    }

    fn replace_param(&mut self, param: Value, replacement: Value) {
        self.pass_uses_on(param, replacement);
        self.function.replace_param(param, replacement);
    }

    fn drop_dead_params(&mut self, block: Block, edges: &[Edge]) {
        for position in 0..self.function.params(block).len() {
            if !matches!(self.op(self.function.params(block)[position]), Op::Param(_)) {
                for &edge in edges {
                    self.release(self.function.args(edge)[position]);
                }
            }
        }
        self.function.drop_dead_params(block, edges);
        self.remove_unused();
    }

    /// On entering the block. Unless it `is_entered`, it may be part of a cycle that nothing reaches.
    pub(super) fn simplify_params(&mut self, edges: &[Edge], is_entered: bool) {
        let block = self.block;
        let mut has_dead = false;
        for position in 0..self.function.params(block).len() {
            let param = self.function.params(block)[position];
            if self.tables.values[param].uses == 0 {
                self.hit();
                self.function.remove_param(param);
            } else if let Some(arg) = self.only_arg(param, position, edges) {
                let home = self.function.block_of(arg).expand();
                if !is_entered
                    && home.is_some_and(|home| !self.dominators.strictly_dominates(home, block))
                {
                    continue;
                }
                self.hit();
                self.replace_param(param, arg);
            } else {
                continue;
            }
            has_dead = true;
        }
        has_dead |= is_entered && self.replace_twins(block, edges);
        if has_dead {
            self.drop_dead_params(block, edges);
        }
    }

    /// Parameters that get the same arguments, or each itself.
    fn replace_twins(&mut self, block: Block, edges: &[Edge]) -> bool {
        if self.function.params(block).len() < 2 {
            return false;
        }
        let input = |this: &Self, edge: Edge, position: u32| {
            let arg = this.arg(edge, position as usize);
            (arg != this.function.params(block)[position as usize]).then_some(arg)
        };
        let mut signatures = core::mem::take(&mut self.tables.params.signatures);
        signatures.clear();
        for (position, &param) in (0..).zip(self.function.params(block)) {
            if let Op::Param(_) = self.op(param) {
                let inputs = edges.iter().map(|&edge| input(self, edge, position));
                let hash = inputs.fold(0u32, |hash, arg| {
                    (hash ^ arg.unwrap_or(Value::NONE).0)
                        .wrapping_mul(0x9e37_79b9)
                        .rotate_left(13)
                });
                signatures.push((hash, position));
            }
        }
        signatures.sort_unstable();
        let mut found = false;
        for group in signatures.chunk_by(|a, b| a.0 == b.0) {
            let first = group[0].1;
            for &(_, other) in &group[1..] {
                if edges
                    .iter()
                    .all(|&edge| input(self, edge, first) == input(self, edge, other))
                {
                    self.hit();
                    let params = self.function.params(block);
                    self.replace_param(params[other as usize], params[first as usize]);
                    found = true;
                }
            }
        }
        self.tables.params.signatures = signatures;
        found
    }

    /// The one value that reaches the parameter as far as is known, the parameter itself if several do, and whether
    /// that can change.
    fn source_of(&self, param: Value, sources: &Table<Value, Value>) -> (Value, bool) {
        let Op::Param(position) = self.op(param) else {
            unreachable!("not a parameter")
        };
        let (mut found, mut found_directly) = (Value::NONE, Value::NONE);
        for &edge in self.cfg.predecessors(self.function.block_of(param)) {
            if !self.function.is_attached(edge) {
                continue;
            }
            let arg = self.arg(edge, position as usize);
            let is_param = matches!(self.op(arg), Op::Param(_));
            let source = if is_param { sources[arg] } else { arg };
            if !is_param {
                if !found_directly.is_none() && found_directly != arg {
                    return (param, false);
                }
                found_directly = arg;
            }
            if arg != param && source != param && !source.is_none() && source != found {
                found = if found.is_none() { source } else { param };
            }
        }
        (found, true)
    }

    /// Parameters that pass one value around among themselves. It assumes that all do until it finds otherwise.
    pub(super) fn replace_cycles_of_params(&mut self) {
        const MOST_SWEEPS: usize = 8;
        if self.cfg.loops().len() == 0 {
            return;
        }
        let Tables {
            mut candidates,
            mut source,
            signatures,
        } = core::mem::take(&mut self.tables.params);
        candidates.clear();
        source.refill(self.function.value_count(), Value::NONE);
        for &block in self.cfg.order() {
            for &param in self.function.params(block) {
                if self.cfg.predecessors(block).is_empty() {
                    source[param] = param;
                } else {
                    candidates.push(param);
                }
            }
        }
        let has_settled = (0..MOST_SWEEPS).any(|_| {
            let mut is_unchanged = true;
            candidates.retain(|&param| {
                let (found, may_change) = self.source_of(param, &source);
                is_unchanged &= core::mem::replace(&mut source[param], found) == found;
                may_change
            });
            is_unchanged
        });
        if has_settled {
            candidates.retain(|&param| !source[param].is_none() && source[param] != param);
            for &param in &candidates {
                self.hit();
                self.replace_param(param, source[param]);
                (self.left_aliases, self.would_find_more) = (true, true);
            }
            let mut edges = core::mem::take(&mut self.tables.edges);
            candidates.dedup_by_key(|param| self.function.block_of(*param));
            for &param in &candidates {
                let block = self.function.block_of(param);
                edges.clear();
                let incoming = self.cfg.predecessors(block).iter();
                edges.extend(incoming.filter(|&&edge| self.function.is_attached(edge)));
                self.drop_dead_params(block, &edges);
            }
            self.tables.edges = edges;
        }
        self.tables.params = Tables {
            candidates,
            source,
            signatures,
        };
    }
}
