//! Calls between the host and WebAssembly, for functions of any type.

use core::ptr;

use bun_engine_asm::{Bank, Reg};
use bun_engine_macros::native_abi;
use bun_engine_wasm_compiler::abi::{Place, frame};
use bun_engine_wasm_format::{FuncIndex, ValType};

use crate::instance::Instance;
use crate::outcome::Outcome;
use crate::slot;
use crate::value::{Bits, HostCell, Reference};

/// Where stubs put the registers for arguments and results, by their numbers.
pub(crate) struct Registers;

impl Registers {
    const GPRS: i32 = 10;
    const FPRS: i32 = 8;
    pub(crate) const SIZE: i32 = Self::GPRS * 8 + Self::FPRS * 16;

    pub(crate) fn offset(reg: Reg) -> i32 {
        match reg.bank() {
            Bank::Gpr => i32::from(reg.gpr().number()) * 8,
            Bank::Fpr => Self::GPRS * 8 + i32::from(reg.fpr().number()) * 16,
        }
    }
}

/// The arguments or the results of a call. They are on the stack, where the collector finds them.
struct Values<'a> {
    types: &'a [ValType],
    places: &'a [Place],
    registers: *mut u8,
    /// The frame pointer of the callee.
    callee_frame: *mut u8,
}

impl Values<'_> {
    fn address(&self, index: usize) -> *mut u8 {
        match self.places[index] {
            Place::Register(reg) => self.registers.wrapping_add(Registers::offset(reg) as usize),
            Place::Stack(offset) => self.callee_frame.wrapping_add(offset as usize),
        }
    }
}

/// From WebAssembly.
pub struct Incoming<'a>(Values<'a>);

impl<'a> Incoming<'a> {
    /// In terms of the module of the instance.
    pub fn types(&self) -> &'a [ValType] {
        self.0.types
    }

    pub fn get(&self, index: usize) -> Bits {
        // SAFETY: a stub or compiled code put it there.
        unsafe { slot::read(self.0.address(index), self.0.types[index]) }
    }
}

/// To WebAssembly.
pub struct Outgoing<'a>(Values<'a>);

impl<'a> Outgoing<'a> {
    /// In terms of the module of the instance.
    pub fn types(&self) -> &'a [ValType] {
        self.0.types
    }

    pub fn set(&mut self, index: usize, value: Bits) {
        // SAFETY: a stub or the caller made room.
        unsafe { slot::write(self.0.address(index), self.0.types[index], value) };
    }
}

/// A call of an import.
pub struct HostCall<'a> {
    instance: &'a Instance,
    func: FuncIndex,
    arguments: Incoming<'a>,
    results: Outgoing<'a>,
}

impl<'a> HostCall<'a> {
    /// The one that imports.
    pub fn instance(&self) -> &'a Instance {
        self.instance
    }

    pub fn func(&self) -> FuncIndex {
        self.func
    }

    /// What was given to [`Instance::link_host_function`].
    pub fn function(&self) -> (Reference, usize) {
        // SAFETY: it is in the cell of the instance, and does not change any more.
        let import = unsafe { &*self.instance.import(self.func) };
        (Reference(import.host_function), import.host_data)
    }

    pub fn arguments(&self) -> &Incoming<'a> {
        &self.arguments
    }

    /// They may take the places of arguments.
    pub fn into_results(self) -> Outgoing<'a> {
        self.results
    }
}

/// The arguments and the results.
fn values_of(
    instance: &Instance,
    func: FuncIndex,
    registers: *mut u8,
    callee_frame: *mut u8,
) -> (Values<'_>, Values<'_>) {
    let ty = instance.module().func_type(func);
    let signature = instance.module().signature(func);
    let values = |types, places| Values {
        types,
        places,
        registers,
        callee_frame,
    };
    (
        values(ty.params, &signature.params),
        values(ty.results, &signature.results),
    )
}

/// # Safety
/// The frame has a function of the instance for its callee.
unsafe fn callee_of(instance: &Instance, fp: usize) -> FuncIndex {
    let callee = ptr::with_exposed_provenance::<HostCell>(fp + frame::CALLEE as usize);
    // SAFETY: the caller's.
    instance.func_with_slot(unsafe { instance.engine().callable_of(callee.read()) }.slot)
}

/// # Safety
/// For the stub that enters.
#[native_abi]
pub(crate) unsafe fn put_arguments(
    instance: &Instance,
    fp: usize,
    registers: *mut u8,
    callee_frame: *mut u8,
) -> Outcome {
    instance.store().set_top_frame(fp);
    // SAFETY: the caller's.
    let func = unsafe { callee_of(instance, fp) };
    let (arguments, _) = values_of(instance, func, registers, callee_frame);
    // SAFETY: the host has called the stub.
    unsafe {
        instance
            .engine()
            .host()
            .put_arguments(instance, fp, Outgoing(arguments))
    }
    .into()
}

/// # Safety
/// For the stub that enters.
#[native_abi]
pub(crate) unsafe fn take_results(
    instance: &Instance,
    fp: usize,
    registers: *mut u8,
    callee_frame: *mut u8,
) -> Outcome {
    instance.store().set_top_frame(fp);
    // SAFETY: the caller's.
    let func = unsafe { callee_of(instance, fp) };
    let (_, results) = values_of(instance, func, registers, callee_frame);
    // SAFETY: the host has called the stub.
    unsafe {
        instance
            .engine()
            .host()
            .take_results(instance, fp, Incoming(results))
    }
    .into()
}

/// # Safety
/// For the stub that is in `slot`, in an import of the instance.
#[native_abi]
pub(crate) unsafe fn call_host(
    instance: &Instance,
    slot: usize,
    registers: *mut u8,
    fp: *mut u8,
) -> Outcome {
    instance.store().set_top_frame(fp.addr());
    let func = instance.func_with_slot(slot);
    let (arguments, results) = values_of(instance, func, registers, fp);
    let call = HostCall {
        instance,
        func,
        arguments: Incoming(arguments),
        results: Outgoing(results),
    };
    instance.engine().host().call(call).into()
}
