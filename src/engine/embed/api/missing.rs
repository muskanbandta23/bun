//! What C++ links against and the engine does not have yet.

#![allow(clippy::unimplemented)]

use core::ffi::{c_char, c_void};

use bun_engine_macros::export;
use bun_engine_text::{StringImpl, StringRef};
use bun_engine_vm::{
    ArrayBuffer, Cx, Executable, Gc, JsCell, JsFunction, JsObject, JsPromise, JsResult, LineColumn,
    Value, Vm,
};

use crate::abi::returned_as_pointer;

// WebAssembly

enum WasmModule {}
enum WasmSharedMemory {}
enum WasmStreamingCompiler {}

#[repr(transparent)]
struct WasmModuleRef(&'static WasmModule);
#[repr(transparent)]
struct WasmSharedMemoryRef(&'static WasmSharedMemory);
#[repr(transparent)]
struct WasmStreamingCompilerRef(&'static WasmStreamingCompiler);

returned_as_pointer! {
    WasmModuleRef,
    WasmSharedMemoryRef,
    WasmStreamingCompilerRef,
}

#[export]
fn wasm_module_retain(_: &WasmModule) {}

#[export]
unsafe fn wasm_module_release(_: *const WasmModule) {}

#[export]
fn wasm_module_object_create(_: &Cx, _: &WasmModule) -> Gc<JsObject> {
    unimplemented!()
}

#[export]
fn wasm_module_object_module(_: &JsObject) -> WasmModuleRef {
    unimplemented!()
}

#[export]
fn wasm_shared_memory_retain(_: &WasmSharedMemory) {}

#[export]
unsafe fn wasm_shared_memory_release(_: *const WasmSharedMemory) {}

#[export]
fn wasm_shared_memory_byte_length(_: &WasmSharedMemory) -> usize {
    unimplemented!()
}

#[export]
fn wasm_is_memory_object(_: &JsCell) -> bool {
    false
}

#[export]
fn wasm_memory_object_create(_: &Cx, _: &WasmSharedMemory) -> Gc<JsObject> {
    unimplemented!()
}

#[export]
fn wasm_memory_object_of_buffer(_: &Vm, _: &ArrayBuffer) -> Option<Gc<JsObject>> {
    None
}

#[export]
fn wasm_memory_object_base(_: &JsObject) -> *mut c_void {
    unimplemented!()
}

#[export]
fn wasm_memory_object_byte_length(_: &JsObject) -> usize {
    unimplemented!()
}

#[export]
fn wasm_memory_object_share(_: &JsObject) -> Option<WasmSharedMemoryRef> {
    unimplemented!()
}

#[export]
fn wasm_streaming_compiler_create(
    _: &Cx,
    _: Gc<JsPromise>,
    _instantiates: bool,
    _imports: Option<Gc<JsObject>>,
    _options: Value,
) -> WasmStreamingCompilerRef {
    unimplemented!()
}

#[export]
fn wasm_streaming_compiler_retain(_: &WasmStreamingCompiler) {}

#[export]
unsafe fn wasm_streaming_compiler_release(_: *const WasmStreamingCompiler) {}

#[export]
fn wasm_streaming_compiler_add_bytes(_: &WasmStreamingCompiler, _: &[u8]) {}

#[export]
fn wasm_streaming_compiler_finish(_: &WasmStreamingCompiler, _: &mut Cx) {}

#[export]
fn wasm_streaming_compiler_fail(_: &WasmStreamingCompiler, _: &mut Cx, _error: Value) {}

#[export]
fn wasm_streaming_compiler_cancel(_: &WasmStreamingCompiler) {}

#[export]
fn wasm_compile_error_create(_: &Cx, _message: &StringImpl) -> Gc<JsObject> {
    unimplemented!()
}

// FFI

#[allow(dead_code)]
#[repr(u8)]
enum FfiType {
    Char,
    Int8,
    Uint8,
    Int16,
    Uint16,
    Int32,
    Uint32,
    Int64,
    Uint64,
    Double,
    Float,
    Bool,
    Pointer,
    Void,
    CString,
    Int64Fast,
    Uint64Fast,
    Function,
    Value = 19,
    Buffer,
    BufferLength,
}

enum FfiInvocation {}

#[export]
fn ffi_type_name(_: u8) -> *const c_char {
    c"invalid".as_ptr()
}

#[export]
fn ffi_is_signature_supported(_arguments: &[u8], _result: u8) -> bool {
    false
}

#[export]
fn ffi_value_to_slot(_: &mut Cx, _type: u8, _: Value) -> JsResult<u64> {
    unimplemented!()
}

#[export]
unsafe fn ffi_function_create(
    _: &Cx,
    _arguments: &[u8],
    _result: u8,
    _target: *mut c_void,
    _name: &StringImpl,
    _owner: Option<Gc<JsObject>>,
) -> JsResult<Gc<JsFunction>> {
    unimplemented!()
}

#[export]
unsafe fn ffi_callback_create(
    _: &Cx,
    _arguments: &[u8],
    _result: u8,
    _callable: Gc<JsObject>,
    _dispatch: Option<unsafe extern "C" fn(*mut FfiInvocation, *mut c_void)>,
    _context: *mut c_void,
) -> JsResult<Gc<JsObject>> {
    unimplemented!()
}

#[export]
fn ffi_is_callback(_: &JsCell) -> bool {
    false
}

#[export]
fn ffi_callback_close(_: &JsObject) {}

#[export]
fn ffi_invocation_run(_: &mut FfiInvocation) {}

#[export]
unsafe fn ffi_invocation_destroy(_: *mut FfiInvocation) {}

// Profilers

#[export]
fn sampling_profiler_exists(_: &Vm) -> bool {
    false
}

#[export]
fn sampling_profiler_create(_: &Vm) {}

#[export]
fn sampling_profiler_set_interval(_: &Vm, _microseconds: f64) {}

#[export]
fn sampling_profiler_start(_: &Vm) {}

#[export]
fn sampling_profiler_pause(_: &Vm) {}

#[export]
fn sampling_profiler_clear(_: &Vm) {}

#[export]
unsafe fn sampling_profiler_release_traces(
    _: &Vm,
    _context: *mut c_void,
    _add_trace: unsafe extern "C" fn(*mut c_void, f64, usize),
    _add_frame: unsafe extern "C" fn(
        *mut c_void,
        &StringImpl,
        Option<Gc<Executable>>,
        i32,
        u32,
        Option<&LineColumn>,
    ),
) {
}

#[export]
fn sampling_profiler_traces_json(_: &Vm) -> StringRef {
    unimplemented!()
}

#[export]
fn sampling_profiler_top_functions(_: &Vm) -> StringRef {
    unimplemented!()
}

#[export]
fn sampling_profiler_top_bytecodes(_: &Vm) -> StringRef {
    unimplemented!()
}
