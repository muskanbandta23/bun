//! Register allocation in one walk over the blocks.
//!
//! A value is in one register or at home. When registers run out, the value that is wanted last leaves. The state at
//! the start of a block is taken from the most frequent of the predecessors that come before it, and moves on the
//! edges make the others agree.

use bun_engine_asm::{Bank, Reg, RegisterSet};

use super::code::{AT_HOME, Class, Code, Home, LOCAL, Operand, Place, Registers, Role, Site, VReg};
use super::liveness::{BEYOND_LOOP, Distance, FARTHEST, Liveness};
use super::moves::{Move, Sequencer, Spot};
use super::slots::Slots;
use crate::analysis::{Cfg, Frequencies, Loop};
use crate::bitset;
use crate::ir::{Block, Edge, Function, Pool, Span, State, Table};

pub(super) type Mask = u64;

#[inline]
pub(super) fn bit(reg: u8) -> Mask {
    1 << (reg & 63)
}

#[inline]
pub(super) fn registers_in(mut mask: Mask) -> impl Iterator<Item = u8> {
    core::iter::from_fn(move || {
        (mask != 0).then(|| {
            let reg = mask.trailing_zeros() as u8;
            mask &= mask - 1;
            reg
        })
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Stored {
    /// Only where a move puts it at home.
    OnDemand,
    /// Right after the definition.
    AtDefinition,
    /// It is at home from the start.
    Always,
}

#[derive(Clone, Copy)]
pub(super) struct Status {
    pub(super) reg: u8,
    pub(super) stored: Stored,
    /// Of a parameter, at the start of its block.
    pub(super) entry_reg: u8,
    /// The block whose scan set `next`.
    pub(super) scanned_in: Block,
    /// The position at which it is wanted next, as of the instruction that the walk is at.
    pub(super) next: Distance,
}

#[derive(Clone, Copy, Default)]
pub(super) struct Snapshot {
    pub(super) occupied: Mask,
    pub(super) clean: Mask,
    pub(super) values: Span<VReg>,
}

/// An instruction of the current block that takes registers for itself.
#[derive(Clone, Copy)]
pub(super) struct Claim {
    pub(super) position: u32,
    pub(super) registers: Mask,
}

/// The result, and tables to use again.
#[derive(Default)]
pub(crate) struct Allocation {
    /// To be done before the instruction at a position. Ascending.
    pub(crate) moves: Vec<(u32, Move)>,
    pub(crate) edge_moves: Table<Edge, Span<Move>>,
    pub(crate) move_lists: Pool<Move>,
    pub(crate) state_assigned: Pool<u8>,
    pub(crate) slots: Slots,
    pub(crate) sequencer: Sequencer,
    pub(super) statuses: Table<VReg, Status>,
    pub(super) entries: Table<Block, Snapshot>,
    pub(super) exits: Table<Block, Snapshot>,
    pub(super) snapshot_values: Pool<VReg>,
    pub(super) loop_registers: Table<Loop, Mask>,
    pub(super) live_out: Vec<u64>,
    pub(super) pending: Vec<Move>,
    pub(super) candidates: Vec<(Distance, VReg)>,
    /// For each operand in the current block, last instruction first: what `next` becomes after it.
    next_uses: Vec<Distance>,
    /// Descending by position.
    pub(super) claims: Vec<Claim>,
    /// Values whose last use is in the state of the instruction at a position. Descending.
    state_deaths: Vec<(u32, VReg)>,
    scanned_states: Table<State, Block>,
    /// Values that have left the register in which the current instruction found them.
    displaced: Vec<(VReg, u8)>,
}

impl Allocation {
    #[inline]
    pub(crate) fn stored(&self, vreg: VReg) -> Stored {
        self.statuses[vreg].stored
    }

    #[inline]
    pub(crate) fn entry_register(&self, param: VReg) -> u8 {
        self.statuses[param].entry_reg
    }

    #[inline]
    pub(crate) fn moves_on(&self, edge: Edge) -> &[Move] {
        &self.move_lists[self.edge_moves[edge]]
    }
}

pub(super) struct Allocator<'a> {
    pub(super) function: &'a Function,
    pub(super) cfg: &'a Cfg,
    pub(super) frequencies: &'a Frequencies,
    pub(super) liveness: &'a Liveness,
    pub(super) code: &'a mut Code,
    pub(super) out: &'a mut Allocation,
    pub(super) registers: Registers,
    pub(super) banks: [Mask; 2],
    pub(super) value: [VReg; Reg::COUNT],
    pub(super) next: [Distance; Reg::COUNT],
    pub(super) occupied: Mask,
    /// The value in the register has been stored at home on every way here.
    pub(super) clean: Mask,
    pub(super) block: Block,
    pub(super) position: u32,
    /// Registers that values return to at the end of the current loop.
    reserved: Mask,
    /// Changes whenever a value that is defined already changes its place.
    epoch: u32,
    last_state: (State, u32, Span<u8>),
}

/// What the operands of the current instruction take.
#[derive(Default)]
struct Taken {
    /// Nothing else may be there when the instruction starts.
    blocked_early: Mask,
    /// Overwritten by the time it ends.
    blocked_late: Mask,
    upper_halves: Mask,
    read: Mask,
    /// Not for results.
    held: Mask,
    /// Hold operands that can be read at home if others need the room.
    yielding: Mask,
}

impl Taken {
    #[inline]
    fn overwritten(&self, class: Class) -> Mask {
        self.blocked_late
            | if class == Class::Vector {
                self.upper_halves
            } else {
                0
            }
    }
}

const CLAIMS_LOOKED_AT: usize = 4;
/// A value that can be read from memory is loaded if it is wanted again within this distance.
const SOON: Distance = 64;

pub(crate) fn allocate(
    function: &Function,
    cfg: &Cfg,
    frequencies: &Frequencies,
    liveness: &Liveness,
    registers: Registers,
    code: &mut Code,
    out: &mut Allocation,
) {
    out.moves.clear();
    out.edge_moves
        .refill(function.edge_count(), Span::default());
    out.move_lists.clear();
    out.state_assigned.clear();
    out.slots.share(function, cfg, code);
    out.sequencer.reset(code.vregs.len());
    out.statuses.clear();
    for data in code.vregs.values() {
        let stored = if data.home == Home::Spill {
            Stored::OnDemand
        } else {
            Stored::Always
        };
        out.statuses.push(Status {
            reg: AT_HOME,
            stored,
            entry_reg: AT_HOME,
            scanned_in: Block::NONE,
            next: FARTHEST,
        });
    }
    out.entries
        .refill(function.block_count(), Snapshot::default());
    out.exits
        .refill(function.block_count(), Snapshot::default());
    out.snapshot_values.clear();
    out.loop_registers.refill(cfg.loops().len(), 0);
    out.scanned_states
        .refill(function.state_count(), Block::NONE);
    let mut allocator = Allocator {
        function,
        cfg,
        frequencies,
        liveness,
        code,
        out,
        registers,
        banks: [Bank::Gpr, Bank::Fpr].map(|bank| registers.allocatable.in_bank(bank).bits()),
        value: [VReg::NONE; Reg::COUNT],
        next: [FARTHEST; Reg::COUNT],
        occupied: 0,
        clean: 0,
        block: Block::NONE,
        position: 0,
        reserved: 0,
        epoch: 0,
        last_state: (State::NONE, 0, Span::default()),
    };
    for &block in cfg.order() {
        allocator.allocate_block(block);
    }
}

impl Allocator<'_> {
    fn allocate_block(&mut self, block: Block) {
        let previous = core::mem::replace(&mut self.block, block);
        let data = self.code.blocks[block];
        self.position = data.first;
        self.scan();
        self.enter(previous);
        self.reserved = self
            .cfg
            .loop_of(block)
            .expand()
            .map_or(0, |own| self.out.loop_registers[own]);
        self.epoch += 1;
        for position in data.first..data.end {
            self.position = position;
            self.allocate_instruction();
        }
        self.leave();
    }

    // The scan of a block from its end

    #[inline]
    fn is_live_out(&self, vreg: VReg) -> bool {
        let global = self.code.vregs[vreg].global;
        global != LOCAL && bitset::contains(&self.out.live_out, global as usize)
    }

    /// The position at which a value that is live at the end of the block is wanted after it.
    pub(super) fn wanted_after_block(&self, vreg: VReg) -> Distance {
        let (end, data) = (self.code.blocks[self.block].end, &self.code.vregs[vreg]);
        if self.liveness.has_distances() {
            return end
                + self
                    .liveness
                    .distance_after(self.function, self.cfg, self.block, data.global);
        }
        match self.cfg.loop_of(self.block).expand() {
            Some(own) if bitset::contains(self.liveness.used_in(own), data.global as usize) => {
                end + SOON / 2
            }
            Some(_) => end + BEYOND_LOOP,
            None => end + (data.end / 2).saturating_sub(end).min(BEYOND_LOOP),
        }
    }

    /// Returns whether the value is dead after the position that the scan is at.
    #[inline]
    fn sight(&mut self, vreg: VReg) -> bool {
        if self.out.statuses[vreg].scanned_in == self.block {
            return false;
        }
        let is_dead = !self.is_live_out(vreg);
        let next = if is_dead {
            FARTHEST
        } else {
            self.wanted_after_block(vreg)
        };
        let status = &mut self.out.statuses[vreg];
        (status.scanned_in, status.next) = (self.block, next);
        is_dead
    }

    fn scan(&mut self) {
        let data = self.code.blocks[self.block];
        self.liveness
            .live_out(self.function, self.cfg, self.block, &mut self.out.live_out);
        self.out.next_uses.clear();
        for edge in self.function.successors(self.block) {
            let wanted = data.end
                + if self.function.is_rare(edge) {
                    BEYOND_LOOP
                } else {
                    0
                };
            for index in self.code.edge_args[edge].range() {
                if let Some(arg) = self.code.vreg_lists.item(index).expand() {
                    self.sight(arg);
                    let status = &mut self.out.statuses[arg];
                    status.next = status.next.min(wanted);
                }
            }
        }
        for position in (data.first..data.end).rev() {
            let head = self.code.heads[position as usize];
            if let Some(site) = head.site.expand() {
                self.scan_state(position, site);
            }
            let mut claimed = 0;
            let first_next = self.out.next_uses.len();
            self.out
                .next_uses
                .resize(first_next + usize::from(head.operand_count), FARTHEST);
            // Of two uses of one value, it is the late one after which it dies.
            for is_late_pass in [true, false] {
                for (offset, index) in head.operands().enumerate() {
                    let operand = self.code.operands[index];
                    if (operand.role == Role::Use) == is_late_pass {
                        continue;
                    }
                    if operand.place == Place::Fixed {
                        claimed |= bit(operand.fixed_register());
                    }
                    if !operand.vreg.is_none() {
                        let dies = self.sight(operand.vreg);
                        self.code.operands[index].set_dies(dies);
                        self.out.next_uses[first_next + offset] =
                            self.out.statuses[operand.vreg].next;
                    }
                }
            }
            for index in head.operands() {
                let operand = self.code.operands[index];
                if operand.is_use() && !matches!(operand.place, Place::ColdAnywhere | Place::Given)
                {
                    self.out.statuses[operand.vreg].next = position;
                }
            }
            if head.is_constrained {
                if let Some(site) = head.site.expand() {
                    let site = &self.code.sites[site];
                    claimed |= (site.clobbers_early | site.clobbers_late).bits();
                }
                self.out.claims.push(Claim {
                    position,
                    registers: claimed,
                });
            }
        }
    }

    fn scan_state(&mut self, position: u32, site: Site) {
        let site = self.code.sites[site];
        if site.state.is_none()
            || core::mem::replace(&mut self.out.scanned_states[site.state], self.block)
                == self.block
        {
            return;
        }
        for index in site.state_vregs.range() {
            let vreg = self.code.vreg_lists.item(index);
            if vreg != site.recoverable && !self.code.vregs[vreg].is_floating() && self.sight(vreg)
            {
                self.out.state_deaths.push((position, vreg));
            }
        }
    }

    // Registers

    #[inline]
    pub(super) fn bank_of(&self, vreg: VReg) -> Mask {
        self.banks[self.code.vregs[vreg].class.bank() as usize]
    }

    #[inline]
    pub(super) fn occupy(&mut self, reg: u8, vreg: VReg, is_clean: bool) {
        debug_assert!(self.occupied & bit(reg) == 0);
        let status = &mut self.out.statuses[vreg];
        status.reg = reg;
        self.value[usize::from(reg & 63)] = vreg;
        self.next[usize::from(reg & 63)] = status.next;
        self.occupied |= bit(reg);
        self.clean = self.clean & !bit(reg) | if is_clean { bit(reg) } else { 0 };
    }

    #[inline]
    pub(super) fn vacate(&mut self, reg: u8) {
        debug_assert!(self.occupied & bit(reg) != 0);
        self.out.statuses[self.value[usize::from(reg & 63)]].reg = AT_HOME;
        self.occupied &= !bit(reg);
    }

    #[inline]
    pub(super) fn is_clean(&self, reg: u8) -> bool {
        self.clean & bit(reg) != 0
            || self.out.statuses[self.value[usize::from(reg & 63)]].stored != Stored::OnDemand
    }

    #[inline]
    pub(super) fn home(&self, vreg: VReg) -> Spot {
        Spot::home(self.out.slots.owner(vreg))
    }

    /// Returns false if the value has to be stored at a place that is run with the given frequency.
    pub(super) fn try_to_store_at_definition(&mut self, vreg: VReg, frequency: f32) -> bool {
        self.out.slots.note_use(vreg);
        let data = &self.code.vregs[vreg];
        let last = self.code.blocks[data.block].end - 1;
        let ends_block = data.start / 2 == last && !self.code.heads[last as usize].site.is_none();
        let is_cheaper = !ends_block && self.frequencies.of_block(data.block) <= frequency;
        if is_cheaper {
            self.out.statuses[vreg].stored = Stored::AtDefinition;
        }
        is_cheaper
    }

    /// Where the value was when the walk came to the current instruction.
    fn origin(&self, vreg: VReg) -> Spot {
        let displaced = self.out.displaced.iter().find(|&&(known, _)| known == vreg);
        match displaced.map_or(self.out.statuses[vreg].reg, |&(_, reg)| reg) {
            AT_HOME => self.home(vreg),
            reg => Spot::register(reg),
        }
    }

    fn note_origin(&mut self, vreg: VReg) {
        self.epoch += 1;
        if !self.out.displaced.iter().any(|&(known, _)| known == vreg) {
            self.out.displaced.push((vreg, self.out.statuses[vreg].reg));
        }
    }

    fn make_clean(&mut self, reg: u8) {
        if !self.is_clean(reg) {
            let vreg = self.value[usize::from(reg & 63)];
            if !self.try_to_store_at_definition(vreg, self.frequencies.of_block(self.block)) {
                self.out.pending.push(Move::new(
                    self.origin(vreg),
                    self.home(vreg),
                    self.code.vregs[vreg].class,
                ));
            }
            self.clean |= bit(reg);
        }
    }

    fn evict(&mut self, reg: u8) {
        self.make_clean(reg);
        self.note_origin(self.value[usize::from(reg & 63)]);
        self.vacate(reg);
    }

    /// Adds a move from where the value was, in place of any other move to the register.
    fn fetch(&mut self, vreg: VReg, reg: u8) {
        let (from, to) = (self.origin(vreg), Spot::register(reg));
        self.out.pending.retain(|step| step.to != to);
        if from != to {
            self.out
                .pending
                .push(Move::new(from, to, self.code.vregs[vreg].class));
        }
    }

    /// Makes a free register the place of a value that is defined already.
    fn bring(&mut self, vreg: VReg, reg: u8) {
        let former = self.out.statuses[vreg].reg;
        self.note_origin(vreg);
        let mut is_clean = true;
        if former != AT_HOME {
            is_clean = self.is_clean(former);
            self.vacate(former);
            self.out
                .pending
                .retain(|step| step.to != Spot::register(former));
        } else if let Stored::OnDemand = self.out.statuses[vreg].stored {
            let home = self.home(vreg);
            is_clean =
                self.origin(vreg) == home || self.out.pending.iter().any(|step| step.to == home);
        }
        self.fetch(vreg, reg);
        self.occupy(reg, vreg, is_clean);
    }

    /// The register among `candidates` whose value is missed least.
    pub(super) fn victim(&self, candidates: Mask) -> Option<u8> {
        registers_in(candidates).max_by_key(|&reg| {
            let distance = u64::from(self.next[usize::from(reg)].saturating_sub(self.position));
            let vreg = self.value[usize::from(reg)];
            if self.code.vregs[vreg].is_floating() {
                distance * 4
            } else if self.is_clean(reg) {
                distance * 2
            } else {
                distance
            }
        })
    }

    /// Registers that others claim while the value is live, and those claimed where it is last used.
    fn contested(&self, vreg: VReg) -> (Mask, Mask) {
        let data = &self.code.vregs[vreg];
        let last = data.end / 2;
        let (mut meanwhile, mut finally) = (0, 0);
        let ahead = self
            .out
            .claims
            .iter()
            .rev()
            .filter(|claim| claim.position > self.position);
        for claim in ahead
            .take(CLAIMS_LOOKED_AT)
            .take_while(|claim| claim.position <= last)
        {
            *if claim.position == last {
                &mut finally
            } else {
                &mut meanwhile
            } |= claim.registers;
        }
        if data.global != LOCAL && data.end >= self.code.blocks[self.block].exit() {
            match self.cfg.loop_of(self.block).expand() {
                Some(own) => meanwhile |= self.code.loop_claims[own].bits(),
                None => {
                    for edge in self
                        .function
                        .successors(self.block)
                        .filter(|&edge| !self.function.is_rare(edge))
                        .take(2)
                    {
                        meanwhile |= self.code.blocks[self.function.target(edge)].claimed.bits();
                    }
                }
            }
        }
        (meanwhile, finally)
    }

    /// The best of `free`, which is not empty.
    pub(super) fn choose(&mut self, vreg: VReg, free: Mask) -> u8 {
        let (meanwhile, finally) = self.contested(vreg);
        let root = self.code.affinity_root(vreg);
        if let Some(preferred) = self.code.vregs[root].preferred {
            let preferred = preferred.index() as u8;
            if free & !meanwhile & bit(preferred) != 0 {
                return preferred;
            }
        }
        let mut best = free;
        for unwanted in [
            meanwhile,
            finally,
            self.reserved,
            self.registers.kept_by_calls.bits(),
        ] {
            if best & !unwanted != 0 {
                best &= !unwanted;
            }
        }
        best.trailing_zeros() as u8
    }

    #[inline]
    pub(super) fn settle_preference(&mut self, vreg: VReg, reg: u8) {
        let root = self.code.affinity_root(vreg);
        self.code.vregs[root]
            .preferred
            .get_or_insert_with(|| Reg::from_index(reg.into()));
    }

    /// Frees one of `allowed` if none is free.
    #[inline]
    fn free_for_result(&mut self, allowed: Mask, taken: &Taken) -> Mask {
        let free = allowed & !self.occupied;
        if free != 0 {
            return free;
        }
        let victim = self.victim(allowed & !taken.yielding).unwrap_or_else(|| {
            let victim = self
                .victim(allowed)
                .expect("an instruction needs more registers than there are");
            self.reassign_uses(victim, AT_HOME, |operand| operand.role == Role::LateUse);
            victim
        });
        self.evict(victim);
        bit(victim)
    }

    /// Frees one of `suitable` if none is free, for what is there when the instruction starts.
    fn free_at_start(&mut self, suitable: Mask, taken: &mut Taken) -> Mask {
        let allowed = suitable & !taken.blocked_early & !taken.read & !taken.held;
        let free = allowed & !self.occupied;
        if free != 0 {
            return free;
        }
        if allowed == 0 {
            return self.move_early_use_aside(suitable, taken);
        }
        let victim = self.victim(allowed & !taken.yielding).unwrap_or_else(|| {
            let victim = self.victim(allowed).unwrap();
            self.reassign_uses(victim, AT_HOME, |_| true);
            victim
        });
        self.evict(victim);
        bit(victim)
    }

    /// What is read at the start can make do with a register that is overwritten later.
    #[cold]
    fn move_early_use_aside(&mut self, suitable: Mask, taken: &mut Taken) -> Mask {
        let movable = suitable & taken.read & !taken.held & !taken.blocked_early;
        assert!(
            movable != 0,
            "an instruction needs more registers than there are"
        );
        let former = movable.trailing_zeros() as u8;
        let vreg = self.value[usize::from(former)];
        let reg = self
            .free_at_start(self.bank_of(vreg) & !suitable & !taken.read, taken)
            .trailing_zeros() as u8;
        self.make_clean(former);
        self.bring(vreg, reg);
        self.reassign_uses(former, reg, |_| true);
        taken.read = taken.read & !bit(former) | bit(reg);
        bit(former)
    }

    fn reassign_uses(&mut self, from: u8, to: u8, applies: impl Fn(Operand) -> bool) {
        for operand in &mut self.code.operands[self.code.heads[self.position as usize].operands()] {
            if operand.is_use()
                && operand.place != Place::Fixed
                && operand.assigned() == from
                && applies(*operand)
            {
                operand.assign(to);
            }
        }
    }

    // Instructions

    fn allocate_instruction(&mut self) {
        let head = self.code.heads[self.position as usize];
        let operands = head.operands();
        let first_next = self.out.next_uses.len() - operands.len();
        let mut taken = Taken::default();
        if head.is_constrained {
            self.make_way(operands.clone(), head.site, &mut taken);
        }

        let (mut has_absent_uses, mut has_temps) = (false, false);
        for index in operands.clone() {
            let operand = self.code.operands[index];
            match (operand.role, operand.place) {
                (_, Place::Given | Place::Fixed) | (Role::Def, _) => {}
                (Role::EarlyDef | Role::Temp, _) => has_temps = true,
                (Role::Use | Role::LateUse, _) => {
                    let reg = self.out.statuses[operand.vreg].reg;
                    let is_late = operand.role == Role::LateUse;
                    if reg == AT_HOME
                        || is_late
                            && taken.overwritten(self.code.vregs[operand.vreg].class) & bit(reg)
                                != 0
                    {
                        has_absent_uses = true;
                    } else {
                        self.assign_use(index, reg, &mut taken);
                    }
                }
            }
        }
        if has_absent_uses {
            self.bring_uses(operands.clone(), first_next, &mut taken);
        }
        if has_temps {
            self.assign_temps(operands.clone(), &mut taken);
        }

        for (index, next_index) in operands.clone().zip(first_next..) {
            let operand = self.code.operands[index];
            if operand.is_use() && operand.place != Place::Given {
                let status = &mut self.out.statuses[operand.vreg];
                status.next = self.out.next_uses[next_index];
                let reg = status.reg;
                if reg != AT_HOME {
                    self.next[usize::from(reg & 63)] = status.next;
                    if operand.role == Role::Use && operand.dies() {
                        self.vacate(reg);
                    }
                }
            }
        }
        if head.is_constrained {
            self.drop_overwritten(&taken);
        }
        let mut results = 0;
        for (index, next_index) in operands.clone().zip(first_next..) {
            let operand = self.code.operands[index];
            match operand.role {
                Role::Def => {
                    self.out.statuses[operand.vreg].next = self.out.next_uses[next_index];
                    let reg = self.register_for_result(operand, operands.start, &taken, results);
                    self.code.operands[index].assign(reg);
                    self.occupy(reg, operand.vreg, false);
                    results |= bit(reg);
                }
                Role::EarlyDef => {
                    self.out.statuses[operand.vreg].next = self.out.next_uses[next_index];
                    self.next[usize::from(operand.assigned() & 63)] =
                        self.out.next_uses[next_index];
                    results |= bit(operand.assigned());
                }
                _ => {}
            }
        }
        if let Some(site) = head.site.expand() {
            self.record_state(site, operands.clone());
        }
        if !self.out.pending.is_empty() {
            self.flush_moves();
        }
        self.out.displaced.clear();

        for index in operands {
            let operand = self.code.operands[index];
            if operand.dies() && operand.role != Role::Use {
                let reg = self.out.statuses[operand.vreg].reg;
                if reg != AT_HOME {
                    self.vacate(reg);
                }
            }
        }
        if let Some(site) = head.site.expand() {
            self.leave_site(site, results);
        }
        self.out.next_uses.truncate(first_next);
        if head.is_constrained {
            self.out.claims.pop();
        }
    }

    fn flush_moves(&mut self) {
        let Allocation {
            sequencer,
            pending,
            moves,
            ..
        } = &mut *self.out;
        let position = self.position;
        sequencer.sequence(pending, self.registers.swaps_gprs, |step| {
            moves.push((position, step))
        });
    }

    /// Clears the registers that the instruction claims, and fills those that operands have to be in.
    fn make_way(&mut self, operands: core::ops::Range<usize>, site: Site, taken: &mut Taken) {
        if let Some(site) = site.expand() {
            let site = &self.code.sites[site];
            taken.blocked_early = site.clobbers_early.bits();
            taken.blocked_late = site.clobbers_late.bits();
            taken.upper_halves = site.clobbers_upper_halves.bits();
        }
        let (mut in_place, mut dying, mut wanted_elsewhere) = (0, 0, 0);
        for &operand in &self.code.operands[operands.clone()] {
            let current = if operand.is_use() && operand.place != Place::Given {
                self.out.statuses[operand.vreg].reg
            } else {
                AT_HOME
            };
            let current_bit = if current == AT_HOME { 0 } else { bit(current) };
            if operand.role == Role::Use && operand.dies() {
                dying |= current_bit;
            }
            if operand.place != Place::Fixed {
                wanted_elsewhere |= current_bit;
                continue;
            }
            let fixed = bit(operand.fixed_register());
            in_place |= fixed & current_bit;
            if operand.role == Role::Def {
                taken.blocked_late |= fixed;
            } else {
                taken.blocked_early |= fixed;
                taken.held |= if operand.role == Role::Use { 0 } else { fixed };
            }
        }

        let unsafe_registers = taken.blocked_early | taken.blocked_late | taken.upper_halves;
        for reg in registers_in(self.occupied & unsafe_registers) {
            let vreg = self.value[usize::from(reg)];
            let overwritten = taken.overwritten(self.code.vregs[vreg].class);
            let must_leave = taken.blocked_early & !in_place & bit(reg) != 0;
            let survives = dying & bit(reg) == 0;
            if !must_leave && !(survives && overwritten & bit(reg) != 0) {
                continue;
            }
            if !survives && wanted_elsewhere & bit(reg) == 0 {
                self.note_origin(vreg);
                self.vacate(reg);
                continue;
            }
            let shelters = self.bank_of(vreg)
                & !self.occupied
                & !taken.blocked_early
                & !if survives { overwritten } else { 0 };
            let is_worth_a_register = !self.code.vregs[vreg].is_floating()
                && (!self.is_clean(reg)
                    || self.next[usize::from(reg)]
                        - self.position.min(self.next[usize::from(reg)])
                        < BEYOND_LOOP);
            if shelters != 0 && is_worth_a_register {
                let shelter = self.choose(vreg, shelters);
                self.bring(vreg, shelter);
            } else if must_leave {
                self.evict(reg);
            } else {
                self.make_clean(reg);
            }
        }

        for index in operands {
            let operand = self.code.operands[index];
            if operand.place != Place::Fixed {
                continue;
            }
            let reg = operand.fixed_register();
            self.code.operands[index].assign(reg);
            if !operand.is_use() {
                if operand.role == Role::EarlyDef {
                    self.occupy(reg, operand.vreg, false);
                }
                continue;
            }
            taken.read |= bit(reg);
            let current = self.out.statuses[operand.vreg].reg;
            if current == reg {
                continue;
            }
            let overwritten = taken.overwritten(self.code.vregs[operand.vreg].class);
            let dies_early = current != AT_HOME && dying & bit(current) != 0;
            let is_settled = current != AT_HOME && taken.read & bit(current) != 0;
            if !is_settled && (dies_early || overwritten & bit(reg) == 0 && current == AT_HOME) {
                self.bring(operand.vreg, reg);
                dying |= if dies_early { bit(reg) } else { 0 };
            } else {
                self.fetch(operand.vreg, reg);
            }
        }
    }

    #[inline]
    fn assign_use(&mut self, index: usize, reg: u8, taken: &mut Taken) {
        let operand = self.code.operands[index];
        self.code.operands[index].assign(reg);
        if self.can_be_read_at_home(operand) {
            taken.yielding |= bit(reg);
        } else {
            taken.read |= bit(reg);
            taken.held |= if operand.role == Role::LateUse {
                bit(reg)
            } else {
                0
            };
        }
    }

    #[inline]
    fn can_be_read_at_home(&self, operand: Operand) -> bool {
        let data = &self.code.vregs[operand.vreg];
        match operand.place {
            Place::Anywhere | Place::ColdAnywhere => true,
            Place::RegisterOrMemory => {
                !data.is_floating()
                    || self.registers.addresses_float_constants && data.class.bank() == Bank::Fpr
            }
            _ => false,
        }
    }

    fn bring_uses(
        &mut self,
        operands: core::ops::Range<usize>,
        first_next: usize,
        taken: &mut Taken,
    ) {
        for needs_register in [true, false] {
            for (index, next_index) in operands.clone().zip(first_next..) {
                let operand = self.code.operands[index];
                let is_open = operand.is_use()
                    && operand.place != Place::Fixed
                    && operand.assigned() == AT_HOME;
                if !is_open || self.can_be_read_at_home(operand) == needs_register {
                    continue;
                }
                let is_late = operand.role == Role::LateUse;
                let (bank, overwritten) = (
                    self.bank_of(operand.vreg),
                    taken.overwritten(self.code.vregs[operand.vreg].class),
                );
                let former = self.out.statuses[operand.vreg].reg;
                if former != AT_HOME && !(is_late && overwritten & bit(former) != 0) {
                    self.assign_use(index, former, taken);
                    continue;
                }
                let lasting = bank & !overwritten;
                let free_and_lasting =
                    lasting & !taken.blocked_early & !taken.read & !self.occupied;
                let free = if needs_register {
                    let has_choice = !is_late && (operand.dies() || free_and_lasting == 0);
                    self.free_at_start(if has_choice { bank } else { lasting }, taken)
                } else {
                    let wanted_in = self.out.next_uses[next_index] - self.position;
                    if operand.place == Place::ColdAnywhere
                        || operand.dies()
                        || wanted_in >= SOON
                        || free_and_lasting == 0
                    {
                        continue;
                    }
                    free_and_lasting
                };
                let reg = self.choose(operand.vreg, free);
                let former = self.out.statuses[operand.vreg].reg;
                self.bring(operand.vreg, reg);
                if former != AT_HOME {
                    self.reassign_uses(former, reg, |_| true);
                    taken.read &= !bit(former) | taken.blocked_early;
                }
                if !operand.dies() && overwritten & bit(reg) != 0 {
                    self.make_clean(reg);
                }
                self.assign_use(index, reg, taken);
            }
        }
    }

    fn assign_temps(&mut self, operands: core::ops::Range<usize>, taken: &mut Taken) {
        for index in operands {
            let operand = self.code.operands[index];
            if !matches!(operand.role, Role::EarlyDef | Role::Temp) || operand.place == Place::Fixed
            {
                continue;
            }
            let lasting = !taken.blocked_late & !taken.upper_halves;
            let reg = if operand.role == Role::Temp {
                let free =
                    self.free_at_start(self.banks[operand.temp_bank() as usize] & lasting, taken);
                let unwanted = self.registers.kept_by_calls.bits();
                (if free & !unwanted != 0 {
                    free & !unwanted
                } else {
                    free
                })
                .trailing_zeros() as u8
            } else {
                let free = self.free_at_start(self.bank_of(operand.vreg) & lasting, taken);
                let reg = self.choose(operand.vreg, free);
                self.settle_preference(operand.vreg, reg);
                self.occupy(reg, operand.vreg, false);
                reg
            };
            self.code.operands[index].assign(reg);
            taken.held |= bit(reg);
        }
    }

    /// Once the results have their registers.
    fn record_state(&mut self, site: Site, operands: core::ops::Range<usize>) {
        let data = &mut self.code.sites[site];
        if data.state.is_none() {
            return;
        }
        if (data.state, self.epoch) == (self.last_state.0, self.last_state.1) {
            data.state_assigned = self.last_state.2;
            return;
        }
        let place_of = |&vreg: &VReg| {
            if vreg == data.recoverable {
                let mut operands = self.code.operands[operands.clone()].iter();
                operands
                    .find(|operand| operand.vreg == vreg)
                    .map_or(AT_HOME, |operand| operand.assigned())
            } else if self.code.vregs[vreg].is_floating() {
                AT_HOME
            } else {
                self.out.statuses[vreg].reg
            }
        };
        data.state_assigned = self
            .out
            .state_assigned
            .add(self.code.vreg_lists[data.state_vregs].iter().map(place_of));
        self.last_state = (data.state, self.epoch, data.state_assigned);
    }

    fn drop_overwritten(&mut self, taken: &Taken) {
        for reg in registers_in(self.occupied & (taken.blocked_late | taken.upper_halves)) {
            if taken.overwritten(self.code.vregs[self.value[usize::from(reg)]].class) & bit(reg)
                != 0
            {
                debug_assert!(self.is_clean(reg));
                self.epoch += 1;
                self.vacate(reg);
            }
        }
    }

    #[inline]
    fn register_for_result(
        &mut self,
        operand: Operand,
        first_operand: usize,
        taken: &Taken,
        results: Mask,
    ) -> u8 {
        if operand.place == Place::Fixed {
            return operand.fixed_register();
        }
        let allowed = self.bank_of(operand.vreg)
            & !taken.blocked_late
            & !taken.upper_halves
            & !taken.held
            & !results;
        for tie in operand.ties() {
            let reg = self.code.operands[first_operand + tie].assigned();
            if usize::from(reg) < Reg::COUNT && allowed & !self.occupied & bit(reg) != 0 {
                if self.out.claims.is_empty() || self.contested(operand.vreg).0 & bit(reg) == 0 {
                    self.settle_preference(operand.vreg, reg);
                    return reg;
                }
            }
        }
        let free = self.free_for_result(allowed, taken);
        let reg = self.choose(operand.vreg, free);
        self.settle_preference(operand.vreg, reg);
        reg
    }

    fn leave_site(&mut self, site: Site, results: Mask) {
        while let Some(&(position, vreg)) = self.out.state_deaths.last()
            && position == self.position
        {
            self.out.state_deaths.pop();
            let reg = self.out.statuses[vreg].reg;
            if reg != AT_HOME {
                self.vacate(reg);
            }
        }
        let live = self.occupied & !results;
        let is_vector =
            |&reg: &u8| self.code.vregs[self.value[usize::from(reg)]].class == Class::Vector;
        let vectors = registers_in(live & self.banks[Bank::Fpr as usize]).filter(is_vector);
        let live_vectors = vectors.fold(0, |mask, reg| mask | bit(reg));
        let data = &mut self.code.sites[site];
        data.live_registers = RegisterSet::from_bits(live);
        data.live_vectors = RegisterSet::from_bits(live_vectors);
    }
}
