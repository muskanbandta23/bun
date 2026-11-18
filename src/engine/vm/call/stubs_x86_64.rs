//! At an entry `rcx` has the executable.

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
            "mov rcx, [rbp - 8]\n",
            "mov [r13 + {top_call_frame}], rcx\n",
            "mov rcx, [rbp - 16]\n",
            "mov [r13 + {top_entry_frame}], rcx\n",
            "mov rbx, [rbp - 24]\n",
            "mov r12, [rbp - 32]\n",
            "mov r13, [rbp - 40]\n",
            "mov r14, [rbp - 48]\n",
            "mov r15, [rbp - 56]\n",
            "leave\n",
            "ret",
        )
    };
}

#[unsafe(naked)]
pub(super) unsafe extern "sysv64" fn enter(
    entry: usize,
    vm: &Vm,
    frame: &ProtoFrame,
) -> JsResult<Value> {
    naked_asm!(
        "push rbp",
        "mov rbp, rsp",
        "push qword ptr [rsi + {top_call_frame}]",
        "push qword ptr [rsi + {top_entry_frame}]",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "mov r13, rsi",
        "mov [r13 + {top_entry_frame}], rbp",
        "mov ecx, [rdx + {copied}]",
        "lea rax, [rcx * 8 + 32 + 15]",
        "and rax, -16",
        "lea rsp, [rbp - 64]",
        "sub rsp, rax",
        "mov rsi, [rdx + {arguments}]",
        "test ecx, ecx",
        "jz 3f",
        "2:",
        "mov rax, [rsi + rcx * 8 - 8]",
        "mov [rsp + rcx * 8 + 24], rax",
        "dec ecx",
        "jnz 2b",
        "3:",
        "mov rax, [rdx + {this_value}]",
        "mov [rsp + 24], rax",
        "mov eax, [rdx + {argument_count}]",
        "mov [rsp + 16], rax",
        "mov rax, [rdx + {callee}]",
        "mov [rsp + 8], rax",
        "mov rax, [rdx + {code_block}]",
        "mov [rsp], rax",
        "mov rcx, [rdx + {executable}]",
        "movabs r14, {number_tag}",
        "lea r15, [r14 + {other_tag}]",
        "call rdi",
        leave_entry_frame!(),
        top_call_frame = const Vm::OFFSET_OF_TOP_CALL_FRAME,
        top_entry_frame = const Vm::OFFSET_OF_TOP_ENTRY_FRAME,
        code_block = const offset_of!(ProtoFrame, code_block),
        callee = const offset_of!(ProtoFrame, callee),
        argument_count = const offset_of!(ProtoFrame, argument_count_including_this),
        copied = const offset_of!(ProtoFrame, copied),
        this_value = const offset_of!(ProtoFrame, this_value),
        arguments = const offset_of!(ProtoFrame, arguments),
        executable = const offset_of!(ProtoFrame, executable),
        number_tag = const Value::NUMBER_TAG,
        other_tag = const Value::OTHER_TAG,
    )
}

/// As `enter`, in a `Vm` that has been entered. What an exception restores on its way out means nothing to the caller.
#[unsafe(naked)]
pub(super) unsafe extern "rust-preserve-none" fn enter_with_registers(
    entry: usize,
    vm: &Vm,
    number_tag: u64,
    not_cell_mask: u64,
    code_block: usize,
    callee: Value,
    this_value: Value,
    executable: usize,
    argument_count_including_this: u64,
    first: Value,
    second: Value,
    third: Value,
) -> JsResult<Value> {
    naked_asm!(
        "push rbp",
        "mov rbp, rsp",
        "push qword ptr [r13 + {top_call_frame}]",
        "push qword ptr [r13 + {top_entry_frame}]",
        "mov [r13 + {top_entry_frame}], rbp",
        "push rax",
        "push rax",
        "push r11",
        "push r9",
        "push rdx",
        "push r8",
        "push rsi",
        "push rdi",
        "call r12",
        "mov rcx, [rbp - 8]",
        "mov [r13 + {top_call_frame}], rcx",
        "mov rcx, [rbp - 16]",
        "mov [r13 + {top_entry_frame}], rcx",
        "leave",
        "ret",
        top_call_frame = const Vm::OFFSET_OF_TOP_CALL_FRAME,
        top_entry_frame = const Vm::OFFSET_OF_TOP_ENTRY_FRAME,
    )
}

#[unsafe(naked)]
pub(super) unsafe extern "sysv64" fn return_to_native_code_with_exception() {
    naked_asm!(
        "xor eax, eax",
        leave_entry_frame!(),
        top_call_frame = const Vm::OFFSET_OF_TOP_CALL_FRAME,
        top_entry_frame = const Vm::OFFSET_OF_TOP_ENTRY_FRAME,
    )
}

#[unsafe(naked)]
pub unsafe extern "sysv64" fn throw_from_frame() {
    naked_asm!(
        "and rsp, -16",
        "mov rdi, r13",
        "mov rsi, rbp",
        "call {unwind}",
        "mov rbp, [r13 + {frame}]",
        "jmp qword ptr [r13 + {address}]",
        unwind = sym super::unwind,
        frame = const Vm::OFFSET_OF_CATCH_FRAME,
        address = const Vm::OFFSET_OF_CATCH_ADDRESS,
    )
}

/// `$load` puts the realm in `rdi` and the function in `rax`, given the callee in `rdx`.
macro_rules! native_entry {
    ($name:ident, [$($load:literal),*], $($operand:ident = $value:expr),*) => {
        #[unsafe(naked)]
        pub(crate) unsafe extern "sysv64" fn $name() {
            naked_asm!(
                "push rbp",
                "mov rbp, rsp",
                "mov [r13 + {top_call_frame}], rbp",
                "mov rdx, [rbp + {callee}]",
                $($load,)*
                "mov rsi, rbp",
                "call rax",
                "cmp qword ptr [r13 + {exception}], 0",
                "jne {throw}",
                "mov ecx, {undefined}",
                "test rax, rax",
                "cmovz rax, rcx",
                "pop rbp",
                "ret",
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
    ["mov rdi, [rdx + {scope}]", "mov rax, [rcx + {function}]"],
    scope = JsFunction::OFFSET_OF_SCOPE,
    function = NativeExecutable::OFFSET_OF_FUNCTION
);
native_entry!(
    native_construct_entry,
    ["mov rdi, [rdx + {scope}]", "mov rax, [rcx + {function}]"],
    scope = JsFunction::OFFSET_OF_SCOPE,
    function = NativeExecutable::OFFSET_OF_CONSTRUCTOR
);
native_entry!(
    internal_function_call_entry,
    ["mov rdi, [rdx + {scope}]", "mov rax, [rdx + {function}]"],
    scope = JsFunction::OFFSET_OF_SCOPE,
    function = InternalFunction::OFFSET_OF_FUNCTION
);
native_entry!(
    internal_function_construct_entry,
    ["mov rdi, [rdx + {scope}]", "mov rax, [rdx + {function}]"],
    scope = JsFunction::OFFSET_OF_SCOPE,
    function = InternalFunction::OFFSET_OF_CONSTRUCTOR
);
native_entry!(
    call_data_entry,
    [
        "mov edi, [rdx]",
        "add rdi, [r13 + {shape_base}]",
        "mov rdi, [rdi + {global}]",
        "mov rax, rcx"
    ],
    shape_base = Vm::OFFSET_OF_SHAPE_BASE,
    global = Shape::OFFSET_OF_GLOBAL
);

macro_rules! link_entry {
    ($name:ident, $specialization:expr) => {
        #[unsafe(naked)]
        pub(super) unsafe extern "sysv64" fn $name() {
            naked_asm!(
                "push rbp",
                "mov rbp, rsp",
                "mov [r13 + {top_call_frame}], rbp",
                "mov rdi, r13",
                "mov rsi, rbp",
                "mov edx, {specialization}",
                "call {link}",
                "test rax, rax",
                "jz {throw}",
                "pop rbp",
                "jmp rax",
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

macro_rules! watched_entry {
    ($name:ident, $specialization:expr) => {
        #[unsafe(naked)]
        pub(super) unsafe extern "sysv64" fn $name() {
            naked_asm!(
                "push rbp",
                "mov rbp, rsp",
                "mov [r13 + {top_call_frame}], rbp",
                "push rcx",
                "push rcx",
                "mov rdi, r13",
                "mov rsi, rbp",
                "mov edx, {specialization}",
                "call {notify}",
                "pop rcx",
                "leave",
                "jmp rax",
                top_call_frame = const Vm::OFFSET_OF_TOP_CALL_FRAME,
                specialization = const $specialization as u32,
                notify = sym crate::debugger::will_call_native_function,
            )
        }
    };
}

watched_entry!(watched_call_entry, Call);
watched_entry!(watched_construct_entry, Construct);

macro_rules! rare_data_entry {
    ($name:ident, $specialization:expr) => {
        #[unsafe(naked)]
        pub(super) unsafe extern "sysv64" fn $name() {
            naked_asm!(
                "mov rcx, [rcx + {executable}]",
                "mov rax, [rcx + {code_block}]",
                "mov [rsp + 8], rax",
                "jmp qword ptr [rcx + {entry}]",
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
pub unsafe extern "sysv64" fn pad_arguments() {
    naked_asm!(
        "pop r8",
        "mov ecx, [rbp + {argument_count}]",
        "add ecx, {header}",
        "add edi, {header}",
        // The frame takes an even number of words, before and after.
        "lea eax, [rdi + 1]",
        "lea edx, [rcx + 1]",
        "and eax, -2",
        "and edx, -2",
        "sub eax, edx",
        "jz 3f",
        "shl rax, 3",
        "mov rsi, rbp",
        "sub rbp, rax",
        "mov rsp, rbp",
        "xor edx, edx",
        "2:",
        "mov rax, [rsi + rdx * 8]",
        "mov [rbp + rdx * 8], rax",
        "inc edx",
        "cmp edx, ecx",
        "jb 2b",
        "3:",
        "mov qword ptr [rbp + rcx * 8], {undefined}",
        "inc ecx",
        "cmp ecx, edi",
        "jb 3b",
        "push r8",
        "ret",
        argument_count = const CallFrame::OFFSET_OF_ARGUMENT_COUNT,
        header = const CallFrame::HEADER_WORDS,
        undefined = const Value::UNDEFINED.to_bits(),
    )
}
