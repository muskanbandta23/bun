//! Which values are live between blocks, and how soon they are wanted.

use bun_engine_asm::Bank;

use super::code::{Code, LOCAL, NEVER};
use crate::TooComplex;
use crate::analysis::{Cfg, Frequencies, Loop};
use crate::bitset;
use crate::ir::{Block, Edge, Entity, Function, Table};

const MAX_WORDS: usize = 8 << 20;

/// In instructions, with more added for what makes a use less likely to come.
pub(crate) type Distance = u32;

pub(crate) const BEYOND_LOOP: Distance = 1 << 16;
pub(crate) const BEYOND_RARE_EDGE: Distance = 1 << 22;
/// All registers are lost on the way.
pub(crate) const BEYOND_BARRIER: Distance = 1 << 24;
pub(crate) const FARTHEST: Distance = 1 << 28;

#[derive(Default)]
pub(crate) struct Liveness {
    words: usize,
    /// A row for each position in the order of blocks.
    live_in: Vec<u64>,
    /// A row for each loop, of what it reads outside cold code.
    used_in_loop: Vec<u64>,
    in_gpr_bank: Vec<u64>,
    definitions: Vec<u32>,
    definitions_end: Vec<u32>,
    row: Vec<u64>,
    seen: Vec<u64>,
    loop_pressure: Table<Loop, [u32; 2]>,
    /// For each word of `live_in`, the position in `distances` of its first bit. Empty if there are no distances.
    ranks: Vec<u32>,
    distances: Vec<Distance>,
    first_warm: Vec<Distance>,
}

fn union(into: &mut [u64], from: &[u64]) {
    into.iter_mut()
        .zip(from)
        .for_each(|(into, from)| *into |= from);
}

impl Liveness {
    /// Also moves the end of each value to where it stops being live.
    pub(crate) fn compute(
        &mut self,
        function: &Function,
        cfg: &Cfg,
        frequencies: &Frequencies,
        code: &mut Code,
        wants_distances: bool,
    ) -> Result<(), TooComplex> {
        self.words = bitset::words_for(code.globals.len());
        if (cfg.order().len() + cfg.loops().len()) * self.words > MAX_WORDS {
            return Err(TooComplex);
        }
        self.gather(cfg, frequencies, code);
        self.propagate(function, cfg);
        self.extend_ends(function, cfg, code);
        self.measure_loops(cfg, frequencies, code);
        self.ranks.clear();
        if wants_distances {
            self.measure_distances(function, cfg, code);
        }
        Ok(())
    }

    #[inline]
    fn range(&self, row: usize) -> core::ops::Range<usize> {
        row * self.words..(row + 1) * self.words
    }

    #[inline]
    pub(crate) fn live_in(&self, cfg: &Cfg, block: Block) -> &[u64] {
        &self.live_in[self.range(cfg.position(block) as usize)]
    }

    #[inline]
    pub(crate) fn used_in(&self, outer: Loop) -> &[u64] {
        &self.used_in_loop[self.range(outer.index())]
    }

    /// About the most registers of each bank that the values which the loop reads or defines need at once.
    pub(crate) fn pressure(&self, outer: Loop) -> [u32; 2] {
        self.loop_pressure[outer]
    }

    pub(crate) fn live_out(
        &self,
        function: &Function,
        cfg: &Cfg,
        block: Block,
        into: &mut Vec<u64>,
    ) {
        into.clear();
        into.resize(self.words, 0);
        for edge in function.successors(block) {
            union(into, self.live_in(cfg, function.target(edge)));
        }
    }

    fn gather(&mut self, cfg: &Cfg, frequencies: &Frequencies, code: &Code) {
        for (rows, count) in [
            (&mut self.live_in, cfg.order().len()),
            (&mut self.used_in_loop, cfg.loops().len()),
        ] {
            rows.clear();
            rows.resize(count * self.words, 0);
        }
        for foreign in &code.foreign_uses {
            let global = code.vregs[foreign.vreg].global;
            if global == LOCAL {
                continue;
            }
            let range = self.range(cfg.position(foreign.block) as usize);
            bitset::insert(&mut self.live_in[range], global as usize);
            if let Some(own) = cfg.loop_of(foreign.block).expand()
                && foreign.first_warm != NEVER
                && !frequencies.is_cold(foreign.block)
            {
                let range = self.range(own.index());
                bitset::insert(&mut self.used_in_loop[range], global as usize);
            }
        }
        for inner in cfg.loops().keys().rev() {
            if let Some(outer) = cfg.loops()[inner].parent.expand() {
                let (outer, inner) = (self.range(outer.index()), self.range(inner.index()));
                let (before, after) = self.used_in_loop.split_at_mut(inner.start);
                union(&mut before[outer], &after[..self.words]);
            }
        }

        self.in_gpr_bank.clear();
        self.in_gpr_bank.resize(self.words, 0);
        self.definitions_end.clear();
        self.definitions_end.resize(cfg.order().len(), 0);
        let defined = || {
            code.globals
                .iter()
                .map(|&vreg| &code.vregs[vreg])
                .filter(|data| !data.is_floating())
        };
        defined().for_each(|data| self.definitions_end[cfg.position(data.block) as usize] += 1);
        let mut total = 0;
        for count in &mut self.definitions_end {
            total += core::mem::replace(count, total);
        }
        self.definitions.clear();
        self.definitions.resize(total as usize, 0);
        for data in defined() {
            let next = &mut self.definitions_end[cfg.position(data.block) as usize];
            self.definitions[*next as usize] = data.global;
            *next += 1;
        }
        for (global, &vreg) in code.globals.iter().enumerate() {
            if code.vregs[vreg].class.bank() == Bank::Gpr {
                bitset::insert(&mut self.in_gpr_bank, global);
            }
        }
    }

    /// Returns whether anything was added.
    fn flow_backward(&mut self, function: &Function, cfg: &Cfg) -> bool {
        let mut added = 0;
        let mut row = core::mem::take(&mut self.row);
        for (position, &block) in cfg.order().iter().enumerate().rev() {
            self.live_out(function, cfg, block, &mut row);
            let start = position
                .checked_sub(1)
                .map_or(0, |before| self.definitions_end[before]);
            for &global in
                &self.definitions[start as usize..self.definitions_end[position] as usize]
            {
                bitset::remove(&mut row, global as usize);
            }
            let range = self.range(position);
            for (live, &out) in self.live_in[range].iter_mut().zip(&row) {
                added |= out & !*live;
                *live |= out;
            }
        }
        self.row = row;
        added != 0
    }

    /// What is live at the header of a natural loop is live in all of it.
    fn propagate(&mut self, function: &Function, cfg: &Cfg) {
        self.flow_backward(function, cfg);
        if cfg.loops().values().any(|data| !data.is_natural) {
            while self.flow_backward(function, cfg) {}
            return;
        }
        for (position, &block) in cfg.order().iter().enumerate() {
            let mut around = cfg.loop_of(block);
            if let Some(own) = cfg.loop_headed_by(block) {
                around = cfg.loops()[own].parent;
            }
            if let Some(around) = around.expand() {
                let header = self.range(cfg.position(cfg.loops()[around].header) as usize);
                let (before, after) = self.live_in.split_at_mut(position * self.words);
                union(&mut after[..self.words], &before[header]);
            }
        }
    }

    fn extend_ends(&mut self, function: &Function, cfg: &Cfg, code: &mut Code) {
        self.seen.clear();
        self.seen.resize(self.words, 0);
        let mut row = core::mem::take(&mut self.row);
        for &block in cfg.order().iter().rev() {
            self.live_out(function, cfg, block, &mut row);
            for (out, seen) in row.iter_mut().zip(&mut self.seen) {
                (*out, *seen) = (*out & !*seen, *seen | *out);
            }
            for global in bitset::iter(&row) {
                let data = &mut code.vregs[code.globals[global]];
                data.end = data.end.max(code.blocks[block].exit());
            }
        }
        self.row = row;
    }

    fn count_by_bank(&self, mut word: impl FnMut(usize) -> u64) -> [u32; 2] {
        let mut counts = [0; 2];
        for (index, &gprs) in self.in_gpr_bank.iter().enumerate() {
            let word = word(index);
            counts[Bank::Gpr as usize] += (word & gprs).count_ones();
            counts[Bank::Fpr as usize] += (word & !gprs).count_ones();
        }
        counts
    }

    fn measure_loops(&mut self, cfg: &Cfg, frequencies: &Frequencies, code: &Code) {
        let mut pressure = core::mem::take(&mut self.loop_pressure);
        pressure.refill(cfg.loops().len(), [0; 2]);
        let passing = |outer: Loop| {
            let (live, used) = (
                self.live_in(cfg, cfg.loops()[outer].header),
                self.used_in(outer),
            );
            move |index: usize| live[index] & !used[index]
        };
        for &block in cfg.order().iter().rev() {
            let Some(own) = cfg.loop_of(block).expand() else {
                continue;
            };
            if !frequencies.is_cold(block) || cfg.loops()[own].header == block {
                let (live, passing) = (self.live_in(cfg, block), passing(own));
                let foreign = self.count_by_bank(|index| live[index] & !passing(index));
                for bank in 0..2 {
                    pressure[own][bank] =
                        pressure[own][bank].max(foreign[bank] + code.blocks[block].pressure[bank]);
                }
            }
            let data = &cfg.loops()[own];
            if let Some(outer) = data.parent.expand().filter(|_| data.header == block) {
                let (inner, around) = (passing(own), passing(outer));
                let waiting = self.count_by_bank(|index| inner(index) & !around(index));
                for bank in 0..2 {
                    pressure[outer][bank] =
                        pressure[outer][bank].max(pressure[own][bank] + waiting[bank]);
                }
            }
        }
        self.loop_pressure = pressure;
    }

    pub(crate) fn has_distances(&self) -> bool {
        !self.ranks.is_empty()
    }

    #[inline]
    fn rank(&self, position: usize, global: usize) -> Option<usize> {
        let (index, bit) = (position * self.words + global / 64, global % 64);
        let word = self.live_in[index];
        (word >> bit & 1 != 0)
            .then(|| (self.ranks[index] + (word & ((1 << bit) - 1)).count_ones()) as usize)
    }

    fn cost_of(function: &Function, cfg: &Cfg, edge: Edge) -> Distance {
        let left = cfg
            .depth(function.source(edge))
            .saturating_sub(cfg.depth(function.target(edge)));
        left.min(16) * BEYOND_LOOP
            + if function.is_rare(edge) {
                BEYOND_RARE_EDGE
            } else {
                0
            }
    }

    /// From the end of the block.
    pub(crate) fn distance_after(
        &self,
        function: &Function,
        cfg: &Cfg,
        block: Block,
        global: u32,
    ) -> Distance {
        let along = |edge| {
            let rank = self.rank(
                cfg.position(function.target(edge)) as usize,
                global as usize,
            )?;
            Some(self.distances[rank] + Self::cost_of(function, cfg, edge))
        };
        function
            .successors(block)
            .filter_map(along)
            .min()
            .unwrap_or(FARTHEST)
            .min(FARTHEST)
    }

    fn measure_distances(&mut self, function: &Function, cfg: &Cfg, code: &Code) {
        let mut total = 0;
        self.ranks.extend(self.live_in.iter().map(|word| {
            total += word.count_ones();
            total - word.count_ones()
        }));
        self.ranks.push(total);
        self.distances.clear();
        self.distances.resize(total as usize, FARTHEST);
        self.first_warm.clear();
        self.first_warm.resize(total as usize, FARTHEST);
        for foreign in code
            .foreign_uses
            .iter()
            .filter(|foreign| foreign.first_warm != NEVER)
        {
            let global = code.vregs[foreign.vreg].global;
            if global != LOCAL {
                let rank = self
                    .rank(cfg.position(foreign.block) as usize, global as usize)
                    .unwrap();
                let block = &code.blocks[foreign.block];
                let distance = (foreign.first_warm - block.entry()) / 2;
                let barrier = block.barrier[code.vregs[foreign.vreg].class.bank() as usize];
                let is_beyond = barrier != 0 && distance >= barrier;
                self.first_warm[rank] = distance + if is_beyond { BEYOND_BARRIER } else { 0 };
            }
        }
        for _ in 0..if cfg.loops().len() == 0 { 1 } else { 2 } {
            for (position, &block) in cfg.order().iter().enumerate().rev() {
                let data = &code.blocks[block];
                let lengths = data.barrier.map(|barrier| {
                    data.end - data.first + if barrier == 0 { 0 } else { BEYOND_BARRIER }
                });
                let length_for =
                    |global| lengths[usize::from(!bitset::contains(&self.in_gpr_bank, global))];
                let range = self.range(position);
                let first = self.ranks[range.start] as usize;
                let mut distances = core::mem::take(&mut self.row);
                distances.clear();
                distances.extend(bitset::iter(&self.live_in[range]).enumerate().map(
                    |(index, global)| {
                        u64::from(match self.first_warm[first + index] {
                            FARTHEST => (length_for(global)
                                + self.distance_after(function, cfg, block, global as u32))
                            .min(FARTHEST),
                            own => own,
                        })
                    },
                ));
                for (index, &distance) in distances.iter().enumerate() {
                    self.distances[first + index] = distance as Distance;
                }
                self.row = distances;
            }
        }
    }
}
