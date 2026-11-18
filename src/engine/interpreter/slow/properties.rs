use bun_engine_bytecode::PutFlags;
use bun_engine_text::{names, symbols};
use bun_engine_vm::code::metadata::{
    ArrayAccess, CasesAndValue, Flags, GetById, GetByVal, GetLength,
};
use bun_engine_vm::ic::{self, AccessCases, Key, StoreCache, StoreMode};
use bun_engine_vm::{
    ArrayProfile, Cx, Field, Gc, JsCell, JsObject, JsResult, JsTypedArray,
    OFFSET_OF_INTERNAL_FIELDS, PutPropertySlot, TypeFlags, Value, errors,
};

use super::{Frame, Next, execute};

fn record_element_access(profile: &ArrayProfile, base: Value, index: u32, is_store: bool) {
    profile.record_base(base);
    if let Some(object) = base.as_object() {
        profile.record_index(&object, index, is_store);
    }
}

/// For a key that is no array index.
fn typed_element(
    profile: &ArrayProfile,
    base: Value,
    key: Value,
) -> Option<(Gc<JsTypedArray>, usize)> {
    let index = JsTypedArray::index_for_number(key.as_number()?);
    let array = base.downcast::<JsTypedArray>()?;
    profile.record_base(base);
    profile.record_typed_index(&array, index);
    Some((array, index))
}

pub(super) fn get_by_val(
    cx: &mut Cx,
    f: Frame<'_>,
    array: &ArrayProfile,
    cases: &AccessCases,
    base: Value,
    key: Value,
) -> JsResult<Value> {
    if let Some(index) = key.as_array_index() {
        record_element_access(array, base, index, false);
        return base.get_index(cx, index);
    }
    if let Some((typed_array, index)) = typed_element(array, base, key) {
        return Ok(typed_array.get_element(cx, index).unwrap_or_default());
    }
    if base.is_undefined_or_null() {
        return Err(errors::throw_not_an_object(cx, base));
    }
    let key = key.to_property_key(cx)?;
    if let Some(index) = key.name().as_index() {
        return base.get_index(cx, index);
    }
    ic::get_by_key(
        cx,
        f.code_block,
        cases,
        base,
        base,
        key.name(),
        Key::Variable,
    )
}

pub(super) fn put_by_val(
    cx: &mut Cx,
    f: Frame<'_>,
    entry: &ArrayAccess,
    base: Value,
    key: Value,
    value: Value,
    mode: StoreMode,
) -> JsResult<()> {
    let put_index = |cx: &mut Cx, index: u32| match mode {
        StoreMode::Set { is_strict } => base.put_index(cx, index, value, is_strict),
        StoreMode::Define { is_strict } => {
            let object = base.cast::<JsObject>();
            object.create_data_index(cx, index.into(), value, is_strict)
        }
    };
    if let Some(index) = key.as_array_index() {
        record_element_access(&entry.array, base, index, true);
        return put_index(cx, index).map(|_| ());
    }
    if let StoreMode::Set { .. } = mode
        && let Some((typed_array, index)) = typed_element(&entry.array, base, key)
    {
        return typed_array.set_element(cx, index, value);
    }
    if base.is_undefined_or_null() {
        return Err(errors::throw_not_an_object(cx, base));
    }
    let key = key.to_property_key(cx)?;
    if let Some(index) = key.name().as_index() {
        return put_index(cx, index).map(|_| ());
    }
    ic::put_by_key(
        cx,
        f.code_block,
        &entry.cases,
        base,
        key.name(),
        value,
        mode,
    )
}

pub(super) fn right_side_of_in(cx: &Cx, base: Value) -> JsResult<Gc<JsObject>> {
    base.as_object()
        .ok_or_else(|| errors::throw_invalid_in_parameter(cx, base))
}

pub(super) fn in_by_val(
    cx: &mut Cx,
    f: Frame<'_>,
    entry: &ArrayAccess,
    base: Value,
    key: Value,
) -> JsResult<bool> {
    let object = right_side_of_in(cx, base)?;
    if let Some(index) = key.as_array_index() {
        record_element_access(&entry.array, base, index, false);
        return object.has_index(cx, index.into());
    }
    let key = key.to_property_key(cx)?;
    if let Some(index) = key.name().as_index() {
        return object.has_index(cx, index.into());
    }
    ic::has_by_key(
        cx,
        f.code_block,
        &entry.cases,
        object,
        key.name(),
        Key::Variable,
    )
}

/// # Safety
/// The cell has that many internal fields.
unsafe fn internal_field(cell: &JsCell, index: u32) -> &Field<Option<Value>> {
    let address = cell.address() + OFFSET_OF_INTERNAL_FIELDS + index as usize * size_of::<u64>();
    // SAFETY: the caller's.
    unsafe { &*core::ptr::with_exposed_provenance(address) }
}

execute! {
    GetById(op, cx, f) {
        let cache = &f.entry::<GetById>(op.metadata).cache;
        let value = cache.get_slow(cx, f.code_block, f.get(op.base), f.name(op.name))?;
        f.set_profiled(op.dst, op.metadata, value);
    }
    GetLength(op, cx, f) {
        let (entry, base) = (f.entry::<GetLength>(op.metadata), f.get(op.base));
        entry.array.record_base(base);
        let value = if let Some(string) = base.as_string() {
            string.len().into()
        } else if let Some(array) = base.downcast::<bun_engine_vm::JsArray>() {
            array.length().into()
        } else {
            entry.cache.get_slow(cx, f.code_block, base, names::length)?
        };
        f.set_profiled(op.dst, op.metadata, value);
    }
    GetByIdDirect(op, cx, f) {
        let (cache, base) = (&f.entry::<GetById>(op.metadata).cache, f.get(op.base).to_object(cx)?);
        let value = ic::get_own_by_id(cx, f.code_block, cache, base, f.name(op.name))?;
        f.set_profiled(op.dst, op.metadata, value.unwrap_or_default());
    }
    GetByIdWithThis(op, cx, f) {
        let cases = &f.entry::<CasesAndValue>(op.metadata).cases;
        let (base, this, name) = (f.reg(op.base), f.get(op.this), f.name(op.name));
        let value = ic::get_by_key(cx, f.code_block, cases, base, this, name, Key::Fixed)?;
        f.set_profiled(op.dst, op.metadata, value);
    }
    GetByVal(op, cx, f) {
        let entry = f.entry::<GetByVal>(op.metadata);
        let value = get_by_val(cx, f, &entry.array, &entry.cases, f.get(op.base), f.get(op.key))?;
        f.set_profiled(op.dst, op.metadata, value);
    }
    GetByValWithThis(op, cx, f) {
        let (base, cases) = (f.get(op.base), &f.entry::<GetByVal>(op.metadata).cases);
        if base.is_undefined_or_null() {
            return Err(errors::throw_not_an_object(cx, base));
        }
        let key = f.get(op.key).to_property_key(cx)?;
        let value = ic::get_by_key(cx, f.code_block, cases, base, f.get(op.this), key.name(), Key::Variable)?;
        f.set_profiled(op.dst, op.metadata, value);
    }
    PutById(op, cx, f) {
        let is_strict = op.flags.contains(PutFlags::STRICT);
        let mode = if op.flags.contains(PutFlags::DIRECT) {
            StoreMode::Define { is_strict: true }
        } else {
            StoreMode::Set { is_strict }
        };
        let cache = f.entry::<StoreCache>(op.metadata);
        cache.put_slow(cx, f.code_block, f.get(op.base), f.name(op.name), f.get(op.value), mode)?;
    }
    PutByIdWithThis(op, cx, f) {
        let mut slot = PutPropertySlot::new(f.get(op.this), op.strict);
        f.reg(op.base).put(cx, f.name(op.name), f.get(op.value), &mut slot)?;
    }
    PutByVal(op, cx, f) {
        let mode = StoreMode::Set { is_strict: op.strict };
        put_by_val(cx, f, f.entry(op.metadata), f.get(op.base), f.get(op.key), f.get(op.value), mode)?;
    }
    PutByValDirect(op, cx, f) {
        let mode = StoreMode::Define { is_strict: true };
        put_by_val(cx, f, f.entry(op.metadata), f.get(op.base), f.get(op.key), f.get(op.value), mode)?;
    }
    PutByValWithThis(op, cx, f) {
        let base = f.get(op.base);
        if base.is_undefined_or_null() {
            return Err(errors::throw_not_an_object(cx, base));
        }
        let key = f.get(op.key).to_property_key(cx)?;
        let mut slot = PutPropertySlot::new(f.get(op.this), op.strict);
        base.put(cx, key.name(), f.get(op.value), &mut slot)?;
    }
    DeleteById(op, cx, f) {
        let (object, cases) = (f.get(op.base).to_object(cx)?, f.entry(op.metadata));
        let name = f.name(op.name);
        f.set(op.dst, ic::delete_by_key(cx, f.code_block, cases, object, name, Key::Fixed, op.strict)?);
    }
    DeleteByVal(op, cx, f) {
        let (object, key) = (f.get(op.base).to_object(cx)?, f.get(op.key));
        let did_delete = if let Some(index) = key.as_array_index() {
            let did_delete = object.delete_property_by_index(cx, index)?;
            if !did_delete && op.strict {
                return Err(cx.throw_type_error(errors::UNABLE_TO_DELETE_PROPERTY));
            }
            did_delete
        } else {
            let (key, cases) = (key.to_property_key(cx)?, f.entry(op.metadata));
            ic::delete_by_key(cx, f.code_block, cases, object, key.name(), Key::Variable, op.strict)?
        };
        f.set(op.dst, did_delete);
    }
    InById(op, cx, f) {
        let (object, cases) = (right_side_of_in(cx, f.get(op.base))?, f.entry(op.metadata));
        f.set(op.dst, ic::has_by_key(cx, f.code_block, cases, object, f.name(op.name), Key::Fixed)?);
    }
    InByVal(op, cx, f) {
        f.set(op.dst, in_by_val(cx, f, f.entry(op.metadata), f.get(op.base), f.get(op.key))?);
    }
    GetPrototypeOf(op, cx, f) {
        f.set_profiled(op.dst, op.metadata, f.get(op.value).get_prototype_of(cx)?);
    }
    SetPrototype(op, cx, f) {
        f.cell::<JsObject>(op.object).set_prototype_direct(cx, f.get(op.prototype));
    }
    GetHasInstance(op, cx, f) {
        let constructor = f.get(op.constructor);
        if !constructor.is_object() {
            return Err(cx.throw_type_error("Right hand side of instanceof is not an object"));
        }
        let cache = &f.entry::<GetById>(op.metadata).cache;
        let value = cache.get_slow(cx, f.code_block, constructor, symbols::hasInstance)?;
        f.set_profiled(op.dst, op.metadata, value);
    }
    InstanceofCustom(op, cx, f) {
        let (value, method) = (f.get(op.value), f.reg(op.has_instance));
        let constructor = f.get(op.constructor).cast::<JsObject>();
        let is_default = method == cx.function_prototype_has_instance().into()
            && constructor.inline_flags().contains(TypeFlags::IMPLEMENTS_DEFAULT_HAS_INSTANCE);
        if !is_default {
            f.entry::<Flags>(op.metadata).0.set(1);
            f.set(op.dst, constructor.has_instance_with(cx, value, method)?);
        } else if !value.is_object() {
            f.set(op.dst, false);
        }
        Next::jump_if(!is_default || !value.is_object())
    }
    Instanceof(op, cx, f) {
        let (value, prototype) = (f.get(op.value), f.reg(op.prototype));
        f.set(op.dst, ic::instance_of(cx, f.code_block, f.entry(op.metadata), value, prototype)?);
    }
    GetInternalField(op, cx, f) {
        let base = f.get(op.base).cast::<JsCell>();
        // SAFETY: only built-ins have the instruction, and they have checked the type.
        let value = unsafe { internal_field(&base, op.index) }.get();
        f.entry::<bun_engine_vm::ValueProfile>(op.metadata).record_bits(Value::bits_of(value));
        f.set_or_empty(op.dst, value);
    }
    PutInternalField(op, cx, f) {
        let base = f.get(op.base).cast::<JsCell>();
        // SAFETY: as above.
        unsafe { internal_field(&base, op.index).set(&base, f.get_or_empty(op.value)) };
    }
}
