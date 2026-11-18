use bun_engine_bytecode::{Register, Width, op};
use bun_engine_vm::code::metadata::{self, Flags, ForInGet, GetPrivateName, PutPrivateName};
use bun_engine_vm::heap::Allocator;
use bun_engine_vm::ic::{self, AccessCases, AccessKind};
use bun_engine_vm::{
    CallFrame, EnumerationMode, ForIn, JsObject, JsSymbol, MappedArguments, RealmSlot, Value,
    ValueProfile,
};

use super::barrier_and_next;
use crate::machine::{EMPTY, Machine, boolean, cell, go, handler, jump, next, slow};

impl<W: Width> Machine<W> {
    #[inline(always)]
    unsafe fn for_in(&self, state: Register) -> ForIn {
        // SAFETY: what an instruction has put there.
        unsafe {
            let part = |index| Value::from_bits(self.reg(state.nth(index)));
            ForIn::from_registers([part(0), part(1), part(2)])
        }
    }
}

#[inline(always)]
fn has_seen(modes: &Flags, mode: EnumerationMode) -> bool {
    modes.0.get() & mode as u32 != 0
}

handler! {
    fn get_from_arguments(m, op: op::GetFromArguments) {
        let value = cell::<MappedArguments>(m.reg(op.arguments)).slot_unchecked(op.index).get().to_bits();
        m.entry::<ValueProfile>(op.metadata).record_bits(value);
        m.set(op.dst, value);
        next!(m, op)
    }

    fn put_to_arguments(m, op: op::PutToArguments) {
        let (arguments, value) = (cell::<MappedArguments>(m.reg(op.arguments)), m.get(op.value));
        arguments.slot_unchecked(op.index).set_without_barrier(Value::from_bits(value));
        barrier_and_next!(m, op: op::PutToArguments, arguments, value)
    }

    fn ensure_arguments(m, op: op::EnsureArguments) {
        if m.reg(op.arguments) == EMPTY {
            slow!(m, op::EnsureArguments)
        }
        next!(m, op)
    }

    fn for_in_next(m, op: op::ForInNext) {
        let mut for_in = m.for_in(op.state);
        if let Some(name) = for_in.try_next(Value::from_bits(m.reg(op.base)))
            && has_seen(&m.entry::<metadata::ForIn>(op.metadata).modes, EnumerationMode::OwnShape)
        {
            m.set(op.state.nth(2), m.box_int32(for_in.index as i32));
            m.set(op.name, name.address() as u64);
            next!(m, op)
        }
        if m.for_in(op.state).is_at_last_name() {
            jump!(m, op.done)
        }
        go!(m, for_in_next_index::<W>)
    }

    #[inline(never)]
    fn for_in_next_index(m, op: op::ForInNext) {
        let (mut for_in, entry) = (m.for_in(op.state), m.entry::<metadata::ForIn>(op.metadata));
        if let Some(object) = m.as_object(m.reg(op.base))
            && let Some(name) = for_in.try_next_index(m.vm, &object)
            && has_seen(&entry.modes, EnumerationMode::Indexed)
        {
            entry.array.record_shape(&object);
            m.set(op.state.nth(2), m.box_int32(for_in.index as i32));
            m.set(op.name, name.address() as u64);
            next!(m, op)
        }
        slow!(m, op::ForInNext)
    }

    fn create_direct_arguments(m, op: op::CreateDirectArguments) {
        let (frame, code_block) = (m.frame(), m.code_block());
        let (length, parameters) = (frame.argument_count() as u32, code_block.parameters() - 1);
        let slot_count = length.max(parameters);
        let size = MappedArguments::offset_of_slot(slot_count);
        let Some(shape) = code_block.global().slot_if_initialized(RealmSlot::direct_arguments_shape) else {
            slow!(m, op::CreateDirectArguments)
        };
        let Some(address) = m.vm.heap.cells.allocator_for(size).and_then(Allocator::try_bump) else {
            slow!(m, op::CreateDirectArguments)
        };
        MappedArguments::initialize_direct(address, shape.cast_unchecked(), frame.callee(), length, parameters);
        let slots = core::ptr::with_exposed_provenance_mut::<u64>(address + MappedArguments::OFFSET_OF_SLOTS);
        let arguments = m.fp.byte_add(CallFrame::OFFSET_OF_ARGUMENTS);
        // The frame has been padded. Not to be made a call of `memcpy`.
        for index in 0..slot_count as usize {
            slots.add(index).write_volatile(*arguments.add(index));
        }
        m.publish();
        m.set(op.dst, address as u64);
        next!(m, op)
    }

    fn for_in_get(m, op: op::ForInGet) {
        let (for_in, base) = (m.for_in(op.state), m.get(op.base));
        let entry = m.entry::<ForInGet>(op.metadata);
        let value = if let Some((object, offset)) = for_in.object_with_slot(Value::from_bits(base)) {
            object.slot_unchecked(offset).get()
        } else if for_in.mode == EnumerationMode::Indexed && let Some(object) = m.as_object(base) {
            entry.for_in.array.record_shape(&object);
            object.try_get_dense(for_in.index)
        } else {
            None
        };
        let Some(value) = value.filter(|_| has_seen(&entry.for_in.modes, for_in.mode)) else {
            slow!(m, op::ForInGet)
        };
        entry.value.record(value);
        m.set(op.dst, value.to_bits());
        next!(m, op)
    }

    fn for_in_put(m, op: op::ForInPut) {
        let (for_in, modes) = (m.for_in(op.state), &m.entry::<metadata::ForIn>(op.metadata).modes);
        let Some((object, offset)) = for_in
            .object_with_writable_slot(Value::from_bits(m.get(op.base)))
            .filter(|_| has_seen(modes, EnumerationMode::OwnShape))
        else {
            slow!(m, op::ForInPut)
        };
        let value = m.get(op.value);
        object.slot_unchecked(offset).set_without_barrier(Some(Value::from_bits(value)));
        barrier_and_next!(m, op: op::ForInPut, object, value)
    }
}

macro_rules! for_in_tests {
    ($($name:ident, $Op:ident;)*) => { handler! {$(
        fn $name(m, op: op::$Op) {
            if m.for_in(op.state).object_with_slot(Value::from_bits(m.get(op.base))).is_none()
                || !has_seen(&m.entry::<metadata::ForIn>(op.metadata).modes, EnumerationMode::OwnShape)
            {
                slow!(m, op::$Op)
            }
            m.set(op.dst, boolean(true));
            next!(m, op)
        }
    )*} };
}

for_in_tests! {
    for_in_has, ForInHas;
    for_in_has_own, ForInHasOwn;
}

/// The case in the list for the shape of the object and the symbol in the register.
macro_rules! find_case {
    ($m:ident, $cases:expr, $object:ident, $symbol:expr) => {{
        let name = cell::<JsSymbol>($m.reg($symbol)).name().address();
        let mut case = $cases.and_then(AccessCases::first);
        while let Some(current) = case
            && !current.matches($object.shape_id(), name)
        {
            case = current.next();
        }
        case
    }};
}

handler! {
    fn get_private_name(m, op: op::GetPrivateName) {
        let base = m.get(op.base);
        if !m.is_cell(base) {
            slow!(m, op::GetPrivateName)
        }
        let (object, entry) = (cell::<JsObject>(base), m.entry::<GetPrivateName>(op.metadata));
        if object.shape_id() != entry.cache.shape() || m.reg(op.name) != entry.name.bits() as u64 {
            go!(m, get_private_name_from_cases::<W>; base)
        }
        let value = Value::bits_of(ic::slot_at(&object, entry.cache.byte_offset()).get());
        entry.value.record_bits(value);
        m.set(op.dst, value);
        next!(m, op)
    }

    #[inline(never)]
    fn get_private_name_from_cases(m, op: op::GetPrivateName, base) {
        let (object, entry) = (cell::<JsObject>(base), m.entry::<GetPrivateName>(op.metadata));
        let Some(case) = find_case!(m, entry.cache.cases(), object, op.name) else {
            slow!(m, op::GetPrivateName)
        };
        let value = Value::bits_of(ic::slot_at(&object, case.byte_offset()).get());
        entry.value.record_bits(value);
        m.set(op.dst, value);
        next!(m, op)
    }

    fn check_private_brand(m, op: op::CheckPrivateBrand) {
        let base = m.get(op.base);
        if !m.is_cell(base) {
            slow!(m, op::CheckPrivateBrand)
        }
        let (object, entry) = (cell::<JsObject>(base), m.entry::<PutPrivateName>(op.metadata));
        if (object.shape_id() != entry.cache.shape() || m.reg(op.brand) != entry.name.bits() as u64)
            && find_case!(m, entry.cache.cases(), object, op.brand).is_none()
        {
            slow!(m, op::CheckPrivateBrand)
        }
        next!(m, op)
    }
}

macro_rules! private_stores {
    ($($name:ident, $from_cases:ident, $Op:ident, $symbol:ident, |$m:ident, $op:ident| ($base:expr, $value:expr);)*) => { handler! {$(
        fn $name($m, $op: op::$Op) {
            let base = $base;
            if !$m.is_cell(base) {
                slow!($m, op::$Op)
            }
            let (object, entry) = (cell::<JsObject>(base), $m.entry::<PutPrivateName>($op.metadata));
            let cache = &entry.cache;
            if object.shape_id() != cache.shape() || $m.reg($op.$symbol) != entry.name.bits() as u64 {
                go!($m, $from_cases::<W>; base)
            }
            ic::slot_at(&object, cache.byte_offset()).set_without_barrier(Some(Value::from_bits($value)));
            if !cache.new_shape().is_none() {
                object.set_shape_id_directly(cache.new_shape());
            }
            barrier_and_next!($m, $op: op::$Op, object)
        }

        #[inline(never)]
        fn $from_cases($m, $op: op::$Op, base) {
            let (object, entry) = (cell::<JsObject>(base), $m.entry::<PutPrivateName>($op.metadata));
            let case = find_case!($m, entry.cache.cases(), object, $op.$symbol);
            let Some(case) = case.filter(|it| it.kind() != AccessKind::TransitionAndGrow) else {
                slow!($m, op::$Op)
            };
            ic::slot_at(&object, case.byte_offset()).set_without_barrier(Some(Value::from_bits($value)));
            if !case.new_shape().is_none() {
                object.set_shape_id_directly(case.new_shape());
            }
            barrier_and_next!($m, $op: op::$Op, object)
        }
    )*} };
}

private_stores! {
    put_private_name, put_private_name_from_cases, PutPrivateName, name,
        |m, op| (m.get(op.base), m.get(op.value));
    define_private_name, define_private_name_from_cases, DefinePrivateName, name,
        |m, op| (m.reg(op.base), m.get(op.value));
    set_private_brand, set_private_brand_from_cases, SetPrivateBrand, brand,
        |m, op| (m.reg(op.base), JsObject::PRIVATE_BRAND_VALUE.to_bits());
}
