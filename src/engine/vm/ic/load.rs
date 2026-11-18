use core::cell::Cell;
use core::mem::{offset_of, transmute};

use bun_engine_text::PropertyName;

use super::{
    AccessCase, AccessCases, AccessKind, Key, PrototypeLookup, is_live, shape_for_lookup, slot_at,
};
use crate::code::metadata::Entry;
use crate::heap::Survivors;
use crate::{
    Attributes, CellType, CodeBlock, CustomGetter, Cx, Field, Gc, GetterSetter, GlobalProxy,
    InternalMethod, Intrinsic, JsFunction, JsObject, JsResult, JsTypedArray, PropertySlot,
    ShapeFlags, ShapeId, Value, Visitor, Vm,
};

/// One case inline, and the others in a list.
#[repr(C)]
pub struct LoadCache {
    shape: Cell<ShapeId>,
    /// Of an own slot. Zero: the value is the constant of the `PrototypeLookup` that is in the place of the list.
    byte_offset: Cell<i32>,
    cases_or_lookup: AccessCases,
}

impl LoadCache {
    pub const OFFSET_OF_SHAPE: usize = offset_of!(LoadCache, shape);
    pub const OFFSET_OF_BYTE_OFFSET: usize = offset_of!(LoadCache, byte_offset);
    pub const OFFSET_OF_CASES_OR_LOOKUP: usize = offset_of!(LoadCache, cases_or_lookup);

    #[inline(always)]
    pub fn shape(&self) -> ShapeId {
        self.shape.get()
    }

    #[inline(always)]
    pub fn byte_offset(&self) -> i32 {
        self.byte_offset.get()
    }

    #[inline(always)]
    fn has_inline_lookup(&self) -> bool {
        self.byte_offset() == 0 && !self.shape().is_none()
    }

    /// # Safety
    /// There is a shape, and the offset is zero.
    #[inline(always)]
    pub unsafe fn inline_lookup_unchecked(&self) -> Gc<PrototypeLookup> {
        // SAFETY: visited with the code block.
        unsafe { Gc::from_address(self.cases_or_lookup.bits()) }
    }

    pub fn inline_lookup(&self) -> Option<Gc<PrototypeLookup>> {
        // SAFETY: checked.
        self.has_inline_lookup()
            .then(|| unsafe { self.inline_lookup_unchecked() })
    }

    #[inline(always)]
    pub fn cases(&self) -> Option<&AccessCases> {
        (!self.has_inline_lookup()).then_some(&self.cases_or_lookup)
    }

    pub fn has_run(&self) -> bool {
        !self.shape().is_none() || self.cases_or_lookup.has_run()
    }

    fn clear_inline_case(&self) {
        if self.has_inline_lookup() {
            self.cases_or_lookup.set_bits(1);
        }
        self.shape.set(ShapeId::NONE);
        self.byte_offset.set(0);
    }

    /// For a cache that has only cases of own slots.
    pub(super) fn list(&self) -> &AccessCases {
        debug_assert!(!self.has_inline_lookup());
        &self.cases_or_lookup
    }

    /// The barrier of the owner follows.
    pub(super) fn set_inline_own(&self, case: &AccessCase) {
        self.shape.set(case.shape());
        self.byte_offset.set(case.byte_offset);
    }

    /// Makes room for the list.
    fn ensure_cases(&self, vm: &Vm, owner: &CodeBlock) -> &AccessCases {
        if let Some(lookup) = self.inline_lookup() {
            let case = AccessCase::new(AccessKind::LoadConstant, self.shape()).with_lookup(lookup);
            self.clear_inline_case();
            if lookup.is_valid() {
                let overflow = AccessKind::Generic;
                self.cases_or_lookup.add(vm, owner, case, None, overflow);
            }
        }
        &self.cases_or_lookup
    }

    fn add(&self, vm: &Vm, owner: &CodeBlock, case: AccessCase) {
        if self.inline_lookup().is_some_and(|it| !it.is_valid()) {
            self.clear_inline_case();
        }
        if self.shape().is_none() {
            let is_only = self.cases_or_lookup.first().is_none();
            let kind = case.kind();
            if kind == AccessKind::LoadOwn || (kind == AccessKind::LoadConstant && is_only) {
                self.shape.set(case.shape());
                self.byte_offset.set(case.byte_offset);
                if kind == AccessKind::LoadConstant {
                    self.cases_or_lookup.set_bits(case.lookup);
                }
                return owner.write_barrier_for_cell();
            }
        }
        let cases = self.ensure_cases(vm, owner);
        cases.add(vm, owner, case, None, AccessKind::Megamorphic);
    }

    /// For when the inline case does not apply.
    #[inline]
    pub fn get_slow(
        &self,
        cx: &mut Cx,
        owner: &CodeBlock,
        base: Value,
        name: PropertyName<'_>,
    ) -> JsResult<Value> {
        let shape = shape_for_lookup(base);
        if let Some(lookup) = self.inline_lookup() {
            if shape == self.shape()
                && let Some(value) = lookup.constant()
            {
                return Ok(value);
            }
        } else if let Some(case) = self.cases_or_lookup.find(shape, 0) {
            if let Some(result) = execute(cx, case, base, base, name) {
                return result;
            }
        } else if self.cases_or_lookup.is_megamorphic() {
            return crate::caches::get_megamorphic(cx, base, name);
        }
        let mut slot = PropertySlot::new(base, InternalMethod::Get);
        let found = base.get_property_slot(cx, name, &mut slot)?;
        self.update(cx, owner, base, name, &slot, Reach::Chain);
        if found {
            slot.value(cx, name)
        } else {
            Ok(Value::UNDEFINED)
        }
    }

    pub(crate) fn update(
        &self,
        cx: &Cx,
        owner: &CodeBlock,
        base: Value,
        name: PropertyName<'_>,
        slot: &PropertySlot,
        reach: Reach,
    ) {
        let is_cheap = self.shape().is_none()
            && slot.is_value()
            && slot.base().is_some_and(|it| base == it.into());
        if !is_cheap && self.cases().is_some_and(|cases| !cases.should_cache()) {
            return;
        }
        match plan(cx, base, name, slot, reach) {
            Plan::Cache(case) => self.add(cx, owner, case),
            Plan::Retry => {}
            Plan::Fail => self
                .ensure_cases(cx, owner)
                .did_fail_to_cache(cx, owner, Key::Fixed),
        }
    }

    pub(crate) fn reset(&self) {
        self.clear_inline_case();
    }
}

// SAFETY: integers.
unsafe impl Entry for LoadCache {
    #[inline]
    fn visit(&self, visitor: &mut Visitor) {
        match self.cases() {
            Some(cases) => cases.visit(visitor),
            // SAFETY: a cell that has been kept alive so far.
            None => unsafe { visitor.append_hidden_word(self.cases_or_lookup.bits() as u64) },
        }
    }

    #[inline]
    fn reconcile(&self, owner: &CodeBlock, survivors: &Survivors) {
        if !is_live(self.shape(), survivors) {
            self.clear_inline_case();
        }
        if let Some(cases) = self.cases() {
            cases.reconcile(owner, survivors);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reach {
    Own,
    Chain,
}

pub(super) enum Plan {
    Cache(AccessCase),
    /// Something has been rearranged so that the next attempt can succeed.
    Retry,
    Fail,
}

/// Whether the shape of the object says where its properties are. `needs_absence`: and which it does not have.
pub(super) fn prepare(vm: &Vm, object: &JsObject, needs_absence: bool) -> Result<(), Plan> {
    let shape = object.shape();
    if shape.is_uncacheable_dictionary() || (needs_absence && shape.is_dictionary()) {
        if shape.has(ShapeFlags::HAS_BEEN_FLATTENED) {
            return Err(Plan::Fail);
        }
        object.flatten_dictionary(vm);
        return Err(Plan::Retry);
    }
    if !shape.property_accesses_are_cacheable()
        || (needs_absence && (!shape.absence_is_cacheable() || object.overrides_get_prototype()))
    {
        return Err(Plan::Fail);
    }
    Ok(())
}

/// What the rest of the chain says, for a receiver that does not have the property.
pub(super) fn lookup_beyond(
    cx: &Cx,
    base: Value,
    name: PropertyName<'_>,
    holder: Option<Gc<JsObject>>,
) -> Result<Gc<PrototypeLookup>, Plan> {
    let first = match base.as_object() {
        Some(object) => {
            prepare(cx, &object, true)?;
            if CellType::TYPED_ARRAYS.contains(object.cell_type())
                && !name.is_symbol()
                && bun_engine_number::canonical_numeric_index(name.text()).is_some()
            {
                return Err(Plan::Fail);
            }
            object.prototype_object_direct()
        }
        None if base.is_undefined_or_null() => return Err(Plan::Fail),
        None => base.synthesize_prototype(cx).ok(),
    };
    match (first, holder) {
        (Some(first), Some(holder)) => PrototypeLookup::for_hit(cx, first, holder, name),
        (None, Some(_)) => None,
        (first, None) => PrototypeLookup::for_miss(cx, first, name),
    }
    .ok_or(Plan::Fail)
}

fn plan(cx: &Cx, base: Value, name: PropertyName<'_>, slot: &PropertySlot, reach: Reach) -> Plan {
    plan_or_reason(cx, base, name, slot, reach).map_or_else(|plan| plan, Plan::Cache)
}

fn plan_or_reason(
    cx: &Cx,
    base: Value,
    name: PropertyName<'_>,
    slot: &PropertySlot,
    reach: Reach,
) -> Result<AccessCase, Plan> {
    if name.as_index().is_some() || slot.is_tainted_by_opaque_object() {
        return Err(Plan::Fail);
    }
    if let (Some((environment, offset)), Some(namespace)) = (slot.module_binding(), slot.base())
        && base == namespace.into()
    {
        let mut case = AccessCase::new(AccessKind::LoadModuleBinding, namespace.shape_id());
        case.pointer = namespace.address();
        // SAFETY: from the namespace, which keeps the environment alive.
        case.second_pointer =
            core::ptr::from_ref(unsafe { environment.slot_unchecked(offset) }).expose_provenance();
        return Ok(case);
    }
    if let Some(proxy) = base.downcast::<GlobalProxy>() {
        return plan_through_global_proxy(cx, proxy, name, slot);
    }
    if !slot.is_cacheable() || !slot.watchpoints().is_null() {
        return Err(Plan::Fail);
    }
    let own = base.as_object().filter(|&it| slot.base() == Some(it));
    let lookup = match (own, base.as_object()) {
        (Some(object), _) => {
            prepare(cx, &object, !slot.cached_offset().is_valid())?;
            None
        }
        (None, Some(object)) if reach == Reach::Own => {
            prepare(cx, &object, true)?;
            Some(PrototypeLookup::of_empty_chain(cx))
        }
        _ => Some(lookup_beyond(cx, base, name, slot.base())?),
    };
    let shape = shape_for_lookup(base);
    let with_lookup = |kind| match lookup {
        Some(lookup) => AccessCase::new(kind, shape).with_lookup(lookup),
        None => AccessCase::new(kind, shape).with_offset(slot.cached_offset()),
    };
    if let Some(getter) = slot.custom_getter() {
        let is_accessor = slot.attributes().contains(Attributes::CUSTOM_ACCESSOR);
        let this = if is_accessor {
            Some(base)
        } else {
            slot.base().map(Into::into)
        };
        if let Some(class) = slot.required_class()
            && !this
                .and_then(Value::as_cell)
                .is_some_and(|it| it.inherits(class))
        {
            return Err(Plan::Fail);
        }
        let mut case = with_lookup(if is_accessor {
            AccessKind::CallCustomAccessorGetter
        } else {
            AccessKind::CallCustomValueGetter
        });
        case.pointer = getter as usize;
        return Ok(case);
    }
    Ok(with_lookup(match lookup {
        _ if slot.is_custom() => return Err(Plan::Fail),
        None if slot.is_accessor() => AccessKind::CallOwnGetter,
        None => AccessKind::LoadOwn,
        Some(lookup) if slot.is_accessor() => kind_for_inherited_getter(base, &lookup),
        Some(lookup) if lookup.constant().is_some() => AccessKind::LoadConstant,
        Some(_) => AccessKind::LoadInherited,
    }))
}

fn kind_for_inherited_getter(base: Value, lookup: &PrototypeLookup) -> AccessKind {
    let getter = lookup
        .constant()
        .and_then(|it| it.cast::<GetterSetter>().getter());
    let function = getter.and_then(|it| it.downcast::<JsFunction>());
    let array = base.downcast::<JsTypedArray>();
    match function.map(|it| it.intrinsic()) {
        _ if array.is_none_or(|it| it.is_resizable_or_growable()) => {
            AccessKind::CallInheritedGetter
        }
        Some(Intrinsic::TypedArrayLength) => AccessKind::TypedArrayLength,
        Some(Intrinsic::TypedArrayByteLength) => AccessKind::TypedArrayByteLength,
        Some(Intrinsic::TypedArrayByteOffset) => AccessKind::TypedArrayByteOffset,
        _ => AccessKind::CallInheritedGetter,
    }
}

fn plan_through_global_proxy(
    cx: &Cx,
    proxy: Gc<GlobalProxy>,
    name: PropertyName<'_>,
    slot: &PropertySlot,
) -> Result<AccessCase, Plan> {
    let target = proxy.target();
    if !slot.is_value() || slot.base() != Some(target.upcast()) {
        return Err(Plan::Fail);
    }
    prepare(cx, &target, false)?;
    let mut case = if slot.is_cacheable() {
        AccessCase::new(AccessKind::LoadOwnThroughGlobalProxy, proxy.shape_id())
            .with_offset(slot.cached_offset())
    } else {
        let binding = target.variables().find(name).ok_or(Plan::Fail)?;
        let mut case = AccessCase::new(AccessKind::LoadGlobalBinding, proxy.shape_id());
        case.pointer = core::ptr::from_ref(binding).expose_provenance();
        case
    };
    case.new_shape = target.shape_id();
    Ok(case)
}

impl AccessCase {
    /// If the case is one that calls nothing and it holds. `base` has its shape.
    ///
    /// # Safety
    /// The case is in a list for loads.
    #[inline(always)]
    pub unsafe fn try_load(&self, base: Gc<JsObject>) -> Option<Value> {
        // SAFETY: the shape says that the slot is there, and these kinds have a lookup.
        unsafe {
            match self.kind() {
                AccessKind::LoadOwn => slot_at(&base, self.byte_offset).get(),
                AccessKind::LoadConstant => self.lookup_unchecked().constant(),
                AccessKind::LoadInherited => self.lookup_unchecked().stored_value(),
                kind @ (AccessKind::TypedArrayLength
                | AccessKind::TypedArrayByteLength
                | AccessKind::TypedArrayByteOffset)
                    if self.lookup_unchecked().is_valid() =>
                {
                    let array = base.cast_unchecked::<JsTypedArray>();
                    Some(Value::from(match kind {
                        AccessKind::TypedArrayLength => array.known_length(),
                        AccessKind::TypedArrayByteLength => {
                            array.known_length() << array.size_shift()
                        }
                        _ => array.declared_byte_offset(),
                    }))
                }
                _ => None,
            }
        }
    }
}

/// `None` if the case no longer holds. `base` has its shape.
#[inline]
fn execute(
    cx: &mut Cx,
    case: &AccessCase,
    base: Value,
    receiver: Value,
    name: PropertyName<'_>,
) -> Option<JsResult<Value>> {
    // SAFETY: only the kinds that were made for an object look at it, and each has what its kind says.
    unsafe {
        let object = Gc::<JsObject>::from_address(base.to_bits() as usize);
        let load = |address| &*core::ptr::with_exposed_provenance::<Field<Option<Value>>>(address);
        match case.kind() {
            AccessKind::CallOwnGetter | AccessKind::CallInheritedGetter => {
                let accessor = match case.lookup() {
                    Some(lookup) => lookup.stored_value()?,
                    None => slot_at(&object, case.byte_offset).get()?,
                };
                Some(accessor.cast::<GetterSetter>().call_getter(cx, receiver))
            }
            kind @ (AccessKind::CallCustomAccessorGetter | AccessKind::CallCustomValueGetter) => {
                let holder = match case.lookup() {
                    Some(lookup) => lookup.holder()?,
                    None => object,
                };
                let this = if kind == AccessKind::CallCustomValueGetter {
                    holder.into()
                } else if receiver == base {
                    receiver
                } else {
                    return None;
                };
                let getter = transmute::<usize, CustomGetter>(case.pointer);
                let cx = cx.in_realm_of(&holder);
                let result = getter(cx, this, name);
                Some(cx.check_exception().map(|()| result.unwrap_or_default()))
            }
            AccessKind::LoadModuleBinding if case.pointer == object.address() => {
                load(case.second_pointer).get().map(Ok)
            }
            kind @ (AccessKind::LoadGlobalBinding | AccessKind::LoadOwnThroughGlobalProxy) => {
                let target = object.cast_unchecked::<GlobalProxy>().target();
                if target.shape_id() != case.new_shape {
                    None
                } else if kind == AccessKind::LoadGlobalBinding {
                    load(case.pointer).get().map(Ok)
                } else {
                    slot_at(&target, case.byte_offset).get().map(Ok)
                }
            }
            AccessKind::TypedArrayLength
            | AccessKind::TypedArrayByteLength
            | AccessKind::TypedArrayByteOffset
                if receiver != base =>
            {
                None
            }
            _ => case.try_load(object).map(Ok),
        }
    }
}

/// `base[name]` with `receiver` for `this`, at a site that has only a list.
#[inline]
pub fn get_by_key(
    cx: &mut Cx,
    owner: &CodeBlock,
    cases: &AccessCases,
    base: Value,
    receiver: Value,
    name: PropertyName<'_>,
    key: Key,
) -> JsResult<Value> {
    let variable_key = (key == Key::Variable).then_some(name);
    let key_address = variable_key.map_or(0, PropertyName::address);
    if let Some(case) = cases.find(shape_for_lookup(base), key_address) {
        if let Some(result) = execute(cx, case, base, receiver, name) {
            return result;
        }
    } else if cases.is_megamorphic() && receiver == base {
        return crate::caches::get_megamorphic(cx, base, name);
    }
    let mut slot = PropertySlot::new(receiver, InternalMethod::Get);
    let found = base.get_property_slot(cx, name, &mut slot)?;
    if cases.should_cache() {
        match plan(cx, base, name, &slot, Reach::Chain) {
            Plan::Cache(case) => cases.add(cx, owner, case, variable_key, AccessKind::Megamorphic),
            Plan::Retry => {}
            Plan::Fail => cases.did_fail_to_cache(cx, owner, key),
        }
    }
    if found {
        slot.value(cx, name)
    } else {
        Ok(Value::UNDEFINED)
    }
}

/// An own property, or `None`.
pub fn get_own_by_id(
    cx: &mut Cx,
    owner: &CodeBlock,
    cache: &LoadCache,
    base: Gc<JsObject>,
    name: PropertyName<'_>,
) -> JsResult<Option<Value>> {
    let value = base.into();
    if let Some(case) = cache.cases().and_then(|it| it.find(base.shape_id(), 0))
        && case.kind() != AccessKind::LoadConstant
        && let Some(result) = execute(cx, case, value, value, name)
    {
        return result.map(Some);
    }
    let mut slot = PropertySlot::new(value, InternalMethod::GetOwnProperty);
    if !base.get_own_property_slot(cx, name, &mut slot)? {
        return Ok(None);
    }
    if !cache.cases().is_some_and(AccessCases::is_megamorphic) {
        cache.update(cx, owner, value, name, &slot, Reach::Own);
    }
    slot.value(cx, name).map(Some)
}
