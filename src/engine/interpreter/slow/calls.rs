//! A call is prepared here and made by the tier.

use core::mem::MaybeUninit;

use bun_engine_bytecode::{Op, Operand, Register, Specialization, op};
use bun_engine_macros::native_abi;
use bun_engine_vm::call::{CallTarget, MAX_ARGUMENTS};
use bun_engine_vm::code::metadata;
use bun_engine_vm::{CallFrame, Cx, JsResult, Value, Vm, errors, options};

use super::{Frame, decode};

/// An instruction that a callee returns to.
pub(crate) trait Returns: Op {
    fn dst(self) -> Register;
    /// Starts with a `metadata::Call`.
    fn metadata(self) -> u32;
}

pub(crate) trait AnyCall: Returns {
    const SPECIALIZATION: Specialization;
    const IS_TAIL: bool;

    fn callee(self) -> Operand;
    /// Or `new.target`.
    fn this(self) -> Operand;
}

pub(crate) trait FixedCall: AnyCall {
    fn frame(self) -> u32;
    /// With `this`.
    fn count(self) -> u32;
    /// Those that the instruction stores.
    fn arguments(self) -> [Option<Operand>; 2];
}

pub(crate) trait VarargsCall: AnyCall {
    fn argument_count(self, f: Frame<'_>) -> usize;
    fn load_arguments(self, cx: &mut Cx, f: Frame<'_>, target: &mut [MaybeUninit<Value>]);
}

macro_rules! calls {
    ($($Op:ident, $specialization:ident, $is_tail:literal, $this:ident;)*) => {$(
        impl Returns for op::$Op {
            #[inline(always)]
            fn dst(self) -> Register {
                self.dst
            }

            #[inline(always)]
            fn metadata(self) -> u32 {
                self.metadata
            }
        }

        impl AnyCall for op::$Op {
            const SPECIALIZATION: Specialization = Specialization::$specialization;
            const IS_TAIL: bool = $is_tail;

            #[inline(always)]
            fn callee(self) -> Operand {
                self.callee.into()
            }

            #[inline(always)]
            fn this(self) -> Operand {
                self.$this.into()
            }
        }
    )*};
}

calls! {
    Call, Call, false, this;
    Call1, Call, false, this;
    Call2, Call, false, this;
    TailCall, Call, true, this;
    Construct, Construct, false, new_target;
    SuperConstruct, Construct, false, new_target;
    CallVarargs, Call, false, this;
    TailCallVarargs, Call, true, this;
    ConstructVarargs, Construct, false, new_target;
    SuperConstructVarargs, Construct, false, new_target;
    CallWithSpread, Call, false, this;
}

macro_rules! fixed_calls {
    ($($Op:ident, |$op:ident| $count:expr, $arguments:expr;)*) => {$(
        impl FixedCall for op::$Op {
            #[inline(always)]
            fn frame(self) -> u32 {
                self.frame
            }

            #[inline(always)]
            #[allow(unused_variables)]
            fn count(self) -> u32 {
                let $op = self;
                $count
            }

            #[inline(always)]
            #[allow(unused_variables)]
            fn arguments(self) -> [Option<Operand>; 2] {
                let $op = self;
                $arguments
            }
        }
    )*};
}

fixed_calls! {
    Call, |op| op.count, [None, None];
    Call1, |op| 2, [Some(op.first), None];
    Call2, |op| 3, [Some(op.first), Some(op.second)];
    TailCall, |op| op.count, [None, None];
    Construct, |op| op.count, [None, None];
    SuperConstruct, |op| op.count, [None, None];
}

macro_rules! list_calls {
    ($($Op:ident, |$op:ident| $skip:expr;)*) => {$(
        impl VarargsCall for op::$Op {
            #[inline(always)]
            #[allow(unused_variables)]
            fn argument_count(self, f: Frame<'_>) -> usize {
                let $op = self;
                match f.get_or_empty(self.arguments) {
                    Some(list) => bun_engine_vm::argument_list_length(list, $skip),
                    None => f.header.rest($skip as usize).len(),
                }
            }

            #[inline(always)]
            #[allow(unused_variables)]
            fn load_arguments(self, cx: &mut Cx, f: Frame<'_>, target: &mut [MaybeUninit<Value>]) {
                let $op = self;
                match f.get_or_empty(self.arguments) {
                    Some(list) => bun_engine_vm::load_argument_list(&cx.no_gc(), list, $skip, target),
                    None => {
                        target.write_copy_of_slice(f.header.rest($skip as usize));
                    }
                }
            }
        }
    )*};
}

list_calls! {
    CallVarargs, |op| op.skip;
    TailCallVarargs, |op| op.skip;
    ConstructVarargs, |op| 0;
    SuperConstructVarargs, |op| 0;
}

impl VarargsCall for op::CallWithSpread {
    fn argument_count(self, f: Frame<'_>) -> usize {
        bun_engine_vm::spread_length(f.run(self.first, self.count))
    }

    fn load_arguments(self, _: &mut Cx, f: Frame<'_>, target: &mut [MaybeUninit<Value>]) {
        bun_engine_vm::load_spread_arguments(f.run(self.first, self.count), target);
    }
}

/// What to call, and what to pass in the register of the executable.
#[repr(C)]
pub struct PreparedCall {
    /// Zero if it threw.
    pub entry: usize,
    pub executable: usize,
}

impl PreparedCall {
    const THREW: PreparedCall = PreparedCall {
        entry: 0,
        executable: 0,
    };
}

pub(crate) const fn frame_words(count_including_this: usize) -> usize {
    (CallFrame::HEADER_WORDS + count_including_this).next_multiple_of(2)
}

/// # Safety
/// `new_frame` has room for a header.
#[inline(always)]
pub(crate) unsafe fn write_header(
    new_frame: *mut u64,
    code_block: usize,
    callee: u64,
    count: u64,
    this: u64,
) {
    let header = [code_block as u64, callee, count, this];
    // SAFETY: the caller's.
    unsafe {
        new_frame
            .byte_add(CallFrame::OFFSET_OF_CODE_BLOCK)
            .cast::<[u64; 4]>()
            .write(header);
    }
}

#[inline(always)]
fn find_target<O: AnyCall>(op: O, cx: &mut Cx, f: Frame<'_>) -> JsResult<CallTarget> {
    let (callee, profile) = (
        f.get(op.callee()),
        &f.entry::<metadata::Call>(op.metadata()).profile,
    );
    let Some(target) = CallTarget::find(callee, O::SPECIALIZATION) else {
        return Err(match O::SPECIALIZATION {
            Specialization::Call => errors::throw_not_a_function(cx, callee),
            Specialization::Construct => errors::throw_not_a_constructor(cx, callee),
        });
    };
    let address = callee.to_bits() as usize;
    if profile.callee().bits() != address
        && profile.executable().bits() != target.executable
        && !profile.try_count(address, target.executable)
    {
        profile.record(f.code_block, callee);
    }
    if O::OPCODE.metadata() == Some(bun_engine_bytecode::MetadataKind::SuperConstruct)
        && let Some(new_target) = f.get(op.this()).as_cell()
    {
        let entry = f.entry::<metadata::SuperConstruct>(op.metadata());
        entry.new_target.record(f.code_block, &new_target);
    }
    Ok(target)
}

/// Fills the frame that the instruction names.
#[native_abi]
pub type PrepareFixedCall = unsafe fn(vm: &Vm, frame: &CallFrame, start: *const u8) -> PreparedCall;

#[native_abi]
#[inline(never)]
pub(crate) unsafe fn prepare_fixed_call<O: FixedCall>(
    vm: &Vm,
    frame: &CallFrame,
    start: *const u8,
) -> PreparedCall {
    // SAFETY: the caller's, and the new frame is among the locals.
    unsafe {
        let (op, (cx, f)) = (decode::<O>(start), Frame::enter(vm, frame, start));
        let Ok(target) = find_target(op, cx, f) else {
            return PreparedCall::THREW;
        };
        let (callee, this) = (f.get(op.callee()).to_bits(), f.get(op.this()).to_bits());
        let arguments = op.arguments().map(|it| it.map(|it| f.get(it)));
        for (index, argument) in (0..).zip(arguments.into_iter().flatten()) {
            f.set(Register::outgoing_argument(op.frame(), index), argument);
        }
        let new_frame = frame.slot(-(op.frame() as isize)).as_ptr();
        write_header(
            new_frame,
            target.code_block,
            callee,
            op.count().into(),
            this,
        );
        PreparedCall {
            entry: target.entry,
            executable: target.executable,
        }
    }
}

/// The address of the new frame, which is below the locals, or zero if it threw. The caller moves the stack pointer
/// below it and goes on with `FillVarargsFrame`.
#[native_abi]
pub type SizeVarargsFrame = unsafe fn(vm: &Vm, frame: &CallFrame, start: *const u8) -> usize;

#[native_abi]
#[inline(never)]
pub(crate) unsafe fn size_varargs_frame<O: VarargsCall>(
    vm: &Vm,
    frame: &CallFrame,
    start: *const u8,
) -> usize {
    // SAFETY: the caller's.
    let (op, (cx, f)) = unsafe { (decode::<O>(start), Frame::enter(vm, frame, start)) };
    let count = op.argument_count(f);
    let words = f.code_block.locals() as usize + frame_words(count.min(MAX_ARGUMENTS) + 1);
    let new_frame = frame.address() - words * size_of::<u64>();
    if count > MAX_ARGUMENTS || new_frame < vm.soft_stack_limit() {
        let _ = cx.throw_stack_overflow();
        return 0;
    }
    new_frame
}

#[native_abi]
pub type FillVarargsFrame =
    unsafe fn(vm: &Vm, frame: &CallFrame, start: *const u8, new_frame: *mut u64) -> PreparedCall;

#[native_abi]
#[inline(never)]
pub(crate) unsafe fn fill_varargs_frame<O: VarargsCall>(
    vm: &Vm,
    frame: &CallFrame,
    start: *const u8,
    new_frame: *mut u64,
) -> PreparedCall {
    // SAFETY: the caller's. Nothing has run since the frame was sized.
    unsafe {
        let (op, (cx, f)) = (decode::<O>(start), Frame::enter(vm, frame, start));
        let Ok(target) = find_target(op, cx, f) else {
            return PreparedCall::THREW;
        };
        let count = op.argument_count(f);
        let arguments = new_frame.byte_add(CallFrame::OFFSET_OF_ARGUMENTS).cast();
        op.load_arguments(cx, f, core::slice::from_raw_parts_mut(arguments, count));
        let (callee, this) = (f.get(op.callee()).to_bits(), f.get(op.this()).to_bits());
        write_header(new_frame, target.code_block, callee, count as u64 + 1, this);
        PreparedCall {
            entry: target.entry,
            executable: target.executable,
        }
    }
}

pub(crate) fn makes_tail_calls() -> bool {
    options().use_tail_calls
}

/// Puts `new_frame` in the place of `frame`, and returns where it starts now.
///
/// # Safety
/// `new_frame` is complete but for the linkage, and nothing of `frame` is needed any more.
#[native_abi]
pub unsafe fn replace_frame(frame: &CallFrame, new_frame: *const u64) -> usize {
    // SAFETY: the caller's. The callee owns all words of its frame.
    unsafe {
        let parameters = frame.code_block().map_or(0, |it| it.parameters());
        let old_count = frame.argument_count_including_this().max(parameters) as usize;
        let new_count = (*new_frame.cast::<CallFrame>()).argument_count_including_this() as usize;
        let linkage = [frame.caller_address(), frame.return_address()];
        let moved = frame_words(old_count) as isize - frame_words(new_count) as isize;
        let target = frame.slot(moved).as_ptr();
        core::ptr::copy(new_frame, target, CallFrame::HEADER_WORDS + new_count);
        target.cast::<[usize; 2]>().write(linkage);
        target.addr()
    }
}
