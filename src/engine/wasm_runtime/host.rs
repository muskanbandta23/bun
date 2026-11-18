//! What the run time needs of whoever owns the heap and the exceptions.

use core::time::Duration;
use std::rc::Rc;

use bun_engine_wasm_compiler::abi::{HostLayout, Trap};
use bun_engine_wasm_format::{FuncIndex, TypeIndex};

use crate::call::{HostCall, Incoming, Outgoing};
use crate::frames::Catch;
use crate::global::Global;
use crate::instance::Instance;
use crate::memory::Memory;
use crate::table::Table;
use crate::tag::Tag;
use crate::value::HostCell;
use crate::waiters::{Expected, Waited};

/// The address of what the host keeps for a thread and its heap.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Vm(pub usize);

/// The host has an exception pending.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Thrown;

/// What an instance defines and can share.
pub enum Owned {
    Memory(Rc<Memory>),
    Table(Rc<Table>),
    Global(Rc<Global>),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Urgency {
    Soon,
    /// Memories that cannot be reached are dropped before the call returns.
    Now,
}

/// The methods that make cells may collect garbage, and give `None` for want of memory.
///
/// # Safety
/// Cells and the VM are laid out as `layout` says. Cells are aligned to 8 bytes, do not move, and live while the
/// stack of the thread points at or into them or a live cell refers to them, which the `trace` methods of this crate
/// tell for its own objects. Those are dropped on the thread.
pub unsafe trait Host: Sync {
    /// The engine fills in `null_guard_size`.
    fn layout(&self) -> HostLayout;

    /// A cell for [`Host::new_object`] that keeps the canonical type, which is that of a struct or an array.
    fn new_type_data(&self, instance: &Instance, ty: TypeIndex) -> Option<HostCell>;
    /// A struct or an array of `size` bytes in all, with its kind filled in and zeros wherever the layout names
    /// another place. It keeps `type_data` alive.
    fn new_object(&self, vm: Vm, type_data: HostCell, size: usize) -> Option<HostCell>;
    /// With room for what the layout says. It keeps the instance alive.
    fn new_function(&self, instance: &Instance, func: FuncIndex) -> Option<HostCell>;
    /// With room for the tag and the payload. It keeps a clone of `tag`, and the references in the payload alive.
    fn new_exception(&self, instance: &Instance, tag: &Rc<Tag>) -> Option<HostCell>;
    /// A cell that keeps and traces the object.
    fn new_owner(&self, instance: &Instance, owned: Owned) -> Option<HostCell>;
    fn write_barrier(&self, vm: Vm, cell: HostCell);

    /// Makes an exception pending that WebAssembly cannot catch.
    fn throw_trap(&self, instance: &Instance, trap: Trap);
    fn throw(&self, instance: &Instance, exception: HostCell);
    /// The pending exception, as an object with a tag and a payload.
    fn take_exception(&self, instance: &Instance) -> HostCell;
    /// Where to go on with the pending exception. See [`crate::Engine::find_handler`].
    ///
    /// # Safety
    /// `fp` is that of the innermost frame.
    unsafe fn unwind(&self, vm: Vm, fp: usize) -> Catch;

    /// For imports with [`crate::Engine::host_call_stub`]. It sets every result to a value of its type, or throws.
    fn call(&self, call: HostCall<'_>) -> Result<(), Thrown>;
    /// It sets every argument to a value of its type, or throws.
    ///
    /// # Safety
    /// `fp` is that of a frame of [`crate::Engine::enter_stub`].
    unsafe fn put_arguments(
        &self,
        instance: &Instance,
        fp: usize,
        arguments: Outgoing<'_>,
    ) -> Result<(), Thrown>;
    /// Gives what the stub returns.
    ///
    /// # Safety
    /// As above.
    unsafe fn take_results(
        &self,
        instance: &Instance,
        fp: usize,
        results: Incoming<'_>,
    ) -> Result<u64, Thrown>;

    /// The bytes may have moved. For a memory that is not shared, this follows growth by zero pages too.
    fn memory_grew(&self, vm: Vm, memory: &Memory, old_length: u64);
    /// `owner` keeps this many more bytes outside the heap.
    fn report_allocation(&self, vm: Vm, owner: HostCell, bytes: usize);
    fn collect_garbage(&self, vm: Vm, urgency: Urgency);

    /// See [`crate::waiters`].
    ///
    /// # Safety
    /// The address is aligned and in a shared memory of the instance.
    unsafe fn wait(
        &self,
        instance: &Instance,
        address: usize,
        expected: Expected,
        timeout: Option<Duration>,
    ) -> Result<Waited, Thrown>;
    fn notify(&self, address: usize, count: u32) -> u32;
}
