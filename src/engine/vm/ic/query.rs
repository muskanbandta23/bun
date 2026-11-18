use bun_engine_text::PropertyName;

use super::load::{Plan, Reach, lookup_beyond, prepare};
use super::{AccessCase, AccessCases, AccessKind, Key, PrototypeLookup, slot_at};
use crate::{
    CodeBlock, Cx, DeleteKind, DeletePropertySlot, Gc, InternalMethod, JsObject, JsResult,
    PropertySlot, ProxyObject, Shape, Value, errors,
};

fn apply(
    cx: &Cx,
    owner: &CodeBlock,
    cases: &AccessCases,
    plan: Result<AccessCase, Plan>,
    variable_key: Option<PropertyName<'_>>,
    overflow: AccessKind,
) {
    match plan {
        Ok(case) => cases.add(cx, owner, case, variable_key, overflow),
        Err(Plan::Fail) if variable_key.is_some() => {
            cases.did_fail_to_cache(cx, owner, Key::Variable);
        }
        Err(Plan::Fail) => cases.did_fail_to_cache(cx, owner, Key::Fixed),
        Err(_) => {}
    }
}

fn plan_has(
    cx: &Cx,
    object: Gc<JsObject>,
    name: PropertyName<'_>,
    slot: &PropertySlot,
    reach: Reach,
) -> Result<AccessCase, Plan> {
    if object.is::<ProxyObject>() && reach == Reach::Chain {
        return Ok(AccessCase::new(AccessKind::ProxyHas, object.shape_id()));
    }
    if !slot.is_cacheable() || slot.is_tainted_by_opaque_object() || name.as_index().is_some() {
        return Err(Plan::Fail);
    }
    if slot.base() == Some(object) {
        prepare(cx, &object, !slot.cached_offset().is_valid())?;
        return Ok(AccessCase::new(AccessKind::HasOwn, object.shape_id()));
    }
    let lookup = if reach == Reach::Own {
        prepare(cx, &object, true)?;
        PrototypeLookup::of_empty_chain(cx)
    } else {
        lookup_beyond(cx, object.into(), name, slot.base())?
    };
    let kind = if slot.is_unset() {
        AccessKind::HasNot
    } else {
        AccessKind::HasInherited
    };
    Ok(AccessCase::new(kind, object.shape_id()).with_lookup(lookup))
}

impl AccessCase {
    /// If the case is one that calls nothing and it holds.
    #[inline(always)]
    pub fn try_has(&self) -> Option<bool> {
        match self.kind() {
            AccessKind::HasOwn => Some(true),
            AccessKind::HasInherited => self.is_valid().then_some(true),
            AccessKind::HasNot => self.is_valid().then_some(false),
            _ => None,
        }
    }
}

/// HasProperty or, for a private name, whether the object itself has it.
pub fn has_by_key(
    cx: &mut Cx,
    owner: &CodeBlock,
    cases: &AccessCases,
    object: Gc<JsObject>,
    name: PropertyName<'_>,
    key: Key,
) -> JsResult<bool> {
    let variable_key = (key == Key::Variable).then_some(name);
    let key_address = variable_key.map_or(0, PropertyName::address);
    if let Some(case) = cases.find(object.shape_id(), key_address) {
        if let Some(answer) = case.try_has() {
            return Ok(answer);
        }
        if case.kind() == AccessKind::ProxyHas {
            return object.has_property(cx, name);
        }
    } else if cases.is_megamorphic() {
        return crate::caches::has_megamorphic(cx, object, name);
    }
    let mut slot = PropertySlot::new(object.into(), InternalMethod::HasProperty);
    let (found, reach, overflow) = if name.is_private() {
        let found = object.get_own_non_index_property_slot(cx, &object.shape(), name, &mut slot)?;
        (found, Reach::Own, AccessKind::Generic)
    } else {
        let found = object.get_property_slot(cx, name, &mut slot)?;
        (found, Reach::Chain, AccessKind::Megamorphic)
    };
    if cases.should_cache() {
        let plan = plan_has(cx, object, name, &slot, reach);
        apply(cx, owner, cases, plan, variable_key, overflow);
    }
    Ok(found)
}

fn plan_delete(
    cx: &Cx,
    object: Gc<JsObject>,
    old: Gc<Shape>,
    name: PropertyName<'_>,
    slot: &DeletePropertySlot,
) -> Result<AccessCase, Plan> {
    if old.is_dictionary()
        || !old.property_accesses_are_cacheable()
        || old.has_non_reified_static_properties()
    {
        return Err(Plan::Fail);
    }
    let kind = match slot.cacheable_kind() {
        DeleteKind::Hit
            if !old.may_be_prototype()
                && old.existing_remove_transition(name) == Some(object.shape()) =>
        {
            let offset = slot.cached_offset();
            old.did_cache_property_replacement(cx, offset);
            let mut case = AccessCase::new(AccessKind::Delete, old.id()).with_offset(offset);
            case.new_shape = object.shape_id();
            return Ok(case);
        }
        DeleteKind::Miss if old.absence_is_cacheable() => AccessKind::DeleteMissing,
        DeleteKind::NonConfigurable => AccessKind::DeleteNonConfigurable,
        _ => return Err(Plan::Fail),
    };
    if object.shape() != old {
        return Err(Plan::Retry);
    }
    Ok(AccessCase::new(kind, old.id()))
}

/// The `delete` operator.
pub fn delete_by_key(
    cx: &mut Cx,
    owner: &CodeBlock,
    cases: &AccessCases,
    object: Gc<JsObject>,
    name: PropertyName<'_>,
    key: Key,
    is_strict: bool,
) -> JsResult<bool> {
    let variable_key = (key == Key::Variable).then_some(name);
    let key_address = variable_key.map_or(0, PropertyName::address);
    let cached = cases.find(object.shape_id(), key_address);
    let did_delete = match cached.filter(|_| !cx.deletes_unconfigurable_properties()) {
        Some(case) => {
            if case.kind() == AccessKind::Delete {
                // SAFETY: the shapes have the same slots, and nothing is no cell.
                unsafe {
                    object.set_shape_id_directly(case.new_shape);
                    slot_at(&object, case.byte_offset).set_without_barrier(None);
                }
                object.write_barrier_for_cell();
            }
            case.kind() != AccessKind::DeleteNonConfigurable
        }
        None => {
            let (old, mut slot) = (object.shape(), DeletePropertySlot::default());
            let did_delete = object.delete_property(cx, name, &mut slot)?;
            if cases.should_cache() {
                let plan = plan_delete(cx, object, old, name, &slot);
                apply(cx, owner, cases, plan, variable_key, AccessKind::Generic);
            }
            did_delete
        }
    };
    if !did_delete && is_strict {
        return Err(cx.throw_type_error(errors::UNABLE_TO_DELETE_PROPERTY));
    }
    Ok(did_delete)
}

/// Whether `prototype` is on the prototype chain of `value`.
pub fn instance_of(
    cx: &mut Cx,
    owner: &CodeBlock,
    cases: &AccessCases,
    value: Value,
    prototype: Value,
) -> JsResult<bool> {
    let (Some(object), Some(wanted)) = (value.as_object(), prototype.as_object()) else {
        return JsObject::default_has_instance(cx, value, prototype);
    };
    let shape = object.shape();
    let matches = |it: &&AccessCase| it.shape() == shape.id() && it.pointer == wanted.address();
    if let Some(answer) = cases
        .iter()
        .find(matches)
        .and_then(|it| it.lookup()?.constant())
    {
        return Ok(answer.is_true());
    }
    let answer = JsObject::default_has_instance(cx, value, prototype)?;
    if cases.should_cache() && object.shape() == shape {
        let lookup = match shape.prototype_object() {
            _ if object.overrides_get_prototype() || shape.is_dictionary() => None,
            Some(first) => PrototypeLookup::for_instanceof(cx, first, wanted, answer),
            None => Some(PrototypeLookup::of_empty_chain(cx)),
        };
        let plan = lookup.ok_or(Plan::Fail).map(|lookup| {
            let mut case = AccessCase::new(AccessKind::Instanceof, shape.id()).with_lookup(lookup);
            case.pointer = wanted.address();
            case
        });
        apply(cx, owner, cases, plan, None, AccessKind::Generic);
    }
    Ok(answer)
}
