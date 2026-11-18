use bun_engine_bytecode::op;
use bun_engine_vm::code::metadata::{GetById, GetLength};
use bun_engine_vm::ic::{self, AccessCases, AccessKind, StoreCache};
use bun_engine_vm::{
    IndexingMode, JsCell, JsObject, JsString, OFFSET_OF_INTERNAL_FIELDS, TypeFlags, Value,
    ValueProfile,
};

use super::barrier_and_next;
use crate::machine::{EMPTY, boolean, cell, go, handler, jump, next, slow};

macro_rules! loads {
    ($($name:ident, $from_cases:ident, $Op:ident, $Entry:ident, $base:ident;)*) => { handler! {$(
        #[inline(always)]
        fn $name(m, op: op::$Op) {
            let base = m.get(op.$base);
            if !m.is_cell(base) {
                slow!(m, op::$Op)
            }
            let (object, entry) = (cell::<JsObject>(base), m.entry::<$Entry>(op.metadata));
            if object.shape_id() != entry.cache.shape() {
                go!(m, $from_cases::<W>; base)
            }
            let value = match entry.cache.byte_offset() {
                0 => match entry.cache.inline_lookup_unchecked().constant() {
                    Some(value) => value.to_bits(),
                    None => slow!(m, op::$Op),
                },
                offset => Value::bits_of(ic::slot_at(&object, offset).get()),
            };
            entry.value.record_bits(value);
            m.set(op.dst, value);
            next!(m, op)
        }

        #[inline(never)]
        fn $from_cases(m, op: op::$Op, base) {
            let (object, entry) = (cell::<JsObject>(base), m.entry::<$Entry>(op.metadata));
            let mut case = entry.cache.cases().and_then(AccessCases::first);
            while let Some(current) = case {
                if current.shape() == object.shape_id() {
                    let Some(value) = current.try_load(object) else { break };
                    entry.value.record(value);
                    m.set(op.dst, value.to_bits());
                    next!(m, op)
                }
                case = current.next();
            }
            slow!(m, op::$Op)
        }
    )*} };
}

loads! {
    get_by_id, get_by_id_from_cases, GetById, GetById, base;
    get_by_id_direct, get_by_id_direct_from_cases, GetByIdDirect, GetById, base;
    get_has_instance, get_has_instance_from_cases, GetHasInstance, GetById, constructor;
    get_length_property, get_length_from_cases, GetLength, GetLength, base;
}

handler! {
    fn get_length(m, op: op::GetLength) {
        let base = m.get(op.base);
        if !m.is_cell(base) {
            slow!(m, op::GetLength)
        }
        let (object, entry) = (cell::<JsObject>(base), m.entry::<GetLength>(op.metadata));
        entry.array.record_shape(&object);
        let mode = object.indexing_mode();
        let length = if mode.is_array() && mode.bits() & IndexingMode::KIND_MASK != 0 {
            object.storage().public_length()
        } else if object.is_string() {
            cell::<JsString>(base).len()
        } else {
            go!(m, get_length_property::<W>)
        };
        if length > i32::MAX as u32 {
            slow!(m, op::GetLength)
        }
        let length = m.box_int32(length as i32);
        entry.value.record_bits(length);
        m.set(op.dst, length);
        next!(m, op)
    }

    fn put_by_id(m, op: op::PutById) {
        let base = m.get(op.base);
        if !m.is_cell(base) {
            slow!(m, op::PutById)
        }
        let (object, cache) = (cell::<JsObject>(base), m.entry::<StoreCache>(op.metadata));
        if object.shape_id() != cache.shape() {
            go!(m, put_by_id_from_cases::<W>; base)
        }
        let (value, slot) = (m.get(op.value), ic::slot_at(&object, cache.byte_offset()));
        if cache.new_shape().is_none() {
            slot.set_without_barrier(Some(Value::from_bits(value)));
            barrier_and_next!(m, op: op::PutById, object, value)
        }
        if !cache.inline_lookup_unchecked().is_valid() {
            slow!(m, op::PutById)
        }
        slot.set_without_barrier(Some(Value::from_bits(value)));
        object.set_shape_id_directly(cache.new_shape());
        barrier_and_next!(m, op: op::PutById, object)
    }

    #[inline(never)]
    fn put_by_id_from_cases(m, op: op::PutById, base) {
        let object = cell::<JsObject>(base);
        let mut case = m.entry::<StoreCache>(op.metadata).cases().and_then(AccessCases::first);
        while let Some(current) = case {
            if current.shape() == object.shape_id() {
                let kind = current.kind();
                if kind != AccessKind::Replace
                    && !(kind == AccessKind::Transition && current.lookup_unchecked().is_valid())
                {
                    break;
                }
                let value = Value::from_bits(m.get(op.value));
                ic::slot_at(&object, current.byte_offset()).set_without_barrier(Some(value));
                if kind == AccessKind::Transition {
                    object.set_shape_id_directly(current.new_shape());
                }
                barrier_and_next!(m, op: op::PutById, object)
            }
            case = current.next();
        }
        slow!(m, op::PutById)
    }

    fn get_prototype_of(m, op: op::GetPrototypeOf) {
        let object = m.as_object(m.get(op.value));
        let Some(object) = object.filter(|it| !it.overrides_get_prototype()) else {
            slow!(m, op::GetPrototypeOf)
        };
        let prototype = object.prototype_direct().to_bits();
        m.entry::<ValueProfile>(op.metadata).record_bits(prototype);
        m.set(op.dst, prototype);
        next!(m, op)
    }

    fn instanceof_custom(m, op: op::InstanceofCustom) {
        let global = m.code_block().global();
        let default = global.slot_if_initialized(bun_engine_vm::RealmSlot::function_prototype_has_instance);
        let flags = cell::<JsCell>(m.get(op.constructor)).inline_flags();
        if m.reg(op.has_instance) != default.map_or(EMPTY, |it| it.address() as u64)
            || !flags.contains(TypeFlags::IMPLEMENTS_DEFAULT_HAS_INSTANCE)
        {
            slow!(m, op::InstanceofCustom)
        }
        if m.as_object(m.get(op.value)).is_none() {
            m.set(op.dst, boolean(false));
            jump!(m, op.done)
        }
        next!(m, op)
    }

    fn get_internal_field(m, op: op::GetInternalField) {
        let fields = m.get(op.base) as usize + OFFSET_OF_INTERNAL_FIELDS;
        let value = *core::ptr::with_exposed_provenance::<u64>(fields).add(op.index as usize);
        m.entry::<ValueProfile>(op.metadata).record_bits(value);
        m.set(op.dst, value);
        next!(m, op)
    }

    fn put_internal_field(m, op: op::PutInternalField) {
        let (base, value) = (m.get(op.base), m.get(op.value));
        let fields = base as usize + OFFSET_OF_INTERNAL_FIELDS;
        let field = core::ptr::with_exposed_provenance_mut::<u64>(fields).add(op.index as usize);
        core::sync::atomic::AtomicU64::from_ptr(field).store(value, core::sync::atomic::Ordering::Relaxed);
        barrier_and_next!(m, op: op::PutInternalField, cell::<JsCell>(base), value)
    }
}
