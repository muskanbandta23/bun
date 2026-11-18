//! Code that all baseline code of the process shares.

#![forbid(unsafe_code)]

use std::sync::OnceLock;

use bun_engine_asm::prelude::*;
use bun_engine_bytecode::Opcode;
use bun_engine_exec_memory::{CodeHandle, Priority};
use bun_engine_interpreter::{SLOW_PATHS, fixed_call_preparation, varargs_call_preparation};
use bun_engine_text::StringImpl;
use bun_engine_vm::ic::{CallProfile, CallTargetCount};
use bun_engine_vm::{
    Arithmetic, CallFrame, CellType, CodeBlock, GlobalObject, JsCell, JsFunction, JsString,
    RealmWatchpoint, TypeFlags, Value, Vm, call,
};

use crate::compiler::{ENTRY, emit_get_length};
use crate::emit::*;
use crate::ic::{Entries, emit_handlers};
use crate::registers::*;
use crate::runtime;

const UNKNOWN: i32 = 2;

macro_rules! thunks {
    ($($(#[$doc:meta])* $name:ident,)*) => {
        pub(crate) struct Thunks {
            $($(#[$doc])* pub(crate) $name: usize,)*
            /// By opcode: the slow path or, of a call, the first step and the second.
            steps: [[usize; 2]; Opcode::COUNT],
            pub(crate) handlers: Vec<usize>,
            pub(crate) entries: Entries<usize>,
            _code: CodeHandle,
        }

        fn generate() -> Option<Thunks> {
            let mut emitter = Emitter::new();
            $(let $name = emitter.routine(Emitter::$name);)*
            let steps = Opcode::ALL.map(|opcode| emitter.steps(opcode));
            let handlers = emit_handlers(&mut emitter.m, emitter.write_barrier);
            let entries = Entries::emit(&mut emitter.m, emitter.write_barrier);
            let code = emitter.m.finish().ok()?;
            let handle = bun_engine_exec_memory::link(&code, Priority::Required, |_| 0).ok()?;
            handle.describe(|| "baseline thunks".into());
            let address = |label| handle.address(code.offset_of(label));
            Some(Thunks {
                $($name: address($name),)*
                steps: steps.map(|labels| labels.map(|label| label.map_or(0, address))),
                handlers: handlers.into_iter().map(address).collect(),
                entries: entries.map(address),
                _code: handle,
            })
        }
    };
}

thunks! {
    /// For the cell in `T1`. Keeps `T0`.
    write_barrier,
    pad_arguments,
    finish_prologue,
    /// With the offset of the instruction in the third argument register.
    tier_up_in_loop,
    /// Goes on at the instruction that the call site stands for.
    resume,
    catch,
    /// With the callee in `T0` and the profile in `T1`, which becomes zero unless the call can go ahead. Keeps `T0` and
    /// those from `T5` on.
    check_callee,
    /// With the new frame in `T1`.
    tail_call,
    /// `T0` and `T1` into `F0` and `F1`. `T2` becomes zero unless both are numbers.
    to_doubles,
    /// Of `T0` and `T1` as doubles, with the profile in `T4`. These three leave zero if there is more to it.
    add,
    subtract,
    multiply,
    /// These three leave the answer in `T0`, or more than 1 if they have none.
    to_boolean,
    strict_equal,
    loose_equal,
    get_length,
    /// Whether iterating over `T1`, which it keeps, is to go through the elements of an array.
    is_pristine_array,
}

static THUNKS: OnceLock<Option<Thunks>> = OnceLock::new();

/// None if there is no memory to run code from.
pub(crate) fn get() -> Option<&'static Thunks> {
    THUNKS.get_or_init(generate).as_ref()
}

impl Thunks {
    /// These take the offset of the instruction in the third argument register, and do not return if it throws.
    pub(crate) fn slow_path(&self, opcode: Opcode) -> usize {
        self.steps[opcode as usize][0]
    }

    pub(crate) fn prepare_call(&self, opcode: Opcode) -> usize {
        self.steps[opcode as usize][0]
    }

    pub(crate) fn size_frame(&self, opcode: Opcode) -> usize {
        self.steps[opcode as usize][0]
    }

    /// With the new frame in the fourth.
    pub(crate) fn fill_frame(&self, opcode: Opcode) -> usize {
        self.steps[opcode as usize][1]
    }
}

struct Emitter {
    m: MacroAssembler,
    throw: Label,
    write_barrier: Label,
}

impl Emitter {
    fn new() -> Emitter {
        let mut m = new_assembler(0);
        let throw = m.external_address(call::throw_from_frame());
        let write_barrier = m.new_label();
        Emitter {
            m,
            throw,
            write_barrier,
        }
    }

    fn routine(&mut self, emit: impl FnOnce(&mut Self)) -> Label {
        self.m.align(16, 15);
        let start = self.m.here();
        emit(self);
        start
    }

    /// Calls a function of the `Vm`, the frame and whatever is in the argument registers after those.
    fn call_with_frame(&mut self, function: usize) {
        self.m.move64(ARGUMENTS[0], VM);
        self.m.move64(ARGUMENTS[1], FP);
        self.m.enter_frame();
        self.m.call_address(function);
        self.m.leave_frame();
    }

    fn step(&mut self, function: usize, prepares_call: bool) -> Option<Label> {
        Some(self.routine(|e| {
            let instructions = at(CODE_BLOCK, CodeBlock::OFFSET_OF_INSTRUCTIONS);
            e.m.add64(ARGUMENTS[2], ARGUMENTS[2], instructions);
            e.call_with_frame(function);
            e.m.branch_if_zero64(T0, e.throw);
            if prepares_call {
                e.m.move64(EXECUTABLE, SECOND_RESULT);
                e.m.move64(ENTRY, T0);
            }
            e.m.ret();
        }))
    }

    fn steps(&mut self, opcode: Opcode) -> [Option<Label>; 2] {
        if let Some(slow_path) = SLOW_PATHS[opcode as usize] {
            [self.step(slow_path as usize, false), None]
        } else if let Some(prepare) = fixed_call_preparation(opcode) {
            [self.step(prepare as usize, true), None]
        } else if let Some((size, fill)) = varargs_call_preparation(opcode) {
            [
                self.step(size as usize, false),
                self.step(fill as usize, true),
            ]
        } else {
            [None; 2]
        }
    }

    fn write_barrier(&mut self) {
        self.m.bind(self.write_barrier);
        self.m.enter_frame();
        self.m.push_pair(T0, T1);
        self.m.move64(ARGUMENTS[0], T1);
        self.m.call_address(runtime::remember as *const () as usize);
        self.m.pop_pair(T0, T1);
        self.m.leave_frame();
        self.m.ret();
    }

    /// The lowest address of the frame of the code block in `code_block`.
    fn frame_bottom(&mut self, dst: Gpr, code_block: Gpr) {
        self.m
            .load32(dst, at(code_block, CodeBlock::OFFSET_OF_LOCALS));
        self.m.shl64(dst, dst, 3);
        self.m.neg64(dst, dst);
        self.m.add64(dst, dst, FP);
    }

    fn pad_arguments(&mut self) {
        let (m, parameters) = (&mut self.m, ARGUMENTS[0]);
        let overflow = m.new_label();
        m.load64(T1, at(FP, CallFrame::OFFSET_OF_CODE_BLOCK));
        m.load32(parameters, at(T1, CodeBlock::OFFSET_OF_PARAMETERS));
        m.shl64(T1, parameters, 3);
        m.sub64(T1, FP, T1);
        m.branch64(
            Cond::Below,
            T1,
            at(VM, Vm::OFFSET_OF_REAL_STACK_LIMIT),
            overflow,
        );
        m.jump_address(call::pad_arguments());
        m.bind(overflow);
        m.leave_frame();
        m.jump_address(bun_engine_interpreter::entry());
    }

    fn return_unless_told_to_jump(&mut self) {
        let jump = self.m.new_label();
        self.m.branch_if_not_zero64(T0, jump);
        self.m.ret();
        self.m.bind(jump);
        self.m.jump_indirect(T0);
    }

    fn finish_prologue(&mut self) {
        self.call_with_frame(runtime::finish_prologue as *const () as usize);
        self.return_unless_told_to_jump();
    }

    fn tier_up_in_loop(&mut self) {
        self.call_with_frame(runtime::tier_up_in_loop as *const () as usize);
        self.return_unless_told_to_jump();
    }

    /// `load_offset` puts the offset of the instruction in the second argument register.
    fn take_over(&mut self, load_offset: impl FnOnce(&mut MacroAssembler)) {
        self.m.load_tags();
        self.m.load_pinned_registers();
        self.frame_bottom(T0, CODE_BLOCK);
        self.m.move64(SP, T0);
        load_offset(&mut self.m);
        self.m.move64(ARGUMENTS[0], FP);
        self.m
            .call_address(runtime::find_start as *const () as usize);
        self.m.move64(T4, T0);
        self.m.move64(T0, SECOND_RESULT);
        self.m.jump_indirect(T4);
    }

    fn resume(&mut self) {
        self.take_over(|m| {
            let instructions = at(CODE_BLOCK, CodeBlock::OFFSET_OF_INSTRUCTIONS);
            m.load32(ARGUMENTS[1], CALL_SITE);
            m.sub32(ARGUMENTS[1], ARGUMENTS[1], instructions);
        });
    }

    fn catch(&mut self) {
        self.take_over(|m| m.load32(ARGUMENTS[1], at(VM, Vm::OFFSET_OF_CATCH_BYTECODE_OFFSET)));
    }

    fn check_callee(&mut self) {
        let m = &mut self.m;
        let [unknown, known, counted] = core::array::from_fn(|_| m.new_label());
        m.branch_if_not_cell(T0, unknown);
        m.branch_unless_type_in(T0, CellType::FUNCTIONS, T2, unknown);
        m.load64(T2, at(T0, JsFunction::OFFSET_OF_EXECUTABLE));
        m.load64(T3, at(T1, CallProfile::OFFSET_OF_EXECUTABLE));
        m.branch64(Cond::Eq, T2, T3, known);
        m.branch64(Cond::Eq, T3, CallProfile::MEGAMORPHIC, known);
        m.load64(T1, at(T1, CallProfile::OFFSET_OF_TARGETS));
        m.branch_if_zero64(T1, unknown);
        let size = size_of::<CallTargetCount>();
        m.add64(T4, T1, size * CallProfile::MAX_TARGETS);
        let next = m.here();
        m.load64(T3, at(T1, core::mem::offset_of!(CallTargetCount, target)));
        m.branch64(Cond::Eq, T3, T2, counted);
        m.branch64(Cond::Eq, T3, T0, counted);
        m.add64(T1, T1, size);
        m.branch64(Cond::Below, T1, T4, next);
        m.bind(unknown);
        m.move32(T1, 0);
        m.ret();
        m.bind(counted);
        m.add32_to_memory(at(T1, core::mem::offset_of!(CallTargetCount, count)), 1);
        m.bind(known);
        m.move32(T1, 1);
        m.ret();
    }

    fn tail_call(&mut self) {
        let m = &mut self.m;
        let (new_frame, target, count) = (T1, T2, T5);
        let round_up = |m: &mut MacroAssembler, dst: Gpr, count: Gpr| {
            m.add32(dst, count, CallFrame::HEADER_WORDS + 1);
            m.and32(dst, dst, -2);
        };
        m.load32(T0, ARGUMENT_COUNT);
        m.load32(T6, at(CODE_BLOCK, CodeBlock::OFFSET_OF_PARAMETERS));
        m.select32(Cond::Below, T0, T0, T6, T6, T0);
        round_up(m, T0, T0);
        m.load32(count, at(new_frame, CallFrame::OFFSET_OF_ARGUMENT_COUNT));
        round_up(m, T6, count);
        m.sub64(T0, T0, T6);
        m.compute_address(target, Address::indexed(FP, T0, Scale::Eight, 0));
        m.load_pair64(T6, T7, at(FP, 0));
        m.add32(count, count, CallFrame::HEADER_WORDS - 1);
        let next = m.here();
        m.load64(T0, Address::indexed(new_frame, count, Scale::Eight, 0));
        m.store64(Address::indexed(target, count, Scale::Eight, 0), T0);
        m.sub32(count, count, 1);
        m.branch32(
            Cond::AboveEq,
            count,
            CallFrame::LINKAGE_SIZE as i32 / WORD,
            next,
        );
        m.store_pair64(at(target, 0), T6, T7);
        m.move64(FP, target);
        m.leave_frame();
        m.jump_indirect(ENTRY);
    }

    fn to_doubles(&mut self) {
        let other = self.m.new_label();
        self.m.number_to_double(F0, T0, T2, other);
        self.m.number_to_double(F1, T1, T2, other);
        self.m.move32(T2, 1);
        self.m.ret();
        self.m.bind(other);
        self.m.move32(T2, 0);
        self.m.ret();
    }

    /// Adds what it is to what is in `T3`.
    fn classify(&mut self, dst: Fpr, value: Gpr, kinds: [Arithmetic; 2], otherwise: Label) {
        let (int32, done) = (self.m.new_label(), self.m.new_label());
        self.m.branch_if_int32(value, int32);
        self.m.branch_if_not_number(value, otherwise);
        self.m.unbox_double(dst, value, T2);
        self.m.or32(T3, T3, i32::from(kinds[1].bits()));
        self.m.jump(done);
        self.m.bind(int32);
        self.m.convert_i32_to_f64(dst, value);
        self.m.or32(T3, T3, i32::from(kinds[0].bits()));
        self.m.bind(done);
    }

    /// Two int32 get here if the result is none, which is for the slow path to record the first time.
    fn arithmetic(&mut self, operate: fn(&mut MacroAssembler)) {
        let (other, mixed) = (self.m.new_label(), self.m.new_label());
        self.m.branch_unless_both_int32(T0, T1, T2, mixed);
        let overflowed = i32::from(Arithmetic::RESULT_OVERFLOWED_INT32.bits());
        (self.m).branch_test16(TestCond::Zero, at(T4, 0), overflowed, other);
        self.m.bind(mixed);
        (self.m).move32(T3, i32::from(Arithmetic::RESULT_DOUBLE.bits()));
        self.classify(
            F0,
            T0,
            [Arithmetic::LHS_INT32, Arithmetic::LHS_DOUBLE],
            other,
        );
        self.classify(
            F1,
            T1,
            [Arithmetic::RHS_INT32, Arithmetic::RHS_DOUBLE],
            other,
        );
        operate(&mut self.m);
        self.m.branch_f64_with_zero(FloatCond::Eq, F0, other);
        self.m.or_to_memory(Size::S16, at(T4, 0), T3);
        self.m.box_double(T0, F0);
        self.m.ret();
        self.answer(other, 0);
    }

    fn add(&mut self) {
        self.arithmetic(|m| m.add_f64(F0, F0, F1));
    }

    fn subtract(&mut self) {
        self.arithmetic(|m| m.sub_f64(F0, F0, F1));
    }

    fn multiply(&mut self) {
        self.arithmetic(|m| m.mul_f64(F0, F0, F1));
    }

    fn answer(&mut self, label: Label, answer: i32) {
        self.m.bind(label);
        self.m.move32(T0, answer);
        self.m.ret();
    }

    fn to_boolean(&mut self) {
        let m = &mut self.m;
        let [yes, no, unknown, cell, number, double, primitive] =
            core::array::from_fn(|_| m.new_label());
        m.branch_if_cell(T0, cell);
        m.branch_if_number(T0, number);
        m.compare64(Cond::Eq, T0, T0, TRUE);
        m.ret();
        m.bind(number);
        m.branch_if_not_int32(T0, double);
        m.test32(TestCond::NonZero, T0, T0, -1);
        m.ret();
        m.bind(double);
        m.unbox_double(F0, T0, T0);
        m.branch_f64_with_zero(FloatCond::Ne, F0, yes);
        m.jump(no);
        m.bind(cell);
        m.branch_if_cell_is_no_object(T0, primitive);
        m.branch_if_flag(T0, TypeFlags::MASQUERADES_AS_UNDEFINED, unknown);
        m.jump(yes);
        m.bind(primitive);
        m.branch_if_not_type(T0, CellType::String, unknown);
        m.load_string_length(T1, T0);
        m.test32(TestCond::NonZero, T0, T1, -1);
        m.ret();
        self.answer(yes, 1);
        self.answer(no, 0);
        self.answer(unknown, UNKNOWN);
    }

    /// Of two different strings.
    fn compare_strings(&mut self, no: Label, unknown: Label) {
        let m = &mut self.m;
        m.load8(T2, at(T0, JsCell::OFFSET_OF_FLAGS));
        m.load8(T3, at(T1, JsCell::OFFSET_OF_FLAGS));
        m.load64(T0, at(T0, JsString::OFFSET_OF_FIBER));
        m.load64(T1, at(T1, JsString::OFFSET_OF_FIBER));
        m.and32(T2, T2, T3);
        let by_content = m.new_label();
        m.branch_test32(
            TestCond::Zero,
            T2,
            TypeFlags::PER_CELL_BIT.bits(),
            by_content,
        );
        m.compare64(Cond::Eq, T0, T0, T1);
        m.ret();
        m.bind(by_content);
        m.or64(T2, T0, T1);
        m.branch_test32(TestCond::NonZero, T2, JsString::IS_ROPE, unknown);
        m.load32(T2, at(T0, StringImpl::OFFSET_OF_LENGTH));
        m.branch32(Cond::Ne, T2, at(T1, StringImpl::OFFSET_OF_LENGTH), no);
        m.jump(unknown);
    }

    fn compare_numbers(&mut self, otherwise: Label) {
        self.m.number_to_double(F0, T0, T2, otherwise);
        self.m.number_to_double(F1, T1, T2, otherwise);
        self.m.compare_f64(FloatCond::Eq, T0, F0, F1);
        self.m.ret();
    }

    fn strict_equal(&mut self) {
        let [yes, no, unknown, cells, by_bits] = core::array::from_fn(|_| self.m.new_label());
        self.m.or64(T2, T0, T1);
        self.m.branch_if_cell(T2, cells);
        self.compare_numbers(by_bits);
        self.m.bind(by_bits);
        self.m.compare64(Cond::Eq, T0, T0, T1);
        self.m.ret();
        self.m.bind(cells);
        self.m.branch64(Cond::Eq, T0, T1, yes);
        self.m.load8(T2, cell_type(T0));
        (self.m).branch32(Cond::Above, T2, i32::from(CellType::BigInt.0), no);
        self.m.load8(T3, cell_type(T1));
        self.m.branch32(Cond::Ne, T2, T3, no);
        (self.m).branch32(Cond::Ne, T2, i32::from(CellType::String.0), unknown);
        self.compare_strings(no, unknown);
        self.answer(yes, 1);
        self.answer(no, 0);
        self.answer(unknown, UNKNOWN);
    }

    fn loose_equal(&mut self) {
        let [yes, no, unknown, cells, no_numbers, primitive] =
            core::array::from_fn(|_| self.m.new_label());
        self.m.or64(T2, T0, T1);
        self.m.branch_if_cell(T2, cells);
        self.compare_numbers(no_numbers);
        self.m.bind(no_numbers);
        self.m.branch64(Cond::Eq, T0, T1, yes);
        self.m.jump(unknown);
        self.m.bind(cells);
        self.m.branch64(Cond::Eq, T0, T1, yes);
        self.m.branch_if_cell_is_no_object(T0, primitive);
        self.m.branch_if_cell_is_object(T1, no);
        self.m.jump(unknown);
        self.m.bind(primitive);
        self.m.branch_if_not_type(T0, CellType::String, unknown);
        self.m.branch_if_not_type(T1, CellType::String, unknown);
        self.compare_strings(no, unknown);
        self.answer(yes, 1);
        self.answer(no, 0);
        self.answer(unknown, UNKNOWN);
    }

    fn get_length(&mut self) {
        emit_get_length(&mut self.m);
    }

    fn is_pristine_array(&mut self) {
        let m = &mut self.m;
        let no = m.new_label();
        m.branch_if_not_cell(T1, no);
        m.branch_if_not_type(T1, CellType::Array, no);
        m.load_global(T2);
        let protocol = GlobalObject::offset_of_watchpoints(RealmWatchpoint::ArrayIteratorProtocol);
        m.load64(T0, at(T2, protocol));
        m.branch_unless_watched(T0, no);
        m.load_indexing_mode(T0, T1, GlobalObject::ARRAY_SHAPE_INDEX_MASK);
        let shapes = GlobalObject::OFFSET_OF_ORIGINAL_ARRAY_SHAPES as i32;
        m.load64(T0, Address::indexed(T2, T0, Scale::Four, shapes));
        m.load32(T2, shape_id(T1));
        m.decode_shape(T2, T2);
        m.compare64(Cond::Eq, T0, T0, T2);
        m.ret();
        self.answer(no, 0);
    }
}

const _: () = assert!(Value::TRUE.to_bits() == Value::FALSE.to_bits() + 1);
