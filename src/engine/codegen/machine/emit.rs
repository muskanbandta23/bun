//! Lays out the blocks and has the architecture emit their instructions.

use bun_engine_asm::{
    Address, Bank, FloatAssembly, Fpr, Gpr, Label, MacroAssembly, Reg, RegisterSet, Scratch, Width,
};
use smallvec::SmallVec;

use super::allocate::{Allocation, Stored};
use super::code::{AT_HOME, Class, Code, Home, Operand, Registers, Role, Site, VReg};
use super::lower::Instruction;
use super::moves::{Move, MoveKind, Spot, SpotKind};
use crate::analysis::{Cfg, Frequencies};
use crate::client::{CallSite, Client, ExitSite, Frame, PatchpointSite};
use crate::ir::{
    Action, Bits64, Block, Edge, Function, Input, Location, Op, Origin, Output, PatchpointFlags,
    Table, Type, Value,
};

pub(crate) trait Isa: Instruction {
    type Masm: FloatAssembly + MacroAssembly<Features = Self::Features>;

    fn registers(function: &Function) -> Registers;
    fn is_jump(self) -> bool;
    /// Leaves the macro assembler the registers that are never allocated.
    fn prepare(masm: &mut Self::Masm);
    fn emit<C: Client<Self::Masm>>(
        emitter: &mut Emitter<'_, Self, C>,
        instruction: Self,
        operands: Operands<'_>,
    );
    fn emit_move(
        masm: &mut Self::Masm,
        function: &Function,
        from: Location,
        to: Location,
        class: Class,
    );
    fn scratch(bank: Bank) -> Location;
    /// Computes the operand that `instruction` has overwritten with its result.
    fn recover(masm: &mut Self::Masm, instruction: Self, operands: Operands<'_>);
    /// Lets the client use the usual scratch registers if they are among the clobbered. Returns what undoes it.
    fn lend_scratch(masm: &mut Self::Masm, clobbered: RegisterSet) -> Scratch;
}

/// Where values live, now that the frame is laid out.
#[derive(Clone, Copy)]
pub(crate) struct Places<'a> {
    pub(crate) function: &'a Function,
    pub(crate) code: &'a Code,
    pub(crate) allocation: &'a Allocation,
    pub(crate) spare_offset: i32,
}

impl<'a> Places<'a> {
    pub(crate) fn home(self, vreg: VReg) -> Location {
        match self.code.vregs[vreg].home {
            Home::Spill => Location::Frame(self.allocation.slots.offset(vreg)),
            Home::Incoming(offset) => Location::Frame(offset),
            Home::FrameAddress(offset) => Location::FrameAddress(offset),
            Home::Constant(value) => match *self.function.op(value) {
                Op::Pinned(reg) => Location::Gpr(reg),
                Op::ConstV128(constant) => Location::VectorConstant(constant),
                _ => Location::Constant(Bits64::new(self.function.constant_bits(value).unwrap())),
            },
        }
    }

    #[inline]
    pub(crate) fn location(self, vreg: VReg, assigned: u8) -> Location {
        if assigned == AT_HOME {
            self.home(vreg)
        } else {
            register(Reg::from_index(assigned.into()))
        }
    }

    pub(crate) fn spot<I: Isa>(self, spot: Spot, class: Class) -> Location {
        match spot.kind() {
            SpotKind::Register(reg) => register(reg),
            SpotKind::Home(vreg) => self.home(vreg),
            SpotKind::Scratch => I::scratch(class.bank()),
            SpotKind::Spare => Location::Frame(self.spare_offset),
        }
    }

    /// The moves of an edge to a block that nothing else reaches are at the start of that block.
    pub(crate) fn moves_at_source(self, cfg: &Cfg, edge: Edge) -> &'a [Move] {
        if cfg.predecessors(self.function.target(edge)).len() == 1 {
            &[]
        } else {
            self.allocation.moves_on(edge)
        }
    }

    pub(crate) fn moves_at_start(
        self,
        cfg: &Cfg,
        block: Block,
    ) -> impl Iterator<Item = Move> + use<'a> {
        let incoming = if let [only] = *cfg.predecessors(block) {
            self.allocation.moves_on(only)
        } else {
            &[]
        };
        let params = self
            .code
            .params(block)
            .iter()
            .filter(|param| !param.is_none());
        let stores = params
            .filter_map(move |&param| self.store(param, self.allocation.entry_register(param)));
        incoming.iter().copied().chain(stores)
    }

    #[inline]
    fn store(self, vreg: VReg, reg: u8) -> Option<Move> {
        (self.allocation.stored(vreg) == Stored::AtDefinition).then(|| {
            Move::new(
                Spot::register(reg),
                Spot::home(vreg),
                self.code.vregs[vreg].class,
            )
        })
    }

    #[inline]
    pub(crate) fn moves_after(
        self,
        operands: &'a [Operand],
    ) -> impl Iterator<Item = Move> + use<'a> {
        let results = operands
            .iter()
            .take_while(|operand| matches!(operand.role, Role::Def | Role::EarlyDef));
        results.filter_map(move |result| self.store(result.vreg, result.assigned()))
    }
}

#[inline]
fn register(reg: Reg) -> Location {
    match reg.bank() {
        Bank::Gpr => Location::Gpr(reg.gpr()),
        Bank::Fpr => Location::Fpr(reg.fpr()),
    }
}

/// Hands out the operands of an instruction in the order in which lowering gave them.
#[derive(Clone)]
pub(crate) struct Operands<'a> {
    places: Places<'a>,
    rest: core::slice::Iter<'a, Operand>,
}

impl Operands<'_> {
    #[inline]
    pub(crate) fn assigned(&mut self) -> u8 {
        self.rest.next().unwrap().assigned()
    }

    #[inline]
    pub(crate) fn gpr(&mut self) -> Gpr {
        Gpr::new(self.assigned())
    }

    #[inline]
    pub(crate) fn fpr(&mut self) -> Fpr {
        Reg::from_index(self.assigned().into()).fpr()
    }

    #[inline]
    pub(crate) fn location(&mut self) -> Location {
        let operand = self.rest.next().unwrap();
        self.places.location(operand.vreg, operand.assigned())
    }

    pub(crate) fn skip(&mut self, count: usize) {
        self.rest.nth(count - 1);
    }

    pub(crate) fn class_of_next(&self) -> Class {
        self.places.code.vregs[self.rest.as_slice()[0].vreg].class
    }

    pub(crate) fn any_is_in(&self, reg: Gpr) -> bool {
        self.rest
            .as_slice()
            .iter()
            .any(|operand| operand.assigned() == reg.number())
    }
}

#[derive(Default)]
pub(crate) struct EmissionTables {
    layout: Vec<Block>,
    labels: Table<Block, Option<Label>>,
    /// Where a jump to the block really has to go.
    destinations: Table<Block, Block>,
    first_moves: Table<Block, u32>,
    detours: Vec<(Label, Edge)>,
    exits: Vec<(Label, u32)>,
    locations: Vec<Location>,
}

pub(crate) struct Emitter<'a, I: Isa, C> {
    pub(crate) masm: &'a mut I::Masm,
    pub(crate) places: Places<'a>,
    pub(crate) frame: &'a Frame,
    client: &'a mut C,
    cfg: &'a Cfg,
    frequencies: &'a Frequencies,
    instructions: &'a [I],
    tables: &'a mut EmissionTables,
    block: Block,
    next_block: Block,
    position: u32,
    /// The flags tell about what the last instruction wrote to this register.
    pub(crate) flags_describe: Option<(Gpr, Width)>,
}

pub struct Emitted {
    pub entries: Vec<Label>,
    /// The code from each label up to the next stems from the origin. Empty unless asked for.
    pub origins: Vec<(Label, Origin)>,
}

/// One edge of a branch is reached by a jump, and the code of the other follows.
pub(crate) struct Fork {
    pub(crate) label: Label,
    /// Jump if the condition does not hold.
    pub(crate) is_inverted: bool,
    other: Edge,
}

pub(crate) fn emit<I: Isa, C: Client<I::Masm>>(
    places: Places<'_>,
    cfg: &Cfg,
    frequencies: &Frequencies,
    instructions: &[I],
    frame: &Frame,
    tables: &mut EmissionTables,
    masm: &mut I::Masm,
    client: &mut C,
    records_origins: bool,
) -> Emitted {
    let mut emitter = Emitter {
        masm,
        places,
        frame,
        client,
        cfg,
        frequencies,
        instructions,
        tables,
        block: Block::NONE,
        next_block: Block::NONE,
        position: 0,
        flags_describe: None,
    };
    emitter.lay_out();
    emitter.masm.reserve(instructions.len() * 6);
    let mut emitted = Emitted {
        entries: Vec::new(),
        origins: Vec::new(),
    };
    let origins = records_origins.then_some(&mut emitted.origins);
    emitter.emit_blocks(origins);
    emitted.entries.extend(
        places
            .function
            .entries()
            .iter()
            .map(|entry| emitter.label(entry.block)),
    );
    emitted
}

impl<'a, I: Isa, C: Client<I::Masm>> Emitter<'a, I, C> {
    fn lay_out(&mut self) {
        let Places {
            function,
            code,
            allocation,
            ..
        } = self.places;
        let tables = &mut *self.tables;
        tables.labels.refill(function.block_count(), None);
        tables
            .destinations
            .refill(function.block_count(), Block::NONE);
        tables.first_moves.refill(function.block_count(), 0);
        tables.detours.clear();
        tables.exits.clear();
        tables.layout.clear();
        let mut end = allocation.moves.len();
        for &block in self.cfg.order().iter().rev() {
            let data = &code.blocks[block];
            let after = end;
            while end != 0 && allocation.moves[end - 1].0 >= data.first {
                end -= 1;
            }
            tables.first_moves[block] = end as u32;
            tables.destinations[block] = block;
            let only_jumps =
                data.end - data.first == 1 && self.instructions[data.first as usize].is_jump();
            if only_jumps && after == end && !self.cfg.predecessors(block).is_empty() {
                let edge = function.successor(block, 0);
                let (target, is_backward) =
                    (function.target(edge), self.cfg.is_backward(function, edge));
                let does_nothing = self.places.moves_at_source(self.cfg, edge).is_empty()
                    && self.places.moves_at_start(self.cfg, block).next().is_none();
                // A header may turn out to lead nowhere but to itself.
                if does_nothing && self.cfg.loop_headed_by(block).is_none() {
                    tables.destinations[block] = if is_backward {
                        target
                    } else {
                        tables.destinations[target]
                    };
                }
            }
        }
        for is_cold in [false, true] {
            let kept = |&&block: &&Block| {
                tables.destinations[block] == block && self.frequencies.is_cold(block) == is_cold
            };
            tables.layout.extend(self.cfg.order().iter().filter(kept));
        }
    }

    fn destination(&self, mut block: Block) -> Block {
        while self.tables.destinations[block] != block {
            block = self.tables.destinations[block];
        }
        block
    }

    fn label(&mut self, block: Block) -> Label {
        let block = self.destination(block);
        *self.tables.labels[block].get_or_insert_with(|| self.masm.new_label())
    }

    fn operands_at(&self, position: u32) -> &'a [Operand] {
        &self.places.code.operands[self.places.code.heads[position as usize].operands()]
    }

    fn cursor(&self, position: u32) -> Operands<'a> {
        Operands {
            places: self.places,
            rest: self.operands_at(position).iter(),
        }
    }

    fn emit_blocks(&mut self, mut origins: Option<&mut Vec<(Label, Origin)>>) {
        let (places, moves) = (self.places, &self.places.allocation.moves);
        let mut note = |masm: &mut I::Masm, position: u32| {
            if let Some(origins) = &mut origins {
                let origin = places.code.heads[position as usize].origin;
                if origins.last().is_none_or(|&(_, last)| last != origin) {
                    origins.push((masm.mark(), origin));
                }
            }
        };
        I::prepare(self.masm);
        for index in 0..self.tables.layout.len() {
            self.block = self.tables.layout[index];
            self.next_block = self
                .tables
                .layout
                .get(index + 1)
                .copied()
                .unwrap_or(Block::NONE);
            self.flags_describe = None;
            let data = places.code.blocks[self.block];
            let label = self.label(self.block);
            self.masm.bind(label);
            note(self.masm, data.first);
            if self.cfg.predecessors(self.block).is_empty() {
                let entry = places
                    .function
                    .entries()
                    .iter()
                    .position(|entry| entry.block == self.block)
                    .unwrap();
                self.client.prologue(self.masm, entry, self.frame);
            }
            places
                .moves_at_start(self.cfg, self.block)
                .for_each(|step| self.emit_move(step));
            let mut next_move = self.tables.first_moves[self.block] as usize;
            for position in data.first..data.end {
                self.position = position;
                note(self.masm, position);
                while let Some(&(before, step)) = moves.get(next_move)
                    && before == position
                {
                    self.emit_move(step);
                    next_move += 1;
                }
                I::emit(
                    self,
                    self.instructions[position as usize],
                    self.cursor(position),
                );
                places
                    .moves_after(self.operands_at(position))
                    .for_each(|step| self.emit_move(step));
            }
        }
        self.next_block = Block::NONE;
        for index in 0..self.tables.detours.len() {
            let (label, edge) = self.tables.detours[index];
            self.masm.bind(label);
            self.follow(edge);
        }
        for index in 0..self.tables.exits.len() {
            let (label, position) = self.tables.exits[index];
            self.masm.bind(label);
            note(self.masm, position);
            self.emit_exit(position);
        }
        self.client.out_of_line(self.masm);
    }

    fn emit_move(&mut self, step: Move) {
        let (from, to) = (
            self.places.spot::<I>(step.from, step.class),
            self.places.spot::<I>(step.to, step.class),
        );
        let keeps_flags = step.kind != MoveKind::Swap
            && matches!(
                from,
                Location::Gpr(_) | Location::Fpr(_) | Location::Frame(_)
            );
        if !keeps_flags
            || self
                .flags_describe
                .is_some_and(|(reg, _)| to == Location::Gpr(reg))
        {
            self.flags_describe = None;
        }
        if step.kind == MoveKind::Swap {
            self.masm.swap(from.gpr(), to.gpr());
        } else if from != to {
            I::emit_move(self.masm, self.places.function, from, to, step.class);
        }
    }

    // Control

    /// Does the moves of the edge and goes to its target.
    fn follow(&mut self, edge: Edge) {
        for &step in self.places.moves_at_source(self.cfg, edge) {
            self.emit_move(step);
        }
        let target = self.destination(self.places.function.target(edge));
        if target != self.next_block {
            let label = self.label(target);
            self.masm.jump(label);
        }
    }

    fn label_of_edge(&mut self, edge: Edge) -> Label {
        if self.places.moves_at_source(self.cfg, edge).is_empty() {
            return self.label(self.places.function.target(edge));
        }
        let label = self.masm.new_label();
        self.tables.detours.push((label, edge));
        label
    }

    pub(crate) fn jump(&mut self) {
        self.follow(self.places.function.successor(self.block, 0));
    }

    pub(crate) fn fork(&mut self) -> Fork {
        let function = self.places.function;
        let [then, or] = [0, 1].map(|position| function.successor(self.block, position));
        let then_is_likelier = self.frequencies.of_edge(then) >= self.frequencies.of_edge(or);
        let has_moves = |edge| !self.places.moves_at_source(self.cfg, edge).is_empty();
        let is_next = |edge| self.destination(function.target(edge)) == self.next_block;
        let (likelier, other) = if then_is_likelier {
            (then, or)
        } else {
            (or, then)
        };
        let likelier_follows =
            has_moves(likelier) || is_next(likelier) && !(has_moves(other) && is_next(other));
        let then_follows = then_is_likelier == likelier_follows;
        let (taken, other) = if then_follows { (or, then) } else { (then, or) };
        Fork {
            label: self.label_of_edge(taken),
            is_inverted: then_follows,
            other,
        }
    }

    pub(crate) fn finish_fork(&mut self, fork: &Fork) {
        self.follow(fork.other);
    }

    pub(crate) fn successor_labels(&mut self) -> SmallVec<[Label; 4]> {
        self.places
            .function
            .successors(self.block)
            .map(|edge| self.label_of_edge(edge))
            .collect()
    }

    pub(crate) fn last_value(&self) -> Value {
        self.places.function.last(self.block)
    }

    // The client

    fn site(&self, position: u32) -> Site {
        self.places.code.heads[position as usize].site
    }

    pub(crate) fn exit_label(&mut self) -> Label {
        let site = &self.places.code.sites[self.site(self.position)];
        let token = self.places.function.op(site.value).exit().unwrap().token;
        self.client
            .shared_exit(self.masm, token)
            .unwrap_or_else(|| {
                let label = self.masm.new_label();
                self.tables.exits.push((label, self.position));
                label
            })
    }

    fn push_state(&mut self, site: Site) {
        let (places, data) = (self.places, &self.places.code.sites[site]);
        let state = places.code.vreg_lists[data.state_vregs]
            .iter()
            .zip(&places.allocation.state_assigned[data.state_assigned]);
        self.tables
            .locations
            .extend(state.map(|(&vreg, &assigned)| places.location(vreg, assigned)));
    }

    fn emit_exit(&mut self, position: u32) {
        let site = self.site(position);
        let data = &self.places.code.sites[site];
        let operands = self.operands_at(position);
        let is_overwritten = |operand: &Operand| {
            operand.vreg == data.recoverable
                && operand.is_use()
                && operand.assigned() == operands[0].assigned()
        };
        if !data.recoverable.is_none() && operands.iter().any(is_overwritten) {
            I::recover(
                self.masm,
                self.instructions[position as usize],
                self.cursor(position),
            );
        }
        self.tables.locations.clear();
        self.push_state(site);
        self.client.exit(
            self.masm,
            &ExitSite {
                function: self.places.function,
                frame: self.frame,
                token: self.places.function.op(data.value).exit().unwrap().token,
                origin: self.places.code.heads[position as usize].origin,
                state: &self.tables.locations,
            },
        );
    }

    pub(crate) fn outgoing(&self, offset: i32) -> Location {
        Location::Frame(offset - self.frame.size as i32)
    }

    pub(crate) fn patchpoint(&mut self, mut operands: Operands<'_>) {
        let function = self.places.function;
        let site = self.site(self.position);
        let site_data = &self.places.code.sites[site];
        let Op::Patchpoint(patchpoint) = *function.op(site_data.value) else {
            unreachable!()
        };
        let data = function.patchpoint(patchpoint);
        let origin = self.places.code.heads[self.position as usize].origin;
        let (inputs, outputs) = (function.inputs(patchpoint), function.outputs(patchpoint));

        self.tables.locations.clear();
        for &(_, output) in outputs {
            let location = if let Output::Outgoing(offset) = output {
                self.outgoing(offset)
            } else {
                operands.location()
            };
            self.tables.locations.push(location);
        }
        let scratch_gprs: SmallVec<[Gpr; 4]> =
            (0..data.scratch_gprs).map(|_| operands.gpr()).collect();
        let scratch_fprs: SmallVec<[Fpr; 4]> =
            (0..data.scratch_fprs).map(|_| operands.fpr()).collect();
        for &input in inputs {
            let location = if let Input::Outgoing(offset) = input {
                self.outgoing(offset)
            } else {
                operands.location()
            };
            self.tables.locations.push(location);
        }
        self.push_state(site);
        let ends_block = site_data.value == self.last_value();
        let returns_to_first = ends_block && data.action != Action::Custom;
        let edges = function
            .successors(self.block)
            .filter(|_| ends_block)
            .skip(returns_to_first.into());
        let successors: SmallVec<[Label; 4]> = edges.map(|edge| self.label_of_edge(edge)).collect();
        let next_label = self.next_block.expand().map(|next| self.label(next));
        let (results, rest) = self.tables.locations.split_at(outputs.len());
        let (operands, state) = rest.split_at(inputs.len());

        let own_scratch = I::lend_scratch(self.masm, data.clobbers_early);
        match data.action {
            Action::Custom => self.client.patchpoint(
                self.masm,
                &PatchpointSite {
                    function,
                    frame: self.frame,
                    token: data.token,
                    origin,
                    results,
                    operands,
                    state,
                    scratch_gprs: &scratch_gprs,
                    scratch_fprs: &scratch_fprs,
                    live_registers: site_data.live_registers,
                    live_vectors: site_data.live_vectors,
                    falls_through_to: successors
                        .iter()
                        .position(|&label| Some(label) == next_label),
                    successors: &successors,
                },
            ),
            Action::Call(target) => {
                let target = self.masm.external(target);
                self.masm.call(target);
            }
            Action::CallIndirect => match operands[0] {
                Location::Frame(offset) => {
                    self.masm
                        .call_indirect(Address::new(I::Masm::FRAME_POINTER, offset));
                }
                callee => self.masm.call_indirect(callee.gpr()),
            },
        }
        let return_address = (data.action != Action::Custom).then(|| self.masm.mark());
        if data.flags.contains(PatchpointFlags::MOVES_STACK_POINTER) {
            let bottom = Address::new(I::Masm::FRAME_POINTER, -(self.frame.size as i32));
            self.masm.compute_address(I::Masm::STACK_POINTER, bottom);
        }
        if let Some(return_address) = return_address {
            let site = CallSite {
                function,
                frame: self.frame,
                token: data.token,
                origin,
                return_address,
                state,
                handlers: &successors,
            };
            self.client.after_call(self.masm, &site);
        }
        self.masm.set_scratch(own_scratch);
        if returns_to_first && function.successors(self.block).len() != 0 {
            if data.flags.contains(PatchpointFlags::UNEXTENDED_RESULTS) {
                for (&(ty, _), result) in outputs.iter().zip(&self.tables.locations) {
                    if ty == Type::I32 {
                        self.masm.zero_extend32(result.gpr(), result.gpr());
                    }
                }
            }
            self.jump();
        }
    }
}
