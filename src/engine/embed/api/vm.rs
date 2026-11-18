use core::ffi::c_void;
use core::ptr::NonNull;
use core::sync::atomic::Ordering;

use bun_engine_macros::export;
use bun_engine_text::{PropertyName, STATIC_ATOM_COUNT};
use bun_engine_vm::heap::{HeapKind, Scope};
use bun_engine_vm::{Cx, Exception, Gc, RealmSlot, Trap, Value, Vm, VmRef};

unsafe extern "C" {
    safe fn engine_host_destroy_vm_members(vm: &Vm);
    safe fn engine_host_reconcile_weak_references(vm: &Vm, scope: Scope);
}

/// With one reference. The counts are what the generated headers say.
#[export]
fn vm_create(kind: HeapKind, static_atom_count: usize, realm_slot_count: usize) -> *mut Vm {
    assert!(static_atom_count == STATIC_ATOM_COUNT && realm_slot_count == RealmSlot::COUNT);
    let vm = bun_engine::create_vm(kind);
    // SAFETY: nothing else has it.
    let embedder = unsafe { vm.get() }.embedder();
    (embedder.destroy_members).set(Some(|vm| engine_host_destroy_vm_members(vm)));
    (embedder.reconcile_weak_references).set(Some(|vm, survivors| {
        engine_host_reconcile_weak_references(vm, survivors.scope());
    }));
    vm.into_raw().as_ptr()
}

#[export]
unsafe fn vm_release(vm: *const Vm) {
    // SAFETY: a reference that C++ holds.
    drop(unsafe { VmRef::from_raw(NonNull::new_unchecked(vm.cast_mut())) });
}

#[export]
fn vm_hard_stack_limit(vm: &Vm) -> *mut c_void {
    core::ptr::without_provenance_mut(vm.hard_stack_limit())
}

struct Task {
    run: unsafe extern "C" fn(*mut c_void),
    context: *mut c_void,
}

#[export]
unsafe fn vm_when_idle(vm: &Vm, run: unsafe extern "C" fn(*mut c_void), context: *mut c_void) {
    let task = Task { run, context };
    // SAFETY: the two go together.
    vm.when_idle(move |_| unsafe { (task.run)(task.context) });
}

#[export]
fn vm_clear_kept_objects(vm: &Vm) {
    vm.clear_kept_objects();
}

#[export]
fn vm_register_private_name(vm: &Vm, name: PropertyName<'_>, symbol: PropertyName<'_>) {
    let name = name.to_atom().expect("no symbol");
    vm.register_private_name(name, symbol.to_identifier());
}

#[export]
fn vm_delete_unlinked_code(vm: &Vm) {
    vm.clear_code_cache();
}

#[export]
fn vm_delete_all_code(vm: &Vm) {
    vm.clear_code_cache();
    vm.clear_source_caches();
}

#[export]
fn throw_value(cx: &Cx, value: Value) -> Option<Gc<Exception>> {
    let _ = cx.throw(value);
    cx.exception()
}

#[export]
fn vm_has_exception_after_handling_traps(vm: &Vm) -> bool {
    if vm.traps().is_pending(Trap::SHELL_TIMEOUT_CHECK)
        && let (Some(global), Some(check)) =
            (vm.entry_global(), vm.embedder().shell_timeout_check.get())
    {
        vm.traps().clear(Trap::SHELL_TIMEOUT_CHECK);
        // SAFETY: C++ has the right to use the heap, and lends it for the call.
        check(unsafe { Cx::from_global(global) });
    }
    if vm.traps().is_pending(Trap::TERMINATION) {
        vm.handle_termination_trap();
    }
    vm.has_exception()
}

/// From any thread.
#[export]
fn vm_fire_trap(vm: &Vm, trap: u32) {
    match Trap::from_bits_retain(trap) {
        Trap::TERMINATION => vm.traps().request_termination(),
        trap => vm.traps().fire(trap),
    }
}

#[export]
fn vm_termination_exception(vm: &Vm) -> Gc<Exception> {
    vm.termination_exception()
}

#[export]
fn vm_throw_termination(vm: &Vm) {
    vm.raise_termination();
}

/// Whether there was anything to cancel.
#[export]
fn vm_cancel_termination(vm: &Vm) -> bool {
    let was_pending = vm.traps().clear(Trap::TERMINATION);
    let was_thrown = vm.has_pending_termination_exception();
    if was_thrown {
        vm.clear_exception();
    }
    let was_requested = vm.traps().termination_flag().swap(false, Ordering::SeqCst);
    was_pending || was_thrown || was_requested
}

#[export]
fn vm_begin_termination_deferral(vm: &Vm) {
    vm.begin_termination_deferral();
}

#[export]
fn vm_end_termination_deferral(vm: &Vm, throws_now: bool) {
    if throws_now {
        vm.end_termination_deferral_and_throw();
    } else {
        vm.end_termination_deferral();
    }
}

#[export]
fn vm_is_termination_deferred(vm: &Vm) -> bool {
    vm.is_termination_deferred()
}

#[export]
fn vm_replace_is_handling_error(vm: &Vm, value: bool) -> bool {
    vm.replace_is_handling_error(value)
}
