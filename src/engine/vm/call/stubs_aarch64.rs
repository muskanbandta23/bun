//! At an entry `x9` has the executable.

use core::arch::naked_asm;
use core::mem::offset_of;

use bun_engine_bytecode::Specialization::{Call, Construct};

use super::ProtoFrame;
use crate::{
    CallFrame, Executable, FunctionRareData, InternalFunction, JsFunction, JsResult,
    NativeExecutable, Shape, Value, Vm,
};

macro_rules! leave_entry_frame {
    () => {
        concat!(
            "ldp x9, x10, [x29, #-16]\n",
            "str x9, [x21, #{top_entry_frame}]\n",
            "str x10, [x21, #{top_call_frame}]\n",
            "ldp x19, x20, [x29, #-32]\n",
            "ldp x21, x22, [x29, #-48]\n",
            "ldp x23, x24, [x29, #-64]\n",
            "ldp x25, x26, [x29, #-80]\n",
            "ldp x27, x28, [x29, #-96]\n",
            "ldp d8, d9, [x29, #-112]\n",
            "ldp d10, d11, [x29, #-128]\n",
            "ldp d12, d13, [x29, #-144]\n",
            "ldp d14, d15, [x29, #-160]\n",
            "mov sp, x29\n",
            "ldp x29, x30, [sp], #16\n",
            "ret",
        )
    };
}

#[unsafe(naked)]
pub(super) unsafe extern "C" fn enter(
    entry: usize,
    vm: &Vm,
    frame: &ProtoFrame,
) -> JsResult<Value> {
    naked_asm!(
        "stp x29, x30, [sp, #-16]!",
        "mov x29, sp",
        "sub sp, sp, #160",
        "ldr x9, [x1, #{top_entry_frame}]",
        "ldr x10, [x1, #{top_call_frame}]",
        "stp x9, x10, [x29, #-16]",
        "stp x19, x20, [x29, #-32]",
        "stp x21, x22, [x29, #-48]",
        "stp x23, x24, [x29, #-64]",
        "stp x25, x26, [x29, #-80]",
        "stp x27, x28, [x29, #-96]",
        "stp d8, d9, [x29, #-112]",
        "stp d10, d11, [x29, #-128]",
        "stp d12, d13, [x29, #-144]",
        "stp d14, d15, [x29, #-160]",
        "mov x21, x1",
        "str x29, [x21, #{top_entry_frame}]",
        "ldr w10, [x2, #{copied}]",
        "add x11, x10, #5",
        "and x11, x11, #-2",
        "sub sp, sp, x11, lsl #3",
        "ldr x11, [x2, #{arguments}]",
        "add x12, sp, #32",
        "cbz w10, 3f",
        "2:",
        "sub w10, w10, #1",
        "ldr x13, [x11, x10, lsl #3]",
        "str x13, [x12, x10, lsl #3]",
        "cbnz w10, 2b",
        "3:",
        "ldr w10, [x2, #{argument_count}]",
        "ldr x11, [x2, #{this_value}]",
        "stp x10, x11, [sp, #16]",
        "ldp x10, x11, [x2, #{code_block}]",
        "stp x10, x11, [sp]",
        "ldr x9, [x2, #{executable}]",
        "mov x22, #{number_tag}",
        "add x23, x22, #{other_tag}",
        "blr x0",
        leave_entry_frame!(),
        top_call_frame = const Vm::OFFSET_OF_TOP_CALL_FRAME,
        top_entry_frame = const Vm::OFFSET_OF_TOP_ENTRY_FRAME,
        code_block = const offset_of!(ProtoFrame, code_block),
        argument_count = const offset_of!(ProtoFrame, argument_count_including_this),
        copied = const offset_of!(ProtoFrame, copied),
        this_value = const offset_of!(ProtoFrame, this_value),
        arguments = const offset_of!(ProtoFrame, arguments),
        executable = const offset_of!(ProtoFrame, executable),
        number_tag = const Value::NUMBER_TAG,
        other_tag = const Value::OTHER_TAG,
    )
}

#[unsafe(naked)]
pub(super) unsafe extern "C" fn return_to_native_code_with_exception() {
    naked_asm!(
        "mov x0, #0",
        leave_entry_frame!(),
        top_call_frame = const Vm::OFFSET_OF_TOP_CALL_FRAME,
        top_entry_frame = const Vm::OFFSET_OF_TOP_ENTRY_FRAME,
    )
}

#[unsafe(naked)]
pub unsafe extern "C" fn throw_from_frame() {
    naked_asm!(
        "mov x0, x21",
        "mov x1, x29",
        "bl {unwind}",
        "ldr x29, [x21, #{frame}]",
        "ldr x16, [x21, #{address}]",
        "br x16",
        unwind = sym super::unwind,
        frame = const Vm::OFFSET_OF_CATCH_FRAME,
        address = const Vm::OFFSET_OF_CATCH_ADDRESS,
    )
}

/// `$load` puts the realm in `x0` and the function in `x16`, given the callee in `x2`.
macro_rules! native_entry {
    ($name:ident, [$($load:literal),*], $($operand:ident = $value:expr),*) => {
        #[unsafe(naked)]
        pub(crate) unsafe extern "C" fn $name() {
            naked_asm!(
                "stp x29, x30, [sp, #-16]!",
                "mov x29, sp",
                "str x29, [x21, #{top_call_frame}]",
                "ldr x2, [x29, #{callee}]",
                $($load,)*
                "mov x1, x29",
                "blr x16",
                "ldr x9, [x21, #{exception}]",
                "cbnz x9, 2f",
                "mov x9, #{undefined}",
                "cmp x0, #0",
                "csel x0, x9, x0, eq",
                "ldp x29, x30, [sp], #16",
                "ret",
                "2:",
                "b {throw}",
                top_call_frame = const Vm::OFFSET_OF_TOP_CALL_FRAME,
                callee = const CallFrame::OFFSET_OF_CALLEE,
                exception = const Vm::OFFSET_OF_EXCEPTION,
                undefined = const Value::UNDEFINED.to_bits(),
                throw = sym throw_from_frame,
                $($operand = const $value,)*
            )
        }
    };
}

native_entry!(
    native_call_entry,
    ["ldr x0, [x2, #{scope}]", "ldr x16, [x9, #{function}]"],
    scope = JsFunction::OFFSET_OF_SCOPE,
    function = NativeExecutable::OFFSET_OF_FUNCTION
);
native_entry!(
    native_construct_entry,
    ["ldr x0, [x2, #{scope}]", "ldr x16, [x9, #{function}]"],
    scope = JsFunction::OFFSET_OF_SCOPE,
    function = NativeExecutable::OFFSET_OF_CONSTRUCTOR
);
native_entry!(
    internal_function_call_entry,
    ["ldr x0, [x2, #{scope}]", "ldr x16, [x2, #{function}]"],
    scope = JsFunction::OFFSET_OF_SCOPE,
    function = InternalFunction::OFFSET_OF_FUNCTION
);
native_entry!(
    internal_function_construct_entry,
    ["ldr x0, [x2, #{scope}]", "ldr x16, [x2, #{function}]"],
    scope = JsFunction::OFFSET_OF_SCOPE,
    function = InternalFunction::OFFSET_OF_CONSTRUCTOR
);
native_entry!(
    call_data_entry,
    [
        "ldr w0, [x2]",
        "ldr x10, [x21, #{shape_base}]",
        "add x0, x0, x10",
        "ldr x0, [x0, #{global}]",
        "mov x16, x9"
    ],
    shape_base = Vm::OFFSET_OF_SHAPE_BASE,
    global = Shape::OFFSET_OF_GLOBAL
);

macro_rules! link_entry {
    ($name:ident, $specialization:expr) => {
        #[unsafe(naked)]
        pub(super) unsafe extern "C" fn $name() {
            naked_asm!(
                "stp x29, x30, [sp, #-16]!",
                "mov x29, sp",
                "str x29, [x21, #{top_call_frame}]",
                "mov x0, x21",
                "mov x1, x29",
                "mov w2, #{specialization}",
                "bl {link}",
                "cbz x0, 2f",
                "ldp x29, x30, [sp], #16",
                "br x0",
                "2:",
                "b {throw}",
                top_call_frame = const Vm::OFFSET_OF_TOP_CALL_FRAME,
                specialization = const $specialization as u32,
                link = sym super::link,
                throw = sym throw_from_frame,
            )
        }
    };
}

link_entry!(link_call_entry, Call);
link_entry!(link_construct_entry, Construct);

macro_rules! rare_data_entry {
    ($name:ident, $specialization:expr) => {
        #[unsafe(naked)]
        pub(super) unsafe extern "C" fn $name() {
            naked_asm!(
                "ldr x9, [x9, #{executable}]",
                "ldr x16, [x9, #{code_block}]",
                "str x16, [sp]",
                "ldr x16, [x9, #{entry}]",
                "br x16",
                executable = const FunctionRareData::OFFSET_OF_EXECUTABLE,
                code_block = const Executable::offset_of_code_block($specialization),
                entry = const Executable::offset_of_entry($specialization),
            )
        }
    };
}

rare_data_entry!(rare_data_call_entry, Call);
rare_data_entry!(rare_data_construct_entry, Construct);

#[unsafe(naked)]
pub unsafe extern "C" fn pad_arguments() {
    naked_asm!(
        "ldr w1, [x29, #{argument_count}]",
        "add w1, w1, #{header}",
        "add w0, w0, #{header}",
        // The frame takes an even number of words, before and after.
        "add w2, w0, #1",
        "add w3, w1, #1",
        "and w2, w2, #-2",
        "and w3, w3, #-2",
        "subs w2, w2, w3",
        "b.eq 3f",
        "mov x3, x29",
        "sub x29, x29, x2, lsl #3",
        "mov sp, x29",
        "mov x2, #0",
        "2:",
        "ldr x4, [x3, x2, lsl #3]",
        "str x4, [x29, x2, lsl #3]",
        "add w2, w2, #1",
        "cmp w2, w1",
        "b.lo 2b",
        "3:",
        "mov x4, #{undefined}",
        "4:",
        "str x4, [x29, x1, lsl #3]",
        "add w1, w1, #1",
        "cmp w1, w0",
        "b.lo 4b",
        "ret",
        argument_count = const CallFrame::OFFSET_OF_ARGUMENT_COUNT,
        header = const CallFrame::HEADER_WORDS,
        undefined = const Value::UNDEFINED.to_bits(),
    )
}
