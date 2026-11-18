//! One pass over the instructions for what is likely, and one over what that left out.

#![forbid(unsafe_code)]

mod allocation;
mod arithmetic;
mod call;
mod control;
mod element;
mod frame;
mod iteration;
mod property;
mod scope;
mod suspension;
mod template;
mod test;

use bun_engine_asm::prelude::*;
use bun_engine_asm::{Code as MachineCode, CodeTooLarge, RegOrImm};
use bun_engine_bytecode::{
    Body, Code, Constant, Flow, Instruction, Jump, MetadataKind, Opcode, Operand, RawInstruction,
    Register, Shape,
};
use bun_engine_interpreter::outcome;
use bun_engine_vm::{Arithmetic, CellType, CodeBlock, Value};
use smallvec::SmallVec;

use crate::emit::{Emit, WORD, at, new_assembler};
use crate::profiles::{Likely, Profiles};
use crate::registers::*;
use crate::thunks::Thunks;

pub(crate) use call::ENTRY;
pub(crate) use property::emit_get_length;

/// Where the code of an instruction starts.
pub(crate) struct Start {
    pub(crate) offset: u32,
    pub(crate) label: Label,
    /// What it expects to find in `T0`.
    pub(crate) carried: Option<Register>,
}

pub(crate) struct Compiled {
    pub(crate) code: MachineCode,
    pub(crate) entry: Label,
    pub(crate) starts: Vec<Start>,
    /// Where the callees of an instruction return to.
    pub(crate) return_points: Vec<(u32, Label)>,
    /// What the code takes for recorded already, by the word of the profile.
    pub(crate) assumed: Vec<(u32, Arithmetic)>,
}

/// Code that is out of the way.
#[derive(Clone, Copy)]
struct Cold {
    emit: fn(&mut Compiler<'_>, &Cold),
    offset: u32,
    entry: Label,
    /// The end of the instruction, unless it asked for another place.
    resume: Label,
    /// Does all of the instruction.
    slow: Label,
    /// For what has a second way to go on.
    other: Label,
    carried: Option<Register>,
}

pub(crate) struct Compiler<'a> {
    m: MacroAssembler,
    code: &'a Code,
    body: &'a Body,
    profiles: Profiles<'a>,
    thunks: &'a Thunks,
    coroutine_type: CellType,
    /// By offset, for those that are jumped to.
    targets: Vec<Option<Label>>,
    /// By the index of the local.
    likely_locals: Vec<Likely>,
    cold: Vec<Cold>,
    starts: Vec<Start>,
    return_points: Vec<(u32, Label)>,
    assumed: Vec<(u32, Arithmetic)>,

    offset: u32,
    /// The register whose value is in `T0`.
    carried: Option<Register>,
    /// That value is a boolean, whichever way it came about.
    carries_boolean: bool,
    incoming: Option<Register>,
    is_incoming_boolean: bool,
    uses_incoming: bool,
    slow: Option<Label>,
    end: Option<Label>,
}

pub(crate) fn compile(
    code_block: &CodeBlock,
    thunks: &Thunks,
    seed: u64,
) -> Result<Compiled, CodeTooLarge> {
    let (code, body): (&Code, _) = (code_block.code(), code_block.body());
    let size = body.instructions().len();
    let mut compiler = Compiler {
        m: new_assembler(seed),
        code,
        body,
        profiles: Profiles::new(code_block),
        thunks,
        coroutine_type: suspension::coroutine_type(code_block),
        targets: vec![None; size],
        likely_locals: vec![Likely::Unknown; body.locals() as usize],
        cold: Vec::with_capacity(size / 4),
        starts: Vec::with_capacity(size / 4),
        return_points: Vec::new(),
        assumed: Vec::new(),
        offset: 0,
        carried: None,
        carries_boolean: false,
        incoming: None,
        is_incoming_boolean: false,
        uses_incoming: false,
        slow: None,
        end: None,
    };
    compiler.m.reserve(size * 12);
    compiler.find_targets();
    let entry = compiler.prologue();
    compiler.main_pass();
    compiler.cold_pass();
    Ok(Compiled {
        code: compiler.m.finish()?,
        entry,
        starts: compiler.starts,
        return_points: compiler.return_points,
        assumed: compiler.assumed,
    })
}

const fn slot(register: Register) -> Address {
    Address::new(FP, register.offset() * WORD)
}

/// A field of the entry that starts at `word`.
const fn metadata(word: u32, field: usize) -> Address {
    at(METADATA, word as usize * WORD as usize + field)
}

impl Compiler<'_> {
    fn raw(&self, offset: u32) -> RawInstruction {
        RawInstruction::read(self.body.instructions(), offset).expect("verified")
    }

    fn instruction(&self, offset: u32) -> Instruction {
        Instruction::read(self.body.instructions(), offset).0
    }

    fn find_targets(&mut self) {
        let (body, mut offset) = (self.body, 0);
        while let Some(instruction) = RawInstruction::read(body.instructions(), offset) {
            if instruction.opcode.branches() {
                instruction.for_each_target(body, offset, |target| {
                    self.label_at(target);
                });
            }
            offset += u32::from(instruction.size);
        }
        for handler in body.handlers() {
            self.label_at(handler.target);
        }
    }

    fn label_at(&mut self, offset: u32) -> Label {
        *self.targets[offset as usize].get_or_insert_with(|| self.m.new_label())
    }

    fn target(&mut self, jump: Jump) -> Label {
        self.label_at(jump.from(self.offset))
    }

    fn main_pass(&mut self) {
        let size = self.body.instructions().len() as u32;
        while self.offset < size {
            let raw = self.raw(self.offset);
            let label = match self.targets[self.offset as usize] {
                Some(label) => {
                    if raw.opcode == Opcode::LoopHint {
                        self.m.align(16, 10);
                    }
                    self.m.bind(label);
                    self.carried = None;
                    label
                }
                None => self.m.mark(),
            };
            self.incoming = self.carried.take();
            self.is_incoming_boolean = core::mem::take(&mut self.carries_boolean);
            self.uses_incoming = false;
            let (incoming, first_cold) = (self.incoming, self.cold.len());
            self.template();
            self.note_results(&raw);
            self.starts.push(Start {
                offset: self.offset,
                label,
                carried: incoming.filter(|_| self.uses_incoming),
            });
            self.finish_instruction(first_cold);
            self.offset += u32::from(raw.size);
        }
    }

    fn finish_instruction(&mut self, first_cold: usize) {
        if let Some(entry) = self.slow.take() {
            let resume = self.end();
            self.cold.push(Cold {
                emit: |c, cold| {
                    c.call_slow_path();
                    c.resume_with_carried(cold);
                },
                offset: self.offset,
                entry,
                resume,
                slow: entry,
                other: entry,
                carried: None,
            });
        }
        for cold in &mut self.cold[first_cold..] {
            cold.carried = self.carried;
        }
        if let Some(end) = self.end.take() {
            self.m.bind(end);
        }
    }

    fn cold_pass(&mut self) {
        (self.carried, self.incoming) = (None, None);
        for cold in core::mem::take(&mut self.cold) {
            self.offset = cold.offset;
            self.m.bind(cold.entry);
            (cold.emit)(self, &cold);
        }
    }

    /// Where to go, with nothing written yet, to have the instruction done the general way.
    fn slow(&mut self) -> Label {
        *self.slow.get_or_insert_with(|| self.m.new_label())
    }

    fn end(&mut self) -> Label {
        *self.end.get_or_insert_with(|| self.m.new_label())
    }

    fn cold(&mut self, emit: fn(&mut Compiler<'_>, &Cold)) -> Label {
        let resume = self.end();
        self.cold_until(resume, emit)
    }

    fn cold_until(&mut self, resume: Label, emit: fn(&mut Compiler<'_>, &Cold)) -> Label {
        let slow = self.slow();
        self.cold_with(resume, slow, emit)
    }

    /// For one that does not fall back on the slow path.
    fn cold_alone(&mut self, resume: Label, emit: fn(&mut Compiler<'_>, &Cold)) -> Label {
        self.cold_with(resume, resume, emit)
    }

    fn cold_with(
        &mut self,
        resume: Label,
        slow: Label,
        emit: fn(&mut Compiler<'_>, &Cold),
    ) -> Label {
        let entry = self.m.new_label();
        self.cold.push(Cold {
            emit,
            offset: self.offset,
            entry,
            resume,
            slow,
            other: slow,
            carried: None,
        });
        entry
    }

    fn resume_with_carried(&mut self, cold: &Cold) {
        if !self.raw(cold.offset).opcode.flow().falls_through() {
            return;
        }
        if let Some(register) = cold.carried {
            self.m.load64(T0, slot(register));
        }
        self.m.jump(cold.resume);
    }

    /// Falls through if the instruction does.
    fn call_slow_path(&mut self) {
        let raw = self.raw(self.offset);
        self.m.move32(ARGUMENTS[2], self.offset);
        self.m.call_address(self.thunks.slow_path(raw.opcode));
        let mut code = outcome::FIRST_TARGET;
        for (&shape, &word) in raw.opcode.shapes().iter().zip(raw.words()) {
            if shape == Shape::Target {
                let target = self.target(Jump(word as i32));
                if raw.opcode.flow() == Flow::Jumps {
                    return self.m.jump(target);
                }
                self.m.branch32(Cond::Eq, T0, code, target);
                code += 1;
            }
        }
        match raw.opcode {
            Opcode::SwitchInt | Opcode::SwitchChar | Opcode::SwitchString => {
                let mut targets: SmallVec<[(i64, Label); 16]> = SmallVec::new();
                raw.for_each_target(self.body, self.offset, |target| {
                    let code = outcome::OFFSET as i64 + i64::from(target);
                    targets.push((code, self.label_at(target)));
                });
                targets.sort_unstable();
                targets.dedup();
                let last = targets[targets.len() - 1].1;
                self.search(Width::W32, T0, &targets, last);
            }
            opcode if !opcode.flow().falls_through() => {
                self.m.move64(T0, SECOND_RESULT);
                self.epilogue();
            }
            _ => {}
        }
    }

    /// Jumps to the label of the key that `value` equals. They are sorted, as signed numbers.
    fn search(&mut self, width: Width, value: Gpr, keys: &[(i64, Label)], otherwise: Label) {
        if keys.len() <= 3 {
            for &(key, label) in keys {
                self.m.branch(width, Cond::Eq, value, Untrusted(key), label);
            }
            return self.m.jump(otherwise);
        }
        let (lower, upper) = keys.split_at(keys.len() / 2);
        let in_upper = self.m.new_label();
        (self.m).branch(width, Cond::Ge, value, Untrusted(upper[0].0), in_upper);
        self.search(width, value, lower, otherwise);
        self.m.bind(in_upper);
        self.search(width, value, upper, otherwise);
    }

    /// What the slot in the frame says while the instruction has called out.
    fn store_call_site(&mut self) {
        let start = self.body.instructions().as_ptr().addr() as u32;
        (self.m).store32(crate::emit::CALL_SITE, start.wrapping_add(self.offset));
    }

    fn restore_stack_pointer(&mut self) {
        let bottom = Address::new(FP, -(self.body.locals() as i32) * WORD);
        self.m.compute_address(SP, bottom);
    }

    /// Right after a call that may have run other code.
    fn return_point(&mut self) {
        let label = self.m.mark();
        self.return_points.push((self.offset, label));
        self.restore_stack_pointer();
        self.m.load_pinned_registers();
    }

    // Operands

    fn constant(&self, operand: Operand) -> Option<&Constant> {
        Some(&self.code.constants[operand.as_constant()? as usize])
    }

    fn int32_constant(&self, operand: Operand) -> Option<i32> {
        match self.constant(operand) {
            Some(&Constant::Int32(value)) => Some(value),
            _ => None,
        }
    }

    /// The bits of a constant that is no cell.
    fn immediate(&self, operand: Operand) -> Option<RegOrImm> {
        let trusted = |value: Value| RegOrImm::from(value.to_bits());
        Some(match *self.constant(operand)? {
            Constant::Empty => 0.into(),
            Constant::Undefined => trusted(Value::UNDEFINED),
            Constant::Null => trusted(Value::NULL),
            Constant::Boolean(value) => trusted(value.into()),
            Constant::Int32(value) => Untrusted(Value::int32(value).to_bits()).into(),
            Constant::Double(value) => Untrusted(Value::double(value).to_bits()).into(),
            _ => return None,
        })
    }

    fn is_never_cell(&self, operand: Operand) -> bool {
        self.immediate(operand).is_some()
    }

    /// Word `index` of what is below the metadata: the constants, the functions, the scopes, the argument profiles.
    fn below_metadata(&self, index: usize) -> Address {
        let code = self.code;
        let words = code.constants.len()
            + code.functions.len()
            + code.scopes.len()
            + self.body.parameters() as usize;
        Address::new(METADATA, (index as i32 - words as i32) * WORD)
    }

    fn load(&mut self, dst: Gpr, operand: impl Into<Operand>) {
        let operand = operand.into();
        match operand.as_register() {
            Some(register) if self.incoming == Some(register) => {
                self.uses_incoming = true;
                return self.m.move64(dst, T0);
            }
            Some(register) => self.m.load64(dst, slot(register)),
            None => match self.immediate(operand) {
                Some(bits) => self.m.move64(dst, bits),
                None => {
                    let index = operand.as_constant().unwrap_or_default();
                    self.m.load64(dst, self.below_metadata(index as usize));
                }
            },
        }
        if dst == T0 {
            self.incoming = None;
        }
    }

    /// Into `T0` and `T1`.
    fn load_both(&mut self, first: Operand, second: Operand) {
        self.load(T1, second);
        self.load(T0, first);
    }

    fn store(&mut self, dst: Register, src: impl Into<RegOrImm>) {
        self.m.store64(slot(dst), src);
    }

    /// The last step of an instruction whose result is in `T0`.
    fn store_result(&mut self, dst: Register) {
        self.store(dst, T0);
        self.carried = Some(dst);
    }

    fn store_profiled_result(&mut self, dst: Register, word: u32) {
        self.m.store64(metadata(word, 0), T0);
        self.store_result(dst);
    }

    /// `T0` is 0 or 1.
    fn store_boolean_result(&mut self, dst: Register) {
        self.m.box_boolean(T0, T0);
        self.store_result(dst);
        self.carries_boolean = true;
    }

    /// The instruction has loaded the operand into `T0`, left it there, and written no register.
    fn keep(&mut self, operand: impl Into<Operand>) {
        self.carried = operand.into().as_register();
    }

    /// The instruction has not touched `T0` on its usual way, nor the register whose value is there.
    fn pass_on_incoming(&mut self) {
        self.carried = self.incoming;
        self.uses_incoming |= self.incoming.is_some();
    }

    fn likely(&self, operand: Operand) -> Likely {
        let Some(register) = operand.as_register() else {
            return match self.constant(operand) {
                Some(Constant::Int32(_)) => Likely::Int32,
                Some(Constant::Double(_)) => Likely::Double,
                Some(Constant::Boolean(_)) => Likely::Boolean,
                _ if self.is_never_cell(operand) => Likely::Unknown,
                _ => Likely::Cell,
            };
        };
        match register.local_index() {
            Some(index) => self.likely_locals[index as usize],
            None => (register.offset() - Register::THIS.offset())
                .try_into()
                .map_or(Likely::Cell, |index| self.profiles.argument(index)),
        }
    }

    fn note_results(&mut self, raw: &RawInstruction) {
        let likely = match (raw.opcode, raw.opcode.metadata()) {
            (Opcode::Move, _) => self.likely(Operand::from_word(raw.words()[1])),
            _ if self.carries_boolean => Likely::Boolean,
            (_, Some(MetadataKind::UnaryArithmetic | MetadataKind::BinaryArithmetic)) => {
                let seen = self.profiles.arithmetic(raw.metadata);
                if seen.contains(Arithmetic::RESULT_DOUBLE) {
                    Likely::Double
                } else if seen.intersects(Arithmetic::RESULT_STRING | Arithmetic::RESULT_BIGINT) {
                    Likely::Cell
                } else {
                    Likely::Int32
                }
            }
            (_, Some(MetadataKind::NewObject | MetadataKind::NewArray))
            | (Opcode::NewFunction | Opcode::Concatenate | Opcode::Typeof | Opcode::ToString, _) => {
                Likely::Cell
            }
            _ => self.profiles.result(raw),
        };
        let mut likely = Some(likely);
        raw.for_each_def(self.body, |run, _| {
            for index in run.registers().filter_map(Register::local_index) {
                self.likely_locals[index as usize] = likely.take().unwrap_or_default();
            }
        });
    }

    // The write barrier

    /// After a store into `T1`. Leaves `T0` alone.
    fn barrier(&mut self) {
        let end = self.end();
        let remember = self.cold_alone(end, |c, cold| {
            c.m.call_address(c.thunks.write_barrier);
            c.m.jump(cold.resume);
        });
        self.m.branch_if_needs_barrier(T1, T2, remember);
    }

    /// The same, if `value`, which is in `reg`, is a cell.
    fn barrier_for(&mut self, value: Operand, reg: Gpr) {
        if self.is_never_cell(value) {
            return;
        }
        if self.likely(value) != Likely::Cell {
            let end = self.end();
            self.m.branch_if_not_cell(reg, end);
        }
        self.barrier();
    }
}
