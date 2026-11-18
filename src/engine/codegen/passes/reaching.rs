//! Finds the value that a variable has at the start of a block, and adds the parameters that bring it there.

use core::ops::Range;

use super::merges::Merges;
use crate::analysis::{Cfg, Dominators};
use crate::ir::{Block, Entity, Function, Table, Type, Value, Variable, entities};

const NO_RANKS: Range<u32> = Range {
    start: u32::MAX,
    end: 0,
};

entities! {
    /// The question what a variable holds at the start of a block.
    Read = "read";
    Definition = "definition";
}

/// A read with its answer, or a definition. `L` links those of one variable.
#[derive(Clone, Copy)]
struct Mention<L> {
    rank: u32,
    value: Value,
    earlier: L,
}

#[derive(Clone)]
struct VariableData {
    ty: Type,
    last_definition: Definition,
    last_read: Read,
    /// Of the blocks that mention it.
    ranks: Range<u32>,
}

pub(super) struct Reaching<'a> {
    cfg: &'a Cfg,
    dominators: &'a Dominators,
    variables: Table<Variable, VariableData>,
    definitions: Table<Definition, Mention<Definition>>,
    reads: Table<Read, Mention<Read>>,
}

/// The state of the search for one variable, with tables by rank.
struct Search<'a> {
    cfg: &'a Cfg,
    merges: Merges<'a>,
    ty: Type,
    bit: u64,
    least_rank: u32,
    found_nothing: bool,
    defined: Vec<Value>,
    params: Vec<Value>,
    /// Parameters before this are of other searches.
    first_param: Value,
    without_args: Vec<u32>,
}

impl<'a> Reaching<'a> {
    pub(super) fn new(cfg: &'a Cfg, dominators: &'a Dominators) -> Self {
        Self {
            cfg,
            dominators,
            variables: Table::default(),
            definitions: Table::default(),
            reads: Table::default(),
        }
    }

    pub(super) fn add_variable(&mut self, ty: Type) -> Variable {
        self.variables.push(VariableData {
            ty,
            last_definition: Definition::NONE,
            last_read: Read::NONE,
            ranks: NO_RANKS,
        })
    }

    #[inline]
    fn mention(&mut self, variable: Variable, block: Block) -> u32 {
        let rank = self.cfg.position(block) + 1;
        let ranks = &mut self.variables[variable].ranks;
        *ranks = ranks.start.min(rank)..ranks.end.max(rank + 1);
        rank
    }

    /// The variable holds `value` at the end of the block. Calls for one variable and block are not separated by one for
    /// the variable and another block, and the last counts.
    #[inline]
    pub(super) fn define(&mut self, variable: Variable, block: Block, value: Value) {
        let rank = self.mention(variable, block);
        let earlier = self.variables[variable].last_definition;
        if !earlier.is_none() && self.definitions[earlier].rank == rank {
            self.definitions[earlier].value = value;
        } else {
            self.variables[variable].last_definition = self.definitions.push(Mention {
                rank,
                value,
                earlier,
            });
        }
    }

    #[inline]
    pub(super) fn read(&mut self, variable: Variable, block: Block) -> Read {
        let rank = self.mention(variable, block);
        let earlier = self.variables[variable].last_read;
        if !earlier.is_none() && self.reads[earlier].rank == rank {
            return earlier;
        }
        let read = self.reads.push(Mention {
            rank,
            value: Value::NONE,
            earlier,
        });
        self.variables[variable].last_read = read;
        read
    }

    /// Once solved.
    #[inline]
    pub(super) fn answer(&self, read: Read) -> Value {
        self.reads[read].value
    }

    /// Where no definition comes first, a variable is zero. If that may be so for a read, some of the new parameters may
    /// have no use.
    pub(super) fn solve(&mut self, function: &mut Function) {
        let mut unsolved: Vec<Variable> = self
            .variables
            .keys()
            .filter(|&variable| !self.solve_without_params(function, variable))
            .collect();
        if unsolved.is_empty() {
            return;
        }
        let ranks = self.cfg.order().len() + 1;
        let mut search = Search {
            cfg: self.cfg,
            merges: Merges::new(self.cfg, self.dominators),
            ty: Type::Void,
            bit: 0,
            least_rank: 0,
            found_nothing: false,
            defined: vec![Value::NONE; ranks],
            params: vec![Value::from_index(0); ranks],
            first_param: Value::NONE,
            without_args: Vec::new(),
        };
        if unsolved.len() > u64::BITS as usize {
            unsolved = self.solve_in_groups(function, &mut search, unsolved);
        }
        self.extend_to_outermost_loops(&unsolved);
        unsolved = self.solve_in_groups(function, &mut search, unsolved);
        debug_assert!(unsolved.is_empty());
    }

    fn solve_without_params(&mut self, function: &mut Function, variable: Variable) -> bool {
        let VariableData {
            ty,
            last_definition,
            last_read,
            ..
        } = self.variables[variable];
        if last_read.is_none() {
            return true;
        }
        let value = match last_definition.expand() {
            None => function.zero(ty),
            Some(only) if self.definitions[only].earlier.is_none() => {
                let block = |rank: u32| self.cfg.order()[rank as usize - 1];
                let home = block(self.definitions[only].rank);
                let mut read = last_read;
                while !read.is_none() {
                    let reader = block(self.reads[read].rank);
                    if !self.dominators.strictly_dominates(home, reader) {
                        return false;
                    }
                    read = self.reads[read].earlier;
                }
                self.definitions[only].value
            }
            Some(_) => return false,
        };
        let mut next = last_read;
        while let Some(read) = next.expand() {
            self.reads[read].value = value;
            next = self.reads[read].earlier;
        }
        true
    }

    fn extend_to_outermost_loops(&mut self, variables: &[Variable]) {
        let loops = self.cfg.loops();
        let mut outermost = Table::filled(loops.len(), 0);
        for known in loops.keys() {
            outermost[known] = match loops[known].parent.expand() {
                Some(parent) => outermost[parent],
                None => loops[known].positions().start + 1,
            };
        }
        for &variable in variables {
            let first = &mut self.variables[variable].ranks.start;
            if let Some(around) = self
                .cfg
                .loop_of(self.cfg.order()[*first as usize - 1])
                .expand()
            {
                *first = outermost[around];
            }
        }
    }

    /// Looks only at the blocks between the first and the last mention. Returns the variables for which that is too
    /// little, because they may keep their value through a turn of a loop around the first mention.
    fn solve_in_groups(
        &mut self,
        function: &mut Function,
        search: &mut Search<'_>,
        mut variables: Vec<Variable>,
    ) -> Vec<Variable> {
        const GROUP: usize = u64::BITS as usize;
        if variables.len() > GROUP {
            variables.sort_unstable_by_key(|&variable| {
                let ranks = &self.variables[variable].ranks;
                (ranks.len().ilog2(), ranks.start)
            });
        }
        let mut unsolved = Vec::new();
        for group in variables.chunks(GROUP) {
            let ranks = self.ranks_of(group);
            for (bit, &variable) in group.iter().enumerate() {
                let mut definition = self.variables[variable].last_definition;
                while !definition.is_none() {
                    let Mention { rank, earlier, .. } = self.definitions[definition];
                    search.merges.add_changes(rank, 1 << bit);
                    definition = earlier;
                }
            }
            search.merges.compute(function, ranks.clone());
            search.least_rank = ranks.start;
            let starts_in_loop = self.is_below_loop_header(ranks.start);
            for (bit, &variable) in group.iter().enumerate() {
                search.bit = 1 << bit;
                self.solve_one(function, search, variable);
                if search.found_nothing && starts_in_loop {
                    unsolved.push(variable);
                }
            }
            search.merges.clear(ranks);
        }
        unsolved
    }

    fn ranks_of(&self, group: &[Variable]) -> Range<u32> {
        let mut ranks = NO_RANKS;
        for &variable in group {
            let own = &self.variables[variable].ranks;
            ranks = ranks.start.min(own.start)..ranks.end.max(own.end);
        }
        let mut around = self.cfg.loop_of(self.cfg.order()[ranks.end as usize - 2]);
        while !around.is_none() && self.cfg.loops()[around].positions().start + 1 >= ranks.start {
            ranks.end = self.cfg.loops()[around].positions().end + 1;
            around = self.cfg.loops()[around].parent;
        }
        ranks
    }

    fn is_below_loop_header(&self, rank: u32) -> bool {
        let block = self.cfg.order()[rank as usize - 1];
        self.cfg.depth(block) > u32::from(self.cfg.loop_headed_by(block).is_some())
    }

    fn solve_one(&mut self, function: &mut Function, search: &mut Search<'_>, variable: Variable) {
        let data = &self.variables[variable];
        search.ty = data.ty;
        search.found_nothing = false;
        search.first_param = Value::from_index(function.value_count());
        let mut definition = data.last_definition;
        while !definition.is_none() {
            let mention = self.definitions[definition];
            search.defined[mention.rank as usize] = mention.value;
            definition = mention.earlier;
        }
        let mut read = data.last_read;
        while !read.is_none() {
            self.reads[read].value = search.at_start(function, self.reads[read].rank);
            read = self.reads[read].earlier;
        }
        while let Some(rank) = search.without_args.pop() {
            for &edge in self.cfg.predecessors(search.merges.block(rank)) {
                let arg = search.at_end(function, search.merges.source_rank(function, edge));
                function.push_arg(edge, arg);
            }
        }
    }
}

impl Search<'_> {
    fn at_start(&mut self, function: &mut Function, rank: u32) -> Value {
        if self.is_merged_at(rank) {
            self.param(function, rank)
        } else {
            self.at_end(function, self.merges.parent(rank))
        }
    }

    fn is_merged_at(&self, rank: u32) -> bool {
        self.merges.merged_at(rank) & self.bit != 0
    }

    fn at_end(&mut self, function: &mut Function, rank: u32) -> Value {
        let rank = self.merges.last_change(rank, self.bit, self.least_rank);
        if rank < self.least_rank {
            self.found_nothing = true;
            function.zero(self.ty)
        } else if self.merges.changed_in(rank) & self.bit != 0 {
            self.defined[rank as usize]
        } else {
            self.param(function, rank)
        }
    }

    fn param(&mut self, function: &mut Function, rank: u32) -> Value {
        if self.params[rank as usize] < self.first_param {
            debug_assert!(!self.cfg.predecessors(self.merges.block(rank)).is_empty());
            self.params[rank as usize] = function.add_param(self.merges.block(rank), self.ty);
            self.without_args.push(rank);
        }
        self.params[rank as usize]
    }
}
