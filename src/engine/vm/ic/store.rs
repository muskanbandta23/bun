use core::cell::Cell;
use core::mem::{offset_of, transmute};

use bun_engine_text::PropertyName;

use super::access_case::visit_transition;
use super::load::{Plan, lookup_beyond, prepare};
use super::{AccessCase, AccessCases, AccessKind, Key, PrototypeLookup, is_live, slot_at};
use crate::code::metadata::Entry;
use crate::heap::Survivors;
use crate::{
    Attributes, CodeBlock, CustomSetter, Cx, Gc, GetterSetter, JsObject, JsResult, ProxyObject,
    PutContext,
    PutKind, PutPropertySlot, Shape, ShapeId, Value, Visitor, Vm,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StoreMode {
    Set {
        is_strict: bool,
    },
    /// CreateDataProperty
    Define {
        is_strict: bool,
    },
}

/// One case inline, and the others in a list.
#[repr(C)]
pub struct StoreCache {
    shape: Cell<ShapeId>,
    /// None for a property that exists.
    new_shape: Cell<ShapeId>,
    byte_offset: Cell<i32>,
    /// For a new property: a `PrototypeLookup` that has to be valid. Then there is no list.
    cases_or_lookup: AccessCases,
}

impl StoreCache {
    pub const OFFSET_OF_SHAPE: usize = offset_of!(StoreCache, shape);
    pub const OFFSET_OF_NEW_SHAPE: usize = offset_of!(StoreCache, new_shape);
    pub const OFFSET_OF_BYTE_OFFSET: usize = offset_of!(StoreCache, byte_offset);
    pub const OFFSET_OF_CASES_OR_LOOKUP: usize = offset_of!(StoreCache, cases_or_lookup);

    #[inline(always)]
    pub fn shape(&self) -> ShapeId {
        self.shape.get()
    }

    #[inline(always)]
    pub fn new_shape(&self) -> ShapeId {
        self.new_shape.get()
    }

    #[inline(always)]
    pub fn byte_offset(&self) -> i32 {
        self.byte_offset.get()
    }

    /// # Safety
    /// There is a new shape.
    #[inline(always)]
    pub unsafe fn inline_lookup_unchecked(&self) -> Gc<PrototypeLookup> {
        // SAFETY: visited with the code block.
        unsafe { Gc::from_address(self.cases_or_lookup.bits()) }
    }

    pub fn inline_lookup(&self) -> Option<Gc<PrototypeLookup>> {
        // SAFETY: checked.
        (!self.new_shape().is_none()).then(|| unsafe { self.inline_lookup_unchecked() })
    }

    #[inline(always)]
    pub fn cases(&self) -> Option<&AccessCases> {
        self.new_shape().is_none().then_some(&self.cases_or_lookup)
    }

    pub fn has_run(&self) -> bool {
        !self.shape().is_none() || self.cases_or_lookup.has_run()
    }

    fn clear_inline_case(&self) {
        if !self.new_shape().is_none() {
            self.cases_or_lookup.set_bits(1);
        }
        self.shape.set(ShapeId::NONE);
        self.new_shape.set(ShapeId::NONE);
    }

    pub(crate) fn reset(&self) {
        self.clear_inline_case();
    }

    /// `inline_key` is the name that the inline case is for, where that varies.
    pub(super) fn ensure_cases(
        &self,
        vm: &Vm,
        owner: &CodeBlock,
        inline_key: Option<PropertyName<'_>>,
    ) -> &AccessCases {
        if let Some(lookup) = self.inline_lookup() {
            let mut case =
                AccessCase::new(AccessKind::Transition, self.shape()).with_lookup(lookup);
            (case.new_shape, case.byte_offset) = (self.new_shape(), self.byte_offset());
            self.clear_inline_case();
            if lookup.is_valid() {
                let overflow = AccessKind::Generic;
                (self.cases_or_lookup).add(vm, owner, case, inline_key, overflow);
            }
        }
        &self.cases_or_lookup
    }

    /// Gives the case back if it has to go in the list.
    pub(super) fn try_set_inline(
        &self,
        owner: &CodeBlock,
        case: AccessCase,
    ) -> Result<(), AccessCase> {
        if self.inline_lookup().is_some_and(|it| !it.is_valid()) {
            self.clear_inline_case();
        }
        let kind = case.kind();
        let fits = match kind {
            AccessKind::Replace | AccessKind::HasOwn => true,
            AccessKind::Transition => self.cases_or_lookup.first().is_none(),
            _ => false,
        };
        if !self.shape().is_none() || !fits {
            return Err(case);
        }
        self.shape.set(case.shape());
        self.new_shape.set(case.new_shape);
        self.byte_offset.set(case.byte_offset);
        if kind == AccessKind::Transition {
            self.cases_or_lookup.set_bits(case.lookup);
            owner.caches.has_transitions.set(true);
        }
        owner.write_barrier_for_cell();
        Ok(())
    }

    fn add(&self, vm: &Vm, owner: &CodeBlock, case: AccessCase) {
        if let Err(case) = self.try_set_inline(owner, case) {
            let cases = self.ensure_cases(vm, owner, None);
            cases.add(vm, owner, case, None, AccessKind::Megamorphic);
        }
    }

    /// For when the inline case does not apply.
    #[inline]
    pub fn put_slow(
        &self,
        cx: &mut Cx,
        owner: &CodeBlock,
        base: Value,
        name: PropertyName<'_>,
        value: Value,
        mode: StoreMode,
    ) -> JsResult<()> {
        let Some(object) = base.as_object() else {
            return put_uncached(cx, base, name, value, mode, PutContext::PutById).map(|_| ());
        };
        if let Some(cases) = self.cases()
            && let Some(result) = try_cases(cx, cases, object, name, 0, value, mode)
        {
            return result;
        }
        let old = object.shape();
        let context = if owner.body().kind() == bun_engine_bytecode::CodeKind::Eval {
            PutContext::PutByIdInEval
        } else {
            PutContext::PutById
        };
        let slot = put_uncached(cx, base, name, value, mode, context)?;
        let needs_setter_miss = matches!(mode, StoreMode::Set { .. });
        self.update(cx, owner, object, old, name, &slot, needs_setter_miss);
        Ok(())
    }

    /// `old` is the shape before the store that `slot` describes.
    pub(crate) fn update(
        &self,
        cx: &Cx,
        owner: &CodeBlock,
        object: Gc<JsObject>,
        old: Gc<Shape>,
        name: PropertyName<'_>,
        slot: &PutPropertySlot,
        needs_setter_miss: bool,
    ) {
        if self.cases().is_some_and(|cases| !cases.should_cache()) {
            return;
        }
        match plan(cx, object, old, name, slot, needs_setter_miss) {
            Ok(case) => self.add(cx, owner, case),
            Err(Plan::Fail) => {
                self.ensure_cases(cx, owner, None)
                    .did_fail_to_cache(cx, owner, Key::Fixed)
            }
            Err(_) => {}
        }
    }
}

// SAFETY: integers.
unsafe impl Entry for StoreCache {
    #[inline]
    fn visit(&self, visitor: &mut Visitor) {
        match self.cases() {
            Some(cases) => cases.visit(visitor),
            // SAFETY: a cell that has been kept alive so far.
            None => unsafe { visitor.append_word(self.cases_or_lookup.bits() as u64) },
        }
    }

    #[inline]
    fn visit_transitions(&self, visitor: &mut Visitor) {
        match self.cases() {
            Some(cases) => cases.visit_transitions(visitor),
            None => visit_transition(self.shape(), self.new_shape(), visitor),
        }
    }

    #[inline]
    fn reconcile(&self, owner: &CodeBlock, survivors: &Survivors) {
        if !is_live(self.shape(), survivors) || !is_live(self.new_shape(), survivors) {
            self.clear_inline_case();
        }
        if let Some(cases) = self.cases() {
            cases.reconcile(owner, survivors);
        }
    }
}

#[inline(always)]
fn put_uncached(
    cx: &mut Cx,
    base: Value,
    name: PropertyName<'_>,
    value: Value,
    mode: StoreMode,
    context: PutContext,
) -> JsResult<PutPropertySlot> {
    let (StoreMode::Set { is_strict } | StoreMode::Define { is_strict }) = mode;
    let mut slot = PutPropertySlot::with_context(base, is_strict, context);
    if let StoreMode::Set { .. } = mode {
        base.put(cx, name, value, &mut slot)?;
    } else {
        let object = base.cast::<JsObject>();
        object.create_data_property_with_slot(cx, name, value, &mut slot)?;
    }
    Ok(slot)
}

pub(super) fn plan(
    cx: &Cx,
    object: Gc<JsObject>,
    old: Gc<Shape>,
    name: PropertyName<'_>,
    slot: &PutPropertySlot,
    needs_setter_miss: bool,
) -> Result<AccessCase, Plan> {
    if object.is::<ProxyObject>() && needs_setter_miss && !name.is_private() {
        return Ok(AccessCase::new(AccessKind::ProxySet, object.shape_id()));
    }
    let (kind, offset) = (slot.cacheable_kind(), slot.cached_offset());
    let is_own = slot.base() == Some(object);
    if kind == PutKind::NewProperty && is_own {
        let new = object.shape();
        if old.is_dictionary() || new.is_dictionary() {
            return Err(Plan::Retry);
        }
        let attributes = new.transition_attributes();
        if old.may_be_prototype()
            || !old.property_accesses_are_cacheable()
            || old.has_non_reified_static_properties()
            || old.instance_indexing_mode().is_copy_on_write()
            || old.existing_add_transition(name, attributes) != Some(new)
            || object.overrides_get_prototype()
        {
            return Err(Plan::Fail);
        }
        let first = new.prototype_object().filter(|_| needs_setter_miss);
        let lookup = PrototypeLookup::for_setter_miss(cx, first, name).ok_or(Plan::Fail)?;
        let kind = if old.out_of_line_capacity() == new.out_of_line_capacity() {
            AccessKind::Transition
        } else {
            AccessKind::TransitionAndGrow
        };
        let mut case = AccessCase::new(kind, old.id())
            .with_offset(offset)
            .with_lookup(lookup);
        case.new_shape = new.id();
        return Ok(case);
    }
    if kind == PutKind::Uncacheable || slot.is_tainted_by_opaque_object() {
        return Err(Plan::Fail);
    }
    // A setter may have changed anything.
    if object.shape() != old {
        return Err(Plan::Retry);
    }
    let holder = slot.base().ok_or(Plan::Fail)?;
    let (found, attributes) = match holder.shape().get(name) {
        Some(found) => found,
        None => {
            let (_, entry) = holder
                .shape()
                .find_static_property(name)
                .ok_or(Plan::Retry)?;
            (crate::PropertyOffset::INVALID, entry.attributes())
        }
    };
    let expected = match kind {
        PutKind::Setter => Attributes::ACCESSOR,
        PutKind::CustomAccessor => Attributes::CUSTOM_ACCESSOR,
        PutKind::CustomValue => Attributes::CUSTOM_VALUE,
        _ => Attributes::NONE,
    };
    if found != offset
        || attributes & (Attributes::ANY_ACCESSOR | Attributes::READ_ONLY) != expected
    {
        return Err(Plan::Retry);
    }
    let case = |kind| -> Result<AccessCase, Plan> {
        if is_own {
            prepare(cx, &object, !offset.is_valid())?;
            return Ok(AccessCase::new(kind, old.id()).with_offset(offset));
        }
        let lookup = lookup_beyond(cx, object.into(), name, Some(holder))?;
        Ok(AccessCase::new(kind, old.id()).with_lookup(lookup))
    };
    match kind {
        PutKind::ExistingProperty if is_own => {
            let case = case(AccessKind::Replace)?;
            old.did_cache_property_replacement(cx, offset);
            Ok(case)
        }
        PutKind::Setter if is_own => case(AccessKind::CallOwnSetter),
        PutKind::Setter => case(AccessKind::CallInheritedSetter),
        PutKind::CustomAccessor | PutKind::CustomValue => {
            let mut case = case(if kind == PutKind::CustomValue {
                AccessKind::CallCustomValueSetter
            } else {
                AccessKind::CallCustomAccessorSetter
            })?;
            case.pointer = slot.custom_setter().ok_or(Plan::Fail)? as usize;
            Ok(case)
        }
        _ => Err(Plan::Fail),
    }
}

impl AccessCase {
    /// If the case is one that calls nothing and it holds. `object` has its shape.
    ///
    /// # Safety
    /// The case is in a list for stores.
    #[inline(always)]
    pub unsafe fn try_store(&self, object: Gc<JsObject>, value: Value) -> bool {
        // SAFETY: the shapes say that the slot is there, and a transition has a lookup.
        unsafe {
            match self.kind() {
                AccessKind::Replace => {
                    slot_at(&object, self.byte_offset).set(&object, Some(value));
                }
                AccessKind::Transition if self.lookup_unchecked().is_valid() => {
                    object.add_cached_property(self.new_shape, self.byte_offset, value);
                }
                _ => return false,
            }
        }
        true
    }
}

impl JsObject {
    /// # Safety
    /// `new_shape` adds the slot to the current shape, and there is room for it.
    #[inline(always)]
    pub unsafe fn add_cached_property(&self, new_shape: ShapeId, byte_offset: i32, value: Value) {
        // SAFETY: the caller's.
        unsafe {
            slot_at(self, byte_offset).set_without_barrier(Some(value));
            self.set_shape_id_directly(new_shape);
        }
        self.write_barrier_for_cell();
    }
}

#[inline(always)]
fn try_cases(
    cx: &mut Cx,
    cases: &AccessCases,
    object: Gc<JsObject>,
    name: PropertyName<'_>,
    key: usize,
    value: Value,
    mode: StoreMode,
) -> Option<JsResult<()>> {
    if let Some(case) = cases.find(object.shape_id(), key) {
        let (StoreMode::Set { is_strict } | StoreMode::Define { is_strict }) = mode;
        return execute(cx, case, object, name, value, is_strict);
    }
    match mode {
        StoreMode::Set { is_strict } if cases.is_megamorphic() => Some(
            crate::caches::put_megamorphic(cx, object, name, value, is_strict),
        ),
        _ => None,
    }
}

/// `None` if the case no longer holds. `object` has its shape.
pub(super) fn execute(
    cx: &mut Cx,
    case: &AccessCase,
    object: Gc<JsObject>,
    name: PropertyName<'_>,
    value: Value,
    is_strict: bool,
) -> Option<JsResult<()>> {
    // SAFETY: each has what its kind says.
    unsafe {
        match case.kind() {
            AccessKind::TransitionAndGrow if case.is_valid() => {
                let (new, byte_offset) = (case.new_shape.decode(), case.byte_offset);
                let old_capacity = object.shape().out_of_line_capacity();
                let storage =
                    object.storage_with_more_slots(cx, old_capacity, new.out_of_line_capacity());
                object.nuke_shape_and_set_storage(storage, false);
                object.add_cached_property(new.id(), byte_offset, value);
                Some(Ok(()))
            }
            AccessKind::ProxySet => Some(object.set(cx, name, value, is_strict).map(|_| ())),
            AccessKind::CallOwnSetter | AccessKind::CallInheritedSetter => {
                let accessor = match case.lookup() {
                    Some(lookup) => lookup.stored_value()?,
                    None => slot_at(&object, case.byte_offset).get()?,
                };
                let accessor = accessor.cast::<GetterSetter>();
                let result = accessor.call_setter(cx, object.into(), value, is_strict);
                Some(result.map(|_| ()))
            }
            AccessKind::CallCustomAccessorSetter | AccessKind::CallCustomValueSetter => {
                let holder = match case.lookup() {
                    Some(lookup) => lookup.holder()?,
                    None => object,
                };
                let setter = transmute::<usize, CustomSetter>(case.pointer);
                let cx = cx.in_realm_of(&holder);
                setter(cx, object.into(), value, name);
                Some(cx.check_exception())
            }
            _ => case.try_store(object, value).then_some(Ok(())),
        }
    }
}

/// `base[name] = value`, where the name is no index and varies.
pub fn put_by_key(
    cx: &mut Cx,
    owner: &CodeBlock,
    cases: &AccessCases,
    base: Value,
    name: PropertyName<'_>,
    value: Value,
    mode: StoreMode,
) -> JsResult<()> {
    let Some(object) = base.as_object() else {
        return put_uncached(cx, base, name, value, mode, PutContext::Unknown).map(|_| ());
    };
    if let Some(result) = try_cases(cx, cases, object, name, name.address(), value, mode) {
        return result;
    }
    let old = object.shape();
    let slot = put_uncached(cx, base, name, value, mode, PutContext::Unknown)?;
    if cases.should_cache() {
        let (needs_setter_miss, overflow) = match mode {
            StoreMode::Set { .. } => (true, AccessKind::Megamorphic),
            StoreMode::Define { .. } => (false, AccessKind::Generic),
        };
        match plan(cx, object, old, name, &slot, needs_setter_miss) {
            Ok(case) => cases.add(cx, owner, case, Some(name), overflow),
            Err(Plan::Fail) => cases.did_fail_to_cache(cx, owner, Key::Variable),
            Err(_) => {}
        }
    }
    Ok(())
}
