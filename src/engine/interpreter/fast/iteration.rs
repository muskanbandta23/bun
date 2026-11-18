use bun_engine_bytecode::{Register, Width, op};
use bun_engine_vm::code::metadata::{Flags, IteratorNext};
use bun_engine_vm::{
    ArrayAllocationProfile, CallFrame, CellType, ElementKind, Gc, ImmutableArray, IterationMode,
    IteratorRecord, JsArray, JsCell, JsObject, Shape, Storage, Value,
};

use crate::machine::{EMPTY, Machine, UNDEFINED, cell, handler, jump, next, slow};

const ARRAY: u64 = IterationMode::Array.marker().to_bits();
const DONE: u64 = IteratorRecord::DONE.to_bits();
const WORD: usize = size_of::<u64>();

#[inline(always)]
fn has_seen(modes: &Flags, mode: IterationMode) -> bool {
    modes.0.get() & mode.flag() != 0
}

impl<W: Width> Machine<W> {
    #[inline(always)]
    unsafe fn open_array(&self, state: Register, array: u64) {
        // SAFETY: the caller's.
        unsafe {
            self.set(state, array);
            self.set(state.nth(1), ARRAY);
            self.set(state.nth(2), self.box_int32(0));
        }
    }

    #[inline(always)]
    unsafe fn allocate_array(&self, shape: Gc<Shape>, storage: usize) -> Option<u64> {
        let address = (self.vm.heap.cells.allocator_for(size_of::<JsArray>())?).try_bump()?;
        let words = [JsCell::header_for(&shape), storage as u64];
        // SAFETY: a new cell.
        unsafe { core::ptr::with_exposed_provenance_mut::<[u64; 2]>(address).write(words) };
        Some(address as u64)
    }

    /// An array of that length, whose elements the caller writes, and where they go. There is room for `room`.
    #[inline(always)]
    unsafe fn allocate_array_with_storage(
        &self,
        kind: ElementKind,
        length: u32,
        room: u32,
    ) -> Option<(u64, *mut u64)> {
        let size = Storage::size(0, Some(Storage::min_vector_length(room)));
        let allocator = self.vm.heap.auxiliary.allocator_for(size)?;
        let lengths = core::ptr::with_exposed_provenance_mut::<u64>(allocator.try_bump()?);
        let vector_length = allocator.cell_size() / WORD - 1;
        // SAFETY: new, and of that size.
        unsafe {
            let (elements, shape) = (lengths.add(1), self.code_block().global().array_shape(kind));
            let array = self.allocate_array(shape, elements.addr())?;
            lengths.write((vector_length as u64) << 32 | u64::from(length));
            for index in length as usize..vector_length {
                elements.add(index).write_volatile(kind.hole());
            }
            Some((array, elements))
        }
    }
}

handler! {
    fn iterator_open(m, op: op::IteratorOpen) {
        let (iterable, method, modes) = (m.get(op.iterable), m.reg(op.method), m.entry::<Flags>(op.metadata));
        let Some(cell_type) = m.as_cell(iterable).map(|it| it.cell_type()) else { slow!(m, op::IteratorOpen) };
        if cell_type == CellType::Array {
            let global = m.code_block().global();
            if method == global.array_prototype_values().address() as u64
                && global.array_iterator_protocol_is_pristine()
                && has_seen(modes, IterationMode::Array)
            {
                m.open_array(op.state, iterable);
                jump!(m, op.opened)
            }
        } else if matches!(cell_type, CellType::Generator | CellType::FinalObject | CellType::IteratorHelper)
            && m.has_type(method, CellType::Function)
            && has_seen(modes, IterationMode::Generic)
        {
            m.set(op.state.nth(2), m.box_int32(0));
            next!(m, op)
        }
        slow!(m, op::IteratorOpen)
    }

    fn iterator_next(m, op: op::IteratorNext) {
        let (method, entry) = (m.reg(op.state.nth(1)), m.entry::<IteratorNext>(op.metadata));
        if method == ARRAY && has_seen(&entry.modes, IterationMode::Array) {
            let (array, index) = (cell::<JsArray>(m.reg(op.state)), m.reg(op.state.nth(2)) as u32);
            if let Some(value) = array.try_get_dense(index) {
                entry.array.record_shape(&array);
                entry.value.record(value);
                m.set(op.state.nth(2), m.box_int32(index as i32 + 1));
                m.set(op.value, value.to_bits());
                next!(m, op)
            }
            if index >= array.length() {
                m.set(op.state.nth(2), DONE);
                jump!(m, op.done)
            }
        } else if !IterationMode::is_marker(Value::from_bits(method)) && has_seen(&entry.modes, IterationMode::Generic) {
            jump!(m, op.generic)
        }
        slow!(m, op::IteratorNext)
    }

    fn iterator_begin(m, op: op::IteratorBegin) {
        let (iterable, global) = (m.get(op.iterable), m.code_block().global());
        if !m.has_type(iterable, CellType::Array)
            || !global.is_original_array_shape(&cell::<JsCell>(iterable).shape())
            || !global.array_iterator_protocol_is_pristine()
            || !has_seen(m.entry(op.metadata), IterationMode::Array)
        {
            slow!(m, op::IteratorBegin)
        }
        m.open_array(op.state, iterable);
        next!(m, op)
    }

    fn iterator_step(m, op: op::IteratorStep) {
        let entry = m.entry::<IteratorNext>(op.metadata);
        if m.reg(op.state.nth(1)) != ARRAY || !has_seen(&entry.modes, IterationMode::Array) {
            slow!(m, op::IteratorStep)
        }
        let (array, index) = (cell::<JsArray>(m.reg(op.state)), m.reg(op.state.nth(2)) as u32);
        let (value, position) = match array.try_get_dense(index) {
            Some(value) => (value.to_bits(), m.box_int32(index as i32 + 1)),
            None if index >= array.length() => (UNDEFINED, DONE),
            None => slow!(m, op::IteratorStep),
        };
        entry.array.record_shape(&array);
        entry.value.record_bits(value);
        m.set(op.state.nth(2), position);
        m.set(op.value, value);
        next!(m, op)
    }

    fn iterator_close(m, op: op::IteratorClose) {
        if m.reg(op.state.nth(1)) != ARRAY
            || !m.code_block().global().array_iterator_protocol_is_pristine()
            || !has_seen(m.entry(op.metadata), IterationMode::Array)
        {
            slow!(m, op::IteratorClose)
        }
        next!(m, op)
    }

    fn new_array(m, op: op::NewArray) {
        let profile = m.entry::<ArrayAllocationProfile>(op.metadata);
        let (kind, room) = profile.update();
        let kind = kind.max(op.shape.into());
        if kind != ElementKind::Contiguous {
            for index in 0..op.count {
                let element = m.reg(op.first.nth(index));
                if element != EMPTY && ElementKind::for_value(Value::from_bits(element)) > kind {
                    slow!(m, op::NewArray)
                }
            }
        }
        let Some((array, elements)) = m.allocate_array_with_storage(kind, op.count, op.count.max(room)) else {
            slow!(m, op::NewArray)
        };
        for index in 0..op.count {
            let mut element = m.reg(op.first.nth(index));
            if kind == ElementKind::Double {
                element = m.to_double(element).map_or(kind.hole(), f64::to_bits);
            }
            elements.add(index as usize).write_volatile(element);
        }
        m.publish();
        profile.did_allocate(cell(array));
        m.set(op.dst, array);
        next!(m, op)
    }

    fn new_array_from_constant(m, op: op::NewArrayFromConstant) {
        let (code_block, profile) = (m.code_block(), m.entry::<ArrayAllocationProfile>(op.metadata));
        let elements = cell::<ImmutableArray>(*code_block.constants_ptr().add(op.elements.index()));
        let global = code_block.global();
        if profile.update().0 > elements.kind() || global.is_having_a_bad_time() {
            slow!(m, op::NewArrayFromConstant)
        }
        let shape = global.original_array_shape(elements.indexing_mode());
        let Some(array) = m.allocate_array(shape, elements.storage().address()) else {
            slow!(m, op::NewArrayFromConstant)
        };
        m.publish();
        profile.did_allocate(cell(array));
        m.set(op.dst, array);
        next!(m, op)
    }

    fn new_array_with_size(m, op: op::NewArrayWithSize) {
        const MAX_LENGTH: u32 = 64;
        let (length, profile) = (m.get(op.length), m.entry::<ArrayAllocationProfile>(op.metadata));
        if !m.is_int32(length) || length as u32 > MAX_LENGTH {
            slow!(m, op::NewArrayWithSize)
        }
        let ((kind, room), length) = (profile.update(), length as u32);
        let Some((array, _)) = m.allocate_array_with_storage(kind, 0, length.max(room)) else {
            slow!(m, op::NewArrayWithSize)
        };
        cell::<JsObject>(array).storage().set_public_length(length);
        m.publish();
        profile.did_allocate(cell(array));
        m.set(op.dst, array);
        next!(m, op)
    }

    fn create_rest(m, op: op::CreateRest) {
        let count = (m.frame().argument_count() as u32).saturating_sub(op.skip);
        let Some((array, elements)) = m.allocate_array_with_storage(ElementKind::Contiguous, count, count) else {
            slow!(m, op::CreateRest)
        };
        let rest = m.fp.byte_add(CallFrame::OFFSET_OF_ARGUMENTS).add(op.skip as usize);
        for index in 0..count as usize {
            elements.add(index).write_volatile(*rest.add(index));
        }
        m.publish();
        m.set(op.dst, array);
        next!(m, op)
    }
}
