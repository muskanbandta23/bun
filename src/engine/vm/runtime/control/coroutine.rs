//! What generators, async functions, async generators and modules that await have in common: code that leaves its frame
//! and is entered again.
//!
//! The registers that the code still needs are saved at the end of the `Coroutine`, which is made with room for
//! `CodeBlock::saved_words` of them. Each is a value, a cell or empty. Code that suspends is never replaced, so they
//! mean the same when it goes on.
//!
//! | instruction              | what a tier does                                                                      |
//! | ------------------------ | ------------------------------------------------------------------------------------- |
//! | `create_generator`       | `JsGenerator::for_frame`                                                              |
//! | `create_async_generator` | `JsAsyncGenerator::for_frame`                                                         |
//! | `suspend`                | `save`, returns the value                                                             |
//! | `await`                  | `JsAsyncFunctionGenerator::ensure`, `await_value`, `save`, returns `promise`          |
//! | `debug_await`            | `Coroutine::ensure`                                                                   |
//! | `resume_generator`       | `mark_executing`, calls `callee` with a count of zero                                 |
//! | `resume_dispatch`        | if the count is zero, jumps to the `resume` of `point` of `Coroutine::of_frame`       |
//! | `resume`                 | `restore`                                                                             |
//! | `generator_return`       | `complete`, returns the value                                                         |
//! | `generator_throw`        | `complete`, throws                                                                    |
//! | `async_return`           | returns what `async_return` does                                                      |
//! | `async_throw`            | returns what `async_throw` does                                                       |
//! | `async_iterator_open`    | `open_async_iterator`                                                                 |
//! | `async_iterator_next`    | if `next` is `Vm::async_iteration_sentinel`: `Coroutine::ensure`, `request_async_step` |
//!
//! No script runs between `await_value` and `save`. `unwind` completes a coroutine whose resumed frame it removes.
//!
//! Inline, `save` stores the registers, then `saved_count` (after a store fence if a marker may be running), `point`
//! and the low half of `state`, and ends with the write barrier. `restore` loads them and makes `saved_count` zero.

use core::cell::Cell;
use core::mem::offset_of;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use bun_engine_bytecode::{Register, ResumeMode, SuspendKind, SuspendPointIndex};
use bun_engine_macros::cell;

use crate::heap::barrier;
use crate::{
    CallFrame, CellKind, CellType, Cx, Extends, Gc, JsAsyncFunctionGenerator, JsAsyncGenerator,
    JsFunction, JsGenerator, JsObject, JsObjectInit, JsResult, OFFSET_OF_INTERNAL_FIELDS, Shape,
    Value, Visitor, Vm,
};

/// The first four are those of `SuspendKind`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum CoroutineState {
    SuspendedStart,
    SuspendedYield,
    SuspendedDelegate,
    SuspendedAwait,
    Executing,
    Completed,
    /// Of an async generator whose body is done.
    DrainingQueue,
}

impl From<SuspendKind> for CoroutineState {
    #[inline(always)]
    fn from(kind: SuspendKind) -> CoroutineState {
        match kind {
            SuspendKind::Start => CoroutineState::SuspendedStart,
            SuspendKind::Yield => CoroutineState::SuspendedYield,
            SuspendKind::Delegate => CoroutineState::SuspendedDelegate,
            SuspendKind::Await => CoroutineState::SuspendedAwait,
        }
    }
}

#[cell(cell_type = Generator..=AsyncGenerator, visit_children)]
pub struct Coroutine {
    base: JsObject,
    /// The bits of an int32, for `get_internal_field`.
    state: Cell<u64>,
    point: Cell<u32>,
    saved_count: AtomicU32,
    /// A `JsFunction`, or the `JsCallee` of a module.
    callee: Gc<JsObject>,
}

impl Coroutine {
    pub const STATE_FIELD: u32 = 0;
    pub const OFFSET_OF_STATE: usize = offset_of!(Coroutine, state);
    pub const OFFSET_OF_POINT: usize = offset_of!(Coroutine, point);
    pub const OFFSET_OF_SAVED_COUNT: usize = offset_of!(Coroutine, saved_count);
    pub const OFFSET_OF_CALLEE: usize = offset_of!(Coroutine, callee);

    /// Of one of the three types.
    #[inline(always)]
    pub const fn offset_of_registers(cell_type: CellType) -> usize {
        const SIZES: u32 = u32::from_le_bytes([
            size_of::<JsGenerator>() as u8,
            size_of::<JsAsyncFunctionGenerator>() as u8,
            size_of::<JsAsyncGenerator>() as u8,
            0,
        ]);
        (SIZES >> ((cell_type.0 - CellType::Generator.0) * 8)) as u8 as usize
    }

    #[inline(always)]
    pub(super) fn init(vm: &Vm, shape: Gc<Shape>, frame: &CallFrame) -> CoroutineInit {
        CoroutineInit {
            base: JsObjectInit::new(vm, shape),
            state: Cell::new(Value::int32(CoroutineState::Executing as i32).to_bits()),
            point: Cell::new(0),
            saved_count: AtomicU32::new(0),
            callee: frame.callee(),
        }
    }

    /// For the code that `frame` runs.
    #[inline(always)]
    pub(super) fn allocate<T: CellKind + Extends<Coroutine>>(
        cx: &Cx,
        frame: &CallFrame,
        init: T::Init,
    ) -> JsResult<Gc<T>> {
        const { assert!(!core::mem::needs_drop::<T>()) };
        assert!(T::shape_of(&init).instance_type() == T::CELL_TYPE);
        let words = frame.code_block().map_or(0, |it| it.saved_words());
        let size = size_of::<T>() + words as usize * size_of::<u64>();
        let address = cx.heap.cells.try_allocate(&cx.heap, size);
        if address == 0 {
            return Err(cx.throw_out_of_memory());
        }
        // SAFETY: a new cell that is large enough. Nothing reads a register that has not been saved.
        unsafe {
            T::initialize(core::ptr::with_exposed_provenance_mut(address), init);
            barrier::publish_fence();
            Ok(Gc::from_address(address))
        }
    }

    /// OrdinaryCreateFromConstructor of the function that `frame` runs, as far as the shape goes.
    #[inline(always)]
    pub(super) fn shape_for_frame(
        cx: &mut Cx,
        frame: &CallFrame,
        base: fn(&Cx) -> Gc<Shape>,
    ) -> JsResult<Gc<Shape>> {
        let function = frame.callee().cast::<JsFunction>();
        // It is no constructor, so nothing else is cached there.
        match function.rare_data().and_then(|it| it.subclass_shape()) {
            Some(shape) => Ok(shape),
            None if function.is_native_or_builtin() => Ok(base(cx)),
            None => cx.subclass_shape(function.upcast(), base(cx)),
        }
    }

    /// `generator` is the operand of an instruction in the code that `frame` runs.
    #[inline(always)]
    pub fn ensure(cx: &Cx, frame: &CallFrame, generator: Option<Value>) -> JsResult<Gc<Coroutine>> {
        match generator {
            Some(generator) => Ok(generator.cast()),
            None => JsAsyncFunctionGenerator::for_frame(cx, frame).map(Gc::upcast),
        }
    }

    /// The one that the frame goes on with.
    #[inline(always)]
    pub fn of_frame(frame: &CallFrame) -> Option<Gc<Coroutine>> {
        if frame.argument_count_including_this() != 0 || frame.has_native_callee() {
            return None;
        }
        frame.this_value().downcast()
    }

    #[inline(always)]
    pub fn state(&self) -> CoroutineState {
        // SAFETY: from `set_state`, or from generated code that does the same.
        unsafe { core::mem::transmute(self.state.get() as u8) }
    }

    #[inline(always)]
    pub(super) fn set_state(&self, state: CoroutineState) {
        self.state.set(Value::int32(state as i32).to_bits());
    }

    #[inline(always)]
    pub fn is_suspended(&self) -> bool {
        (self.state() as u8) < CoroutineState::Executing as u8
    }

    /// Where it has suspended last.
    #[inline(always)]
    pub fn point(&self) -> SuspendPointIndex {
        SuspendPointIndex(self.point.get())
    }

    #[inline(always)]
    pub fn callee(&self) -> Gc<JsObject> {
        self.callee
    }

    #[inline(always)]
    fn registers(&self) -> *mut u64 {
        let address = self.address() + Self::offset_of_registers(self.cell_type());
        core::ptr::with_exposed_provenance_mut(address)
    }

    /// # Safety
    /// `frame` runs the code that this was made for and is about to return, and `registers` are those of `point`.
    #[inline(always)]
    pub unsafe fn save(
        &self,
        frame: &CallFrame,
        registers: &[Register],
        point: SuspendPointIndex,
        kind: SuspendKind,
    ) {
        let saved = self.registers();
        debug_assert!(saved.addr() + registers.len() * 8 <= self.address() + self.cell_size());
        for (index, register) in registers.iter().enumerate() {
            // SAFETY: the caller's. There is room for the most that any point saves.
            unsafe {
                let word = frame.slot(register.offset() as isize).get();
                AtomicU64::from_ptr(saved.add(index)).store(word, Ordering::Relaxed);
            }
        }
        barrier::publish_fence();
        (self.saved_count).store(registers.len() as u32, Ordering::Relaxed);
        self.point.set(point.0);
        self.set_state(kind.into());
        self.write_barrier_for_cell();
    }

    /// # Safety
    /// `frame` has been entered to go on with this, and `registers` are those of `point`.
    #[inline(always)]
    pub unsafe fn restore(&self, frame: &CallFrame, registers: &[Register]) {
        debug_assert!(self.saved_count.load(Ordering::Relaxed) as usize == registers.len());
        let saved = self.registers();
        for (index, register) in registers.iter().enumerate() {
            // SAFETY: the caller's.
            unsafe {
                frame
                    .slot(register.offset() as isize)
                    .set(*saved.add(index))
            };
        }
        self.saved_count.store(0, Ordering::Relaxed);
    }

    /// Panics unless it is suspended.
    #[inline(always)]
    pub fn mark_executing(&self) {
        assert!(self.is_suspended());
        self.set_state(CoroutineState::Executing);
    }

    #[inline(always)]
    pub fn complete(&self) {
        self.set_state(CoroutineState::Completed);
        self.saved_count.store(0, Ordering::Relaxed);
    }

    /// Runs the code until it leaves its frame. Panics unless it is suspended.
    #[inline(always)]
    pub(super) fn resume(&self, cx: &mut Cx, value: Value, mode: ResumeMode) -> JsResult<Value> {
        self.mark_executing();
        let mode = Value::int32(mode as i32);
        // SAFETY: as it asks.
        let result = unsafe { cx.resume(self.callee, self.into(), value, mode) };
        if result.is_err() {
            self.complete();
        }
        result
    }

    fn visit_children(&self, visitor: &mut Visitor) {
        let count = self.saved_count.load(Ordering::Acquire) as usize;
        for index in 0..count {
            // SAFETY: saved, and kept alive since.
            unsafe {
                let word = AtomicU64::from_ptr(self.registers().add(index));
                visitor.append_word(word.load(Ordering::Relaxed));
            }
        }
    }
}

const _: () = assert!(Coroutine::OFFSET_OF_STATE == OFFSET_OF_INTERNAL_FIELDS);
