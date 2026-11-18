//! What compiled code calls where an instruction cannot do the work itself. Each is a thin wrapper that the code
//! reaches by address, so every one follows the C convention.
//!
//! A function that gives a value returns its bits, and zero means that an exception is pending. One that gives a
//! boolean or nothing returns 0 or 1, and 2 means that an exception is pending.

use bun_engine_macros::native_abi;
use bun_engine_vm::heap::{Allocator, barrier};
use bun_engine_vm::{
    CallFrame, CodeBlock, Cx, ElementKind, Gc, JsCell, JsResult, JsString, Value, Vm,
};

use crate::graph::Relation;

macro_rules! addresses {
    ($($name:ident,)*) => {
        /// What generated code calls, by address.
        pub(crate) mod address {
            $(pub(crate) fn $name() -> usize {
                super::$name as *const () as usize
            })*
        }
    };
}

addresses! {
    to_int32,
    allocate_slow,
    write_barrier_slow,
    handle_traps,
    throw_stack_overflow,
    double_remainder,
    resolve_rope,
    string_from_unit,
    concatenate,
    string_equal,
    generic_to_boolean,
    quiet_loose_equal,
    primitive_compare,
    generic_strict_equal,
    store_element_growing,
}

/// Three answers where the language has two.
pub(crate) mod answer {
    pub(crate) const THREW: u32 = 2;
}

fn answer(result: JsResult<bool>) -> u32 {
    result.map_or(answer::THREW, u32::from)
}

fn bits(result: JsResult<Value>) -> u64 {
    result.map_or(0, Value::to_bits)
}

/// # Safety
/// The bits of a value that is alive.
unsafe fn value(bits: u64) -> Value {
    // SAFETY: the caller's.
    unsafe { Value::from_bits(bits) }
}

/// # Safety
/// The address of a string that is alive.
unsafe fn string(bits: u64) -> Gc<JsString> {
    // SAFETY: the caller's. The operand has passed a check that it is a string.
    unsafe { Gc::from_address(bits as usize) }
}

/// The realm of the code that the frame is running. Compiled code has made the frame known to the `Vm` already.
///
/// # Safety
/// The header of a frame whose code block is that of running code.
unsafe fn realm_of<'a>(frame: &CallFrame) -> &'a mut Cx {
    // SAFETY: the caller's. The frame keeps its code block alive, and generated code runs inside a call that
    // holds the `Cx`.
    unsafe {
        let code_block = Gc::<CodeBlock>::from_address(frame.code_block_bits());
        Cx::from_global(code_block.global())
    }
}

/// Does what a trap that the run time has raised asks for. Non-zero if an exception is pending afterwards.
#[native_abi]
pub(crate) unsafe fn handle_traps(frame: &CallFrame) -> u32 {
    // SAFETY: the caller's.
    let cx = unsafe { realm_of(frame) };
    u32::from(cx.handle_traps().is_err())
}

/// ToInt32 of a double that rounding toward zero cannot give an i64 for.
#[native_abi]
pub(crate) fn to_int32(value: f64) -> i32 {
    bun_engine_number::to_int32(value)
}

/// A cell of the allocator's size, once bumping its own interval has used it up.
#[native_abi]
pub(crate) fn allocate_slow(vm: &Vm, allocator: &Allocator) -> usize {
    let cell = allocator.allocate_slow(&vm.heap);
    if cell == 0 {
        bun_alloc::out_of_memory();
    }
    cell
}

/// The stack has no room for the frame that a call has just built. The frame has a header and nothing else.
#[native_abi]
pub(crate) unsafe fn throw_stack_overflow(vm: &Vm, frame: &CallFrame) {
    vm.set_top_call_frame(frame);
    frame.set_call_site(0);
    // SAFETY: the caller's. The code block is still the one of the callee.
    unsafe {
        let cx = realm_of(frame);
        frame.set_code_block(None);
        let _ = cx.throw_stack_overflow();
    }
}

/// Follows a store into a cell whose state is at most the barrier threshold.
#[native_abi]
pub(crate) fn write_barrier_slow(cell: &JsCell) {
    barrier::slow_path(cell);
}

/// The remainder of two doubles, which no instruction of either architecture gives.
#[native_abi]
pub(crate) fn double_remainder(dividend: f64, divisor: f64) -> f64 {
    dividend % divisor
}

/// Puts the pieces of a string together, so that its characters are in one place.
#[native_abi]
pub(crate) unsafe fn resolve_rope(frame: &CallFrame, rope: u64) -> u64 {
    // SAFETY: the caller's.
    unsafe {
        let (cx, rope) = (realm_of(frame), string(rope));
        match rope.resolve(cx) {
            Ok(_) => rope.as_value().to_bits(),
            Err(_) => 0,
        }
    }
}

/// The string of one code unit, for the units that the shared table does not hold.
#[native_abi]
pub(crate) fn string_from_unit(vm: &Vm, unit: u32) -> u64 {
    JsString::from_unit(vm, unit as u16).as_value().to_bits()
}

/// Two or three strings, one after the other. `third` is zero for two.
#[native_abi]
pub(crate) unsafe fn concatenate(frame: &CallFrame, first: u64, second: u64, third: u64) -> u64 {
    // SAFETY: the caller's.
    unsafe {
        let cx = realm_of(frame);
        let (first, second) = (string(first), string(second));
        let made = match third {
            0 => JsString::concat(cx, first, second),
            _ => JsString::concat3(cx, first, second, string(third)),
        };
        bits(made.map(Value::from))
    }
}

/// Whether two strings hold the same characters, for the cases that no inline test settles.
#[native_abi]
pub(crate) unsafe fn string_equal(frame: &CallFrame, left: u64, right: u64) -> u32 {
    // SAFETY: the caller's.
    unsafe {
        let (cx, left, right) = (realm_of(frame), string(left), string(right));
        answer(left.equals(cx, &right))
    }
}

/// ToBoolean, which runs no script.
#[native_abi]
pub(crate) unsafe fn generic_to_boolean(frame: &CallFrame, operand: u64) -> u32 {
    // SAFETY: the caller's.
    unsafe {
        let cx = realm_of(frame);
        u32::from(value(operand).to_boolean(cx))
    }
}

/// IsLooselyEqual of two operands that convert without running a script.
#[native_abi]
pub(crate) unsafe fn quiet_loose_equal(frame: &CallFrame, left: u64, right: u64) -> u32 {
    // SAFETY: the caller's.
    unsafe {
        let cx = realm_of(frame);
        answer(value(left).loose_equals(cx, value(right)))
    }
}

/// `<`, `<=`, `>` or `>=` of two primitives.
#[native_abi]
pub(crate) unsafe fn primitive_compare(
    frame: &CallFrame,
    relation: u32,
    left: u64,
    right: u64,
) -> u32 {
    use core::cmp::Ordering::{Equal, Greater, Less};
    // SAFETY: the caller's.
    let (cx, left, right) = unsafe { (realm_of(frame), value(left), value(right)) };
    let order = match left.compare(cx, right) {
        Ok(order) => order,
        Err(_) => return answer::THREW,
    };
    let holds = match Relation::from_data(relation) {
        Relation::Less => order == Some(Less),
        Relation::LessEqual => matches!(order, Some(Less | Equal)),
        Relation::Greater => order == Some(Greater),
        Relation::GreaterEqual => matches!(order, Some(Greater | Equal)),
    };
    u32::from(holds)
}

/// IsStrictlyEqual, for the cases that no inline test settles.
#[native_abi]
pub(crate) unsafe fn generic_strict_equal(frame: &CallFrame, left: u64, right: u64) -> u32 {
    // SAFETY: the caller's.
    unsafe {
        let cx = realm_of(frame);
        answer(value(left).strict_equals(cx, value(right)))
    }
}

/// A store at an index that the elements do not reach, which gives them room.
#[native_abi]
pub(crate) unsafe fn store_element_growing(
    frame: &CallFrame,
    array: u64,
    index: u32,
    stored: u64,
    kind: u32,
) -> u32 {
    // SAFETY: the caller's.
    unsafe {
        let cx = realm_of(frame);
        let stored = if kind == ElementKind::Double as u32 {
            Value::number(f64::from_bits(stored))
        } else {
            value(stored)
        };
        // The elements have passed a check that they are writable, so only an exception can stop the store.
        answer(value(array).put_index(cx, index, stored, false))
    }
}
