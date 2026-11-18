//! What instruction selection does alike for every architecture.
//!
//! It goes backward through blocks and instructions, so a use is seen before the definition, and the last use first.

use bun_engine_asm::{Bank, Gpr, Reg, RegisterSet, Sign};

use super::code::{
    BlockData, Class, Code, ForeignUse, Head, Home, LOCAL, NEVER, Operand, Place, Point, Role,
    Site, SiteData, VReg, VRegData,
};
use crate::analysis::{Cfg, Frequencies};
use crate::ir::{
    Action, Block, Boundary, EffectFlags, Entity, Function, Input, Op, Origin, Output, Patchpoint,
    PatchpointFlags, Slot, Span, State, Table, Type, Unary, Value,
};

/// The instructions that lowering itself needs of an architecture.
pub(crate) trait Instruction: Copy + core::fmt::Debug {
    type Features: Copy;

    /// Result, source.
    const COPY: Self;
    /// Result, source. Clears the upper half.
    const ZERO_EXTEND: Self;
    const JUMP: Self;
    const RETURN: Self;
    const UNREACHABLE: Self;
    /// Results, temporaries, operands.
    const PATCHPOINT: Self;
    const FRAME_POINTER: Gpr;

    /// Source, anywhere. The offset is from the stack pointer.
    fn store_outgoing(offset: i32, ty: Type) -> Self;
    fn load_outgoing(offset: i32, ty: Type) -> Self;
    /// Source, anywhere. The offset is from the frame pointer.
    fn store_result(offset: i32, ty: Type) -> Self;

    fn lower(lowering: &mut Lowering<'_, Self>, value: Value);
}

#[derive(Default)]
pub(crate) struct LoweringTables {
    uses: Table<Value, u32>,
    vregs: Table<Value, VReg>,
    is_absorbed: Table<Value, bool>,
    states: Table<State, (Span<VReg>, Block)>,
    scratch: Vec<VReg>,
    /// Arguments to extend with zeros before the block ends: result and source.
    extensions: Vec<(VReg, VReg)>,
}

pub(crate) struct Lowering<'a, I: Instruction> {
    pub(crate) features: I::Features,
    pub(crate) function: &'a mut Function,
    pub(crate) cfg: &'a Cfg,
    frequencies: &'a Frequencies,
    slot_offsets: &'a Table<Slot, i32>,
    code: &'a mut Code,
    instructions: &'a mut Vec<I>,
    tables: &'a mut LoweringTables,
    block: Block,
    is_cold: bool,
    origin: Origin,
    first_operand: usize,
    is_constrained: bool,
    site: Site,
    live_locals: [u32; 2],
    pressure: [u32; 2],
    defined_globals: [u32; 2],
    claimed: RegisterSet,
    allocatable: RegisterSet,
    barrier: [Option<u32>; 2],
}

/// How far a load is looked for from its user.
const REACH_OF_FUSION: usize = 16;

pub(crate) fn lower<I: Instruction>(
    function: &mut Function,
    cfg: &Cfg,
    frequencies: &Frequencies,
    features: I::Features,
    allocatable: RegisterSet,
    slot_offsets: &Table<Slot, i32>,
    code: &mut Code,
    instructions: &mut Vec<I>,
    tables: &mut LoweringTables,
) {
    count_uses(function, cfg, &mut tables.uses);
    tables.vregs.refill(function.value_count(), VReg::NONE);
    tables.is_absorbed.refill(function.value_count(), false);
    tables
        .states
        .refill(function.state_count(), (Span::default(), Block::NONE));
    instructions.clear();
    code.heads.clear();
    code.operands.clear();
    code.vregs.clear();
    code.globals.clear();
    code.foreign_uses.clear();
    code.sites.clear();
    code.vreg_lists.clear();
    code.blocks
        .refill(function.block_count(), BlockData::default());
    code.edge_args
        .refill(function.edge_count(), Span::default());
    code.loop_claims
        .refill(cfg.loops().len(), RegisterSet::EMPTY);
    code.outgoing_bytes = function.outgoing_bytes;
    let mut lowering = Lowering {
        features,
        function,
        cfg,
        frequencies,
        slot_offsets,
        code,
        instructions,
        tables,
        block: Block::NONE,
        is_cold: false,
        origin: Origin::default(),
        first_operand: 0,
        is_constrained: false,
        site: Site::NONE,
        live_locals: [0; 2],
        pressure: [0; 2],
        defined_globals: [0; 2],
        claimed: RegisterSet::EMPTY,
        allocatable,
        barrier: [None; 2],
    };
    for &block in cfg.order().iter().rev() {
        lowering.lower_block(block);
    }
    lowering.turn_around();
}

fn count_uses(function: &Function, cfg: &Cfg, uses: &mut Table<Value, u32>) {
    uses.refill(function.value_count(), 0);
    for &block in cfg.order() {
        let mut counted = State::NONE;
        for value in function.instructions(block) {
            function
                .operands(value)
                .iter()
                .for_each(|&operand| uses[operand] += 1);
            let state = function.state_of(value);
            if !state.is_none() && state != counted {
                function
                    .state(state)
                    .iter()
                    .for_each(|&operand| uses[operand] += 1);
                counted = state;
            }
        }
        for edge in function.successors(block) {
            function.args(edge).iter().for_each(|&arg| uses[arg] += 1);
        }
    }
}

impl<I: Instruction> Lowering<'_, I> {
    fn lower_block(&mut self, block: Block) {
        self.block = block;
        self.is_cold = self.frequencies.is_cold(block);
        (self.live_locals, self.pressure, self.defined_globals) = Default::default();
        self.claimed = RegisterSet::EMPTY;
        self.barrier = [None; 2];
        let after = self.instructions.len() as u32;

        let last = self.function.last(block);
        self.origin = self.function.origin(last);
        self.lower_args(block);
        self.lower_value(last);
        while let Some((extended, source)) = self.tables.extensions.pop() {
            self.push_def(Operand::tied(extended, 1, None));
            self.push_use(source, Role::Use, Place::RegisterOrMemory);
            self.finish(I::ZERO_EXTEND);
        }
        let mut value = self.function.previous(last);
        while !value.is_none() {
            let is_wanted =
                !self.tables.vregs[value].is_none() || self.function.effects(value).must_execute();
            if is_wanted && !self.tables.is_absorbed[value] {
                self.origin = self.function.origin(value);
                self.lower_value(value);
            }
            value = self.function.previous(value);
        }

        self.define_params(block);
        let data = &mut self.code.blocks[block];
        (data.first, data.end) = (self.instructions.len() as u32, after);
        data.pressure = [0, 1].map(|bank| self.pressure[bank] + self.defined_globals[bank]);
        data.claimed = self.claimed;
        data.barrier = self
            .barrier
            .map(|position| position.map_or(0, |position| data.first - position));
        if let Some(own) = self.cfg.loop_of(block).expand() {
            if !self.is_cold {
                self.code.loop_claims[own] |= self.claimed;
            }
            let data = &self.cfg.loops()[own];
            if let Some(parent) = data.parent.expand().filter(|_| data.header == block) {
                let inner = self.code.loop_claims[own];
                self.code.loop_claims[parent] |= inner;
            }
        }
    }

    fn lower_args(&mut self, block: Block) {
        for edge in self.function.successors(block) {
            let (target, count) = (self.function.target(edge), self.function.args(edge).len());
            let args = self
                .code
                .vreg_lists
                .add(core::iter::repeat_n(VReg::NONE, count));
            self.code.edge_args[edge] = args;
            let is_backward = self.cfg.is_backward(self.function, edge);
            for position in 0..count {
                let param = self.function.params(target)[position];
                if !self.is_materialized(param) && (!is_backward || self.tables.uses[param] == 0) {
                    continue;
                }
                let param = self.vreg(param);
                let mut arg = self.vreg(self.function.args(edge)[position]);
                if self.code.vregs[param].class == Class::Int32
                    && self.code.vregs[arg].class == Class::Int64
                {
                    let extended = self.new_vreg(Class::Int32);
                    self.tables.extensions.push((extended, arg));
                    arg = extended;
                }
                if self.note_use(arg, Role::LateUse, !self.function.is_rare(edge))
                    && self.ends_at_last_use(arg)
                {
                    self.code.join_affinities(arg, param);
                }
                self.code.vreg_lists[args][position] = arg;
            }
        }
    }

    /// No loop around this block comes back for the value.
    #[inline]
    fn ends_at_last_use(&self, vreg: VReg) -> bool {
        let data = &self.code.vregs[vreg];
        !data.is_floating()
            && self
                .cfg
                .loop_of(self.block)
                .expand()
                .is_none_or(|own| self.cfg.includes(own, data.block))
    }

    fn define_params(&mut self, block: Block) {
        let vregs = &self.tables.vregs;
        let params = self.code.vreg_lists.add(
            self.function
                .params(block)
                .iter()
                .map(|&param| vregs[param]),
        );
        self.code.blocks[block].params = params;
        // After turning around this is the point at which the first instruction reads.
        let entry = self.reversed_point(Role::Use) - 2;
        for position in 0..params.len() {
            if let Some(param) = self.code.vreg_lists[params][position].expand() {
                self.note_def(param, entry);
            }
        }
    }

    fn turn_around(&mut self) {
        self.instructions.reverse();
        self.code.heads.reverse();
        let count = self.instructions.len() as u32;
        for &block in self.cfg.order() {
            let data = &mut self.code.blocks[block];
            (data.first, data.end) = (count - data.first, count - data.end);
        }
        let turn = |point: Point| {
            if point == NEVER {
                NEVER
            } else {
                count * 2 - 1 - point
            }
        };
        for data in self.code.vregs.values_mut() {
            (data.start, data.end) = (turn(data.start), turn(data.end));
        }
        for foreign in &mut self.code.foreign_uses {
            foreign.first_warm = turn(foreign.first_warm);
        }
    }

    /// Points count from the end while lowering goes on.
    #[inline]
    fn reversed_point(&self, role: Role) -> Point {
        self.instructions.len() as Point * 2
            + Point::from(matches!(role, Role::Use | Role::EarlyDef | Role::Temp))
    }

    fn lower_value(&mut self, value: Value) {
        match *self.function.op(value) {
            Op::Jump => self.finish(I::JUMP),
            Op::Unreachable => self.finish(I::UNREACHABLE),
            Op::Return(_) => self.lower_return(value),
            Op::Patchpoint(patchpoint) => self.lower_patchpoint(value, patchpoint),
            Op::Extract(..) => {}
            Op::Unary(Unary::Widen(Sign::Unsigned), narrow) => {
                self.push_def(Operand::tied(self.tables.vregs[value], 1, None));
                self.use_value(narrow, Role::Use, Place::RegisterOrMemory);
                self.finish(I::ZERO_EXTEND);
            }
            _ => I::lower(self, value),
        }
    }

    // Values

    #[inline]
    pub(crate) fn op(&self, value: Value) -> Op {
        *self.function.op(value)
    }

    #[inline]
    pub(crate) fn type_of(&self, value: Value) -> Type {
        self.function.type_of(value)
    }

    #[inline]
    pub(crate) fn int_constant(&self, value: Value) -> Option<i64> {
        self.function.int_constant(value)
    }

    #[inline]
    pub(crate) fn constant_bits(&self, value: Value) -> Option<u64> {
        self.function.constant_bits(value)
    }

    #[inline]
    pub(crate) fn use_count(&self, value: Value) -> u32 {
        self.tables.uses[value]
    }

    /// Some later instruction needs the value in a register of its own.
    #[inline]
    pub(crate) fn is_materialized(&self, value: Value) -> bool {
        !self.tables.vregs[value].is_none()
    }

    pub(crate) fn slot_offset(&self, slot: Slot) -> i32 {
        self.slot_offsets[slot]
    }

    pub(crate) fn intern(&mut self, ty: Type, bits: u64) -> Value {
        let value = self.function.constant(ty, bits);
        self.tables.vregs.grow_to(value.index() + 1, VReg::NONE);
        self.tables.uses.grow_to(value.index() + 1, 0);
        value
    }

    /// Computing a pure value again here does not move work into a loop.
    #[inline]
    pub(crate) fn is_nearby(&self, value: Value) -> bool {
        let home = self.function.block_of(value);
        home == self.block
            || !home.is_none() && self.cfg.loop_of(home) == self.cfg.loop_of(self.block)
    }

    #[inline]
    pub(crate) fn is_in_this_block(&self, value: Value) -> bool {
        self.function.block_of(value) == self.block
    }

    /// Whether a load that has one use can be done as late as `user`.
    pub(crate) fn can_absorb(&self, value: Value, user: Value) -> bool {
        if self.tables.uses[value] != 1
            || !self.is_in_this_block(value)
            || self.is_materialized(value)
        {
            return false;
        }
        let effects = self.function.effects(value);
        if effects.has(EffectFlags::FENCE) {
            return false;
        }
        let mut between = self.function.previous(user);
        for _ in 0..REACH_OF_FUSION {
            if between == value {
                return true;
            }
            if between.is_none() {
                return false;
            }
            let other = self.function.effects(between);
            let keeps_order = if effects.has(EffectFlags::EXITS) {
                other.must_execute() || other.has(EffectFlags::CONTROL_DEPENDENT)
            } else {
                other.has(EffectFlags::FENCE)
                    || self.function.may_alias(effects.reads, other.writes)
            };
            if keeps_order {
                return false;
            }
            between = self.function.previous(between);
        }
        false
    }

    pub(crate) fn absorb(&mut self, value: Value) {
        self.tables.is_absorbed[value] = true;
    }

    /// The register of an I32 has its upper half clear.
    pub(crate) fn is_zero_extended(&self, mut value: Value) -> bool {
        loop {
            value = match self.op(value) {
                Op::Unary(Unary::Opaque, inner) | Op::Extract(inner, _) => inner,
                Op::Unary(Unary::Wrap, _) => return false,
                Op::Param(_) => {
                    return self.boundary_of(value).is_none_or(|boundary| {
                        self.function.params_are_extended
                            && matches!(boundary, Boundary::Register(_))
                    });
                }
                Op::Patchpoint(patchpoint) => {
                    return !self
                        .function
                        .patchpoint(patchpoint)
                        .flags
                        .contains(PatchpointFlags::UNEXTENDED_RESULTS);
                }
                _ => return true,
            }
        }
    }

    /// For a parameter of an entry.
    fn boundary_of(&self, param: Value) -> Option<Boundary> {
        let (Op::Param(position), block) = (self.op(param), self.function.block_of(param)) else {
            return None;
        };
        if !self.cfg.predecessors(block).is_empty() {
            return None;
        }
        let entry = *self
            .function
            .entries()
            .iter()
            .find(|entry| entry.block == block)?;
        Some(self.function.entry_params(entry)[position as usize])
    }

    /// The value whose register already holds the bits of `value`.
    pub(crate) fn representative(&self, mut value: Value) -> Value {
        loop {
            value = match self.op(value) {
                Op::Unary(Unary::Wrap | Unary::Opaque, inner) => inner,
                Op::Unary(Unary::Widen(Sign::Unsigned), inner) if self.is_zero_extended(inner) => {
                    inner
                }
                _ => return value,
            }
        }
    }

    fn new_vreg(&mut self, class: Class) -> VReg {
        self.add_vreg(class, Home::Spill, self.block)
    }

    #[inline]
    fn add_vreg(&mut self, class: Class, home: Home, block: Block) -> VReg {
        let vreg = VReg::from_index(self.code.vregs.len());
        self.code.vregs.push(VRegData {
            class,
            preferred: None,
            home,
            block,
            global: LOCAL,
            start: NEVER,
            end: NEVER,
            affinity: vreg,
            foreign_use: u32::MAX,
        })
    }

    #[inline]
    fn vreg(&mut self, value: Value) -> VReg {
        let value = self.representative(value);
        self.tables.vregs[value]
            .expand()
            .unwrap_or_else(|| self.create_vreg(value))
    }

    fn create_vreg(&mut self, value: Value) -> VReg {
        let ty = self.type_of(value);
        let class = if ty == Type::I32 && !self.is_zero_extended(value) {
            Class::Int64
        } else {
            Class::of(ty)
        };
        let block = self.function.block_of(value);
        let vreg = match self.op(value) {
            Op::Extract(tuple, position) => VReg(self.tuple_vregs(tuple).0 + position),
            Op::SlotAddress(slot) => self.add_vreg(
                class,
                Home::FrameAddress(self.slot_offsets[slot]),
                self.block,
            ),
            Op::Pinned(reg) if reg == I::FRAME_POINTER => {
                self.add_vreg(class, Home::FrameAddress(0), self.block)
            }
            Op::Param(_) => match self.boundary_of(value) {
                Some(Boundary::Frame(offset)) => {
                    self.add_vreg(class, Home::Incoming(offset), block)
                }
                _ => self.add_vreg(class, Home::Spill, block),
            },
            op if op.is_floating() => self.add_vreg(class, Home::Constant(value), self.block),
            _ => self.add_vreg(class, Home::Spill, block),
        };
        self.tables.vregs[value] = vreg;
        vreg
    }

    /// The first of those for the elements, which are consecutive.
    fn tuple_vregs(&mut self, tuple: Value) -> VReg {
        if let Some(first) = self.tables.vregs[tuple].expand() {
            return first;
        }
        let first = VReg::from_index(self.code.vregs.len());
        let (block, is_extended) = (self.function.block_of(tuple), self.is_zero_extended(tuple));
        let mut position = 0;
        while let Some(ty) = self.function.tuple_element_type(tuple, position) {
            self.add_vreg(
                if ty == Type::I32 && !is_extended {
                    Class::Int64
                } else {
                    Class::of(ty)
                },
                Home::Spill,
                block,
            );
            position += 1;
        }
        self.tables.vregs[tuple] = first;
        first
    }

    fn result_vreg(&mut self, value: Value, position: usize) -> VReg {
        if self.type_of(value) == Type::Tuple {
            VReg(self.tuple_vregs(value).0 + position as u32)
        } else {
            self.tables.vregs[value]
                .expand()
                .unwrap_or_else(|| self.create_vreg(value))
        }
    }

    /// Returns whether this is the last use.
    #[inline]
    fn note_use(&mut self, vreg: VReg, role: Role, is_warm: bool) -> bool {
        let point = self.reversed_point(role);
        let data = &mut self.code.vregs[vreg];
        let is_last = data.end == NEVER;
        data.end = data.end.min(point);
        if data.block != self.block || data.is_floating() {
            self.note_foreign_use(vreg, if is_warm { point } else { NEVER });
        } else if is_last {
            let bank = data.class.bank() as usize;
            self.live_locals[bank] += 1;
            self.pressure[bank] = self.pressure[bank].max(self.live_locals[bank]);
        }
        is_last
    }

    fn note_foreign_use(&mut self, vreg: VReg, warm_point: Point) {
        let data = &mut self.code.vregs[vreg];
        match self.code.foreign_uses.get_mut(data.foreign_use as usize) {
            Some(known) if known.block == self.block => {
                if warm_point != NEVER {
                    known.first_warm = warm_point;
                }
            }
            known => {
                // A constant that one block uses, and that once, is no concern of other blocks.
                let stays_local =
                    data.is_floating() && known.is_none() && self.cfg.loop_of(self.block).is_none();
                if data.global == LOCAL && !stays_local {
                    data.global = self.code.globals.len() as u32;
                    self.code.globals.push(vreg);
                }
                data.foreign_use = self.code.foreign_uses.len() as u32;
                self.code.foreign_uses.push(ForeignUse {
                    block: self.block,
                    vreg,
                    first_warm: warm_point,
                });
            }
        }
    }

    #[inline]
    fn note_def(&mut self, vreg: VReg, point: Point) {
        let data = &mut self.code.vregs[vreg];
        data.start = point;
        let bank = data.class.bank() as usize;
        if data.global != LOCAL {
            self.defined_globals[bank] += 1;
        } else if data.end != NEVER {
            self.live_locals[bank] = self.live_locals[bank].saturating_sub(1);
        }
    }

    // Operands. Results come first.

    #[inline]
    fn push_def(&mut self, operand: Operand) {
        self.note_def(operand.vreg, self.reversed_point(operand.role));
        self.code.operands.push(operand);
    }

    #[inline]
    fn push_use(&mut self, vreg: VReg, role: Role, place: Place) {
        let is_last = self.note_use(vreg, role, place != Place::ColdAnywhere);
        if is_last && let Some(&result) = self.code.operands.get(self.first_operand) {
            let position = self.code.operands.len() - self.first_operand;
            let is_tied = result.role == Role::Def && result.place == Place::Register;
            if is_tied && result.ties().next() == Some(position) && self.ends_at_last_use(vreg) {
                self.code.join_affinities(vreg, result.vreg);
            }
        }
        self.code.operands.push(Operand::new(vreg, role, place));
    }

    fn claim(&mut self, registers: RegisterSet) {
        self.claimed |= registers;
        self.is_constrained = true;
    }

    #[inline]
    pub(crate) fn def(&mut self, value: Value) {
        let vreg = self.result_vreg(value, 0);
        self.push_def(Operand::new(vreg, Role::Def, Place::Register));
    }

    /// The code is shorter if the result is in the register of the operand at one of the positions.
    #[inline]
    pub(crate) fn def_tied(&mut self, value: Value, first: u8, second: Option<u8>) {
        let vreg = self.result_vreg(value, 0);
        self.push_def(Operand::tied(vreg, first, second));
    }

    pub(crate) fn def_early(&mut self, value: Value, position: usize) {
        let vreg = self.result_vreg(value, position);
        self.push_def(Operand::new(vreg, Role::EarlyDef, Place::Register));
    }

    pub(crate) fn def_element(&mut self, tuple: Value, position: usize) {
        let vreg = self.result_vreg(tuple, position);
        self.push_def(Operand::new(vreg, Role::Def, Place::Register));
    }

    pub(crate) fn def_fixed(&mut self, value: Value, position: usize, reg: impl Into<Reg>) {
        let (vreg, reg) = (self.result_vreg(value, position), reg.into());
        self.claim(RegisterSet::EMPTY.with(reg));
        self.push_def(Operand::fixed(vreg, Role::Def, reg));
    }

    pub(crate) fn def_fixed_early(&mut self, value: Value, reg: impl Into<Reg>) {
        let (vreg, reg) = (self.result_vreg(value, 0), reg.into());
        self.claim(RegisterSet::EMPTY.with(reg));
        self.push_def(Operand::fixed(vreg, Role::EarlyDef, reg));
    }

    /// A result that is of no use.
    pub(crate) fn overwrite(&mut self, reg: Gpr) {
        let vreg = self.new_vreg(Class::Int64);
        self.claim(RegisterSet::EMPTY.with(reg.into()));
        self.push_def(Operand::fixed(vreg, Role::Def, reg.into()));
    }

    pub(crate) fn temp(&mut self, bank: Bank) {
        let bank_index = bank as usize;
        self.pressure[bank_index] = self.pressure[bank_index].max(self.live_locals[bank_index] + 1);
        self.code.operands.push(Operand::temp(bank));
    }

    pub(crate) fn temp_fixed(&mut self, reg: impl Into<Reg>) {
        let reg = reg.into();
        self.claim(RegisterSet::EMPTY.with(reg));
        self.code
            .operands
            .push(Operand::fixed(VReg::NONE, Role::Temp, reg));
    }

    /// The zero register and the like have an index that no allocatable register has.
    pub(crate) fn given(&mut self, reg: u8) {
        self.code.operands.push(Operand::given(reg));
    }

    #[inline]
    pub(crate) fn use_value(&mut self, value: Value, role: Role, place: Place) {
        let value = self.representative(value);
        if let Op::Pinned(reg) = self.op(value) {
            return self.given(reg.number());
        }
        let vreg = self.tables.vregs[value]
            .expand()
            .unwrap_or_else(|| self.create_vreg(value));
        self.push_use(vreg, role, place);
    }

    #[inline]
    pub(crate) fn use_register(&mut self, value: Value) {
        self.use_value(value, Role::Use, Place::Register);
    }

    #[inline]
    pub(crate) fn use_late(&mut self, value: Value) {
        self.use_value(value, Role::LateUse, Place::Register);
    }

    pub(crate) fn use_fixed(&mut self, value: Value, role: Role, reg: impl Into<Reg>) {
        let (vreg, reg) = (self.vreg(value), reg.into());
        self.claim(RegisterSet::EMPTY.with(reg));
        if self.note_use(vreg, role, true) && !self.is_cold {
            let root = self.code.affinity_root(vreg);
            self.code.vregs[root].preferred.get_or_insert(reg);
        }
        self.code.operands.push(Operand::fixed(vreg, role, reg));
    }

    /// For an instruction that exits to the state of `value`. Follows the operands.
    pub(crate) fn exit_of(&mut self, value: Value, recoverable: Option<Value>) {
        let recoverable = recoverable.map_or(VReg::NONE, |operand| self.vreg(operand));
        self.add_site(value, recoverable, [RegisterSet::EMPTY; 3]);
    }

    fn add_site(
        &mut self,
        value: Value,
        recoverable: VReg,
        [early, late, upper_halves]: [RegisterSet; 3],
    ) {
        let state = self.function.state_of(value);
        let state_vregs = if state.is_none() {
            Span::default()
        } else {
            self.use_state(state, recoverable)
        };
        let is_wanted =
            !recoverable.is_none() && self.code.vreg_lists[state_vregs].contains(&recoverable);
        let recoverable = if is_wanted { recoverable } else { VReg::NONE };
        if !(early | late | upper_halves).is_empty() {
            self.claim(early | late);
            for bank in [Bank::Gpr, Bank::Fpr] {
                if (self.allocatable - early - late).in_bank(bank).is_empty() {
                    self.barrier[bank as usize] = Some(self.instructions.len() as u32);
                }
            }
        }
        self.site = self.code.sites.push(SiteData {
            value,
            state,
            state_vregs,
            recoverable,
            clobbers_early: early,
            clobbers_late: late,
            clobbers_upper_halves: upper_halves,
            state_assigned: Span::default(),
            live_registers: RegisterSet::EMPTY,
            live_vectors: RegisterSet::EMPTY,
        });
    }

    fn use_state(&mut self, state: State, recoverable: VReg) -> Span<VReg> {
        let (mut vregs, noted_in) = self.tables.states[state];
        if noted_in == self.block {
            return vregs;
        }
        let count = self.function.state(state).len();
        if noted_in.is_none() {
            vregs = self
                .code
                .vreg_lists
                .add(core::iter::repeat_n(VReg::NONE, count));
            for position in 0..count {
                self.code.vreg_lists[vregs][position] =
                    self.vreg(self.function.state(state)[position]);
            }
        }
        for position in 0..count {
            let vreg = self.code.vreg_lists[vregs][position];
            if !self.code.vregs[vreg].is_floating() {
                self.note_use(
                    vreg,
                    if vreg == recoverable {
                        Role::Use
                    } else {
                        Role::LateUse
                    },
                    false,
                );
            }
        }
        self.tables.states[state] = (vregs, self.block);
        vregs
    }

    /// Ends the instruction whose operands have been given. The next one precedes it.
    #[inline]
    pub(crate) fn finish(&mut self, instruction: I) {
        let end = self.code.operands.len();
        self.code.heads.push(Head {
            first_operand: self.first_operand as u32,
            operand_count: (end - self.first_operand) as u16,
            is_constrained: core::mem::take(&mut self.is_constrained),
            site: core::mem::replace(&mut self.site, Site::NONE),
            origin: self.origin,
        });
        self.instructions.push(instruction);
        self.first_operand = end;
    }

    /// `value = source`, for an operation that turns out to do nothing.
    pub(crate) fn copy(&mut self, value: Value, source: Value) {
        self.def_tied(value, 1, None);
        self.use_value(source, Role::Use, Place::Anywhere);
        self.finish(if self.type_of(value) == Type::I32 {
            I::ZERO_EXTEND
        } else {
            I::COPY
        });
    }

    /// A value that the next instruction, which has been lowered already, reads from `copy`.
    fn copy_vreg(&mut self, copy: VReg, source: Value) {
        self.push_def(Operand::tied(copy, 1, None));
        self.use_value(source, Role::Use, Place::Anywhere);
        let narrows = self.code.vregs[copy].class == Class::Int32;
        self.finish(if narrows { I::ZERO_EXTEND } else { I::COPY });
    }

    fn lower_return(&mut self, value: Value) {
        let count = self.function.results().len();
        let overwrites_params = self
            .function
            .results()
            .iter()
            .any(|result| matches!(result, Boundary::Frame(_)));
        self.tables.scratch.clear();
        for position in 0..count {
            let result = self.function.operands(value)[position];
            let vreg = self.vreg(result);
            let data = &self.code.vregs[vreg];
            let needs_extension = self.type_of(result) == Type::I32 && data.class == Class::Int64;
            let needs_copy =
                needs_extension || overwrites_params && matches!(data.home, Home::Incoming(_));
            let class = Class::of(self.type_of(result));
            let used = if needs_copy {
                self.new_vreg(class)
            } else {
                vreg
            };
            self.tables.scratch.push(used);
            if let Boundary::Register(reg) = self.function.results()[position] {
                self.claim(RegisterSet::EMPTY.with(reg));
                if self.note_use(used, Role::Use, true) {
                    let root = self.code.affinity_root(used);
                    self.code.vregs[root].preferred.get_or_insert(reg);
                }
                self.code
                    .operands
                    .push(Operand::fixed(used, Role::Use, reg));
            }
        }
        self.finish(I::RETURN);
        for position in 0..count {
            if let Boundary::Frame(offset) = self.function.results()[position] {
                self.push_use(self.tables.scratch[position], Role::Use, Place::Anywhere);
                self.finish(I::store_result(
                    offset,
                    self.type_of(self.function.operands(value)[position]),
                ));
            }
        }
        for position in 0..count {
            let (used, result) = (
                self.tables.scratch[position],
                self.function.operands(value)[position],
            );
            if used != self.vreg(result) {
                self.copy_vreg(used, result);
            }
        }
    }

    fn lower_patchpoint(&mut self, value: Value, patchpoint: Patchpoint) {
        let data = *self.function.patchpoint(patchpoint);
        let result_count = self.function.outputs(patchpoint).len();
        for position in (0..result_count).rev() {
            if let (ty, Output::Outgoing(offset)) = self.function.outputs(patchpoint)[position] {
                self.code.outgoing_bytes = self
                    .code
                    .outgoing_bytes
                    .max(offset as u32 + ty.bytes().max(8));
                let vreg = self.result_vreg(value, position);
                if self.code.vregs[vreg].end != NEVER {
                    self.push_def(Operand::new(vreg, Role::Def, Place::Register));
                    self.finish(I::load_outgoing(offset, ty));
                }
            }
        }

        for position in 0..result_count {
            match self.function.outputs(patchpoint)[position].1 {
                Output::Register => self.def_element(value, position),
                Output::EarlyRegister => self.def_early(value, position),
                Output::Fixed(reg) => self.def_fixed(value, position, reg),
                Output::Outgoing(_) => {}
            }
        }
        (0..data.scratch_gprs).for_each(|_| self.temp(Bank::Gpr));
        (0..data.scratch_fprs).for_each(|_| self.temp(Bank::Fpr));
        self.add_site(
            value,
            VReg::NONE,
            [
                data.clobbers_early,
                data.clobbers_late,
                data.clobbers_upper_halves,
            ],
        );
        self.tables.scratch.clear();
        let operand_count = self.function.inputs(patchpoint).len();
        let extends = !data.flags.contains(PatchpointFlags::UNEXTENDED_OPERANDS);
        for position in 0..operand_count {
            let (operand, input) = (
                self.function.operands(value)[position],
                self.function.inputs(patchpoint)[position],
            );
            let is_in_register = !matches!(
                input,
                Input::Anywhere | Input::ColdAnywhere | Input::LateAnywhere | Input::Outgoing(_)
            );
            let needs_extension = extends
                && is_in_register
                && self.type_of(operand) == Type::I32
                && !self.is_zero_extended(operand);
            let copy = if input == Input::ConsumedRegister || needs_extension {
                self.new_vreg(Class::of(self.type_of(operand)))
            } else {
                VReg::NONE
            };
            self.tables.scratch.push(copy);
            let (role, place) = match input {
                Input::Anywhere if position == 0 && data.action == Action::CallIndirect => {
                    (Role::Use, Place::RegisterOrMemory)
                }
                Input::Anywhere => (Role::Use, Place::Anywhere),
                Input::ColdAnywhere => (Role::Use, Place::ColdAnywhere),
                Input::LateAnywhere => (Role::LateUse, Place::Anywhere),
                Input::Register => (Role::Use, Place::Register),
                Input::LateRegister | Input::ConsumedRegister => (Role::LateUse, Place::Register),
                Input::Fixed(reg) | Input::LateFixed(reg) => {
                    let role = if input == Input::Fixed(reg) {
                        Role::Use
                    } else {
                        Role::LateUse
                    };
                    if copy.is_none() {
                        self.use_fixed(operand, role, reg);
                    } else {
                        self.claim(RegisterSet::EMPTY.with(reg));
                        self.note_use(copy, role, true);
                        self.code.vregs[copy].preferred = Some(reg);
                        self.code.operands.push(Operand::fixed(copy, role, reg));
                    }
                    continue;
                }
                Input::Outgoing(_) => continue,
            };
            if copy.is_none() {
                self.use_value(operand, role, place);
            } else {
                self.note_use(copy, role, true);
                self.code.operands.push(Operand::new(copy, role, place));
            }
        }
        self.finish(I::PATCHPOINT);

        for position in (0..operand_count).rev() {
            let (operand, copy) = (
                self.function.operands(value)[position],
                self.tables.scratch[position],
            );
            if let Input::Outgoing(offset) = self.function.inputs(patchpoint)[position] {
                let ty = self.type_of(operand);
                self.code.outgoing_bytes = self
                    .code
                    .outgoing_bytes
                    .max(offset as u32 + ty.bytes().max(8));
                self.use_value(operand, Role::Use, Place::Anywhere);
                self.finish(I::store_outgoing(offset, ty));
            } else if !copy.is_none() {
                self.copy_vreg(copy, operand);
            }
        }
    }
}
