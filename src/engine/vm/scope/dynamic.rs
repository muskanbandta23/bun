//! Names that only a search at run time finds. What holds a binding is an environment, the object that has the
//! variables of `eval` code, or the global object.

use bun_engine_bytecode::{SlotKind, Unresolved};
use bun_engine_text::{PropertyName, symbols};

use crate::{
    Attributes, CellType, Cx, Environment, Gc, JsCell, JsObject, JsResult, ModuleEnvironment,
    Value, errors,
};

enum Holder {
    Slots(Gc<Environment>),
    With(Gc<JsObject>),
    EvalVariables(Gc<JsObject>),
    Global,
}

impl Holder {
    fn of(cell: Gc<JsCell>) -> Holder {
        if let Some(environment) = cell.downcast::<Environment>() {
            return (environment.with_object()).map_or(Holder::Slots(environment), Holder::With);
        }
        match cell.cell_type() {
            CellType::GlobalObject => Holder::Global,
            _ => Holder::EvalVariables(cell.cast()),
        }
    }
}

/// HasBinding of an Object Environment Record whose `[[IsWithEnvironment]]` is true.
fn with_has_binding(cx: &mut Cx, object: &JsObject, name: PropertyName<'_>) -> JsResult<bool> {
    if !object.has_property(cx, name)? {
        return Ok(false);
    }
    match object.get(cx, symbols::unscopables)?.as_object() {
        Some(unscopables) => Ok(!unscopables.get(cx, name)?.to_boolean(cx)),
        None => Ok(true),
    }
}

impl Environment {
    /// What a read of the slot that finds it empty comes to.
    #[cold]
    #[inline(never)]
    pub fn read_empty_slot(&self, cx: &mut Cx, index: u32) -> JsResult<Value> {
        if let Some(module) = self.downcast::<ModuleEnvironment>()
            && let Some(value) = module.create_on_demand(cx, index)?
        {
            return Ok(value);
        }
        let template = self.template();
        let name = (template.scope().slot_at(index)).map(|slot| template.scope().name_of(slot));
        let name = name.as_ref().map_or(PropertyName::EMPTY, |it| it.name());
        Err(errors::throw_uninitialized_variable(cx, name))
    }

    /// The instruction `get_scoped_checked`.
    #[inline(always)]
    pub fn get_checked(&self, cx: &mut Cx, index: u32) -> JsResult<Value> {
        match self.get(index) {
            Some(value) => Ok(value),
            None => self.read_empty_slot(cx, index),
        }
    }

    fn get_named(&self, cx: &mut Cx, name: PropertyName<'_>) -> JsResult<Value> {
        let slot = *(self.template().scope().find(name)).expect("where it was resolved");
        if slot.kind == SlotKind::Import {
            let module = Gc::from(self).cast::<ModuleEnvironment>();
            return module.get_import(cx, slot.index);
        }
        self.get_checked(cx, slot.index)
    }

    fn put_named(
        &self,
        cx: &mut Cx,
        name: PropertyName<'_>,
        value: Value,
        is_strict: bool,
    ) -> JsResult<()> {
        let slot = *(self.template().scope().find(name)).expect("where it was resolved");
        if matches!(
            slot.kind,
            SlotKind::Let | SlotKind::Const | SlotKind::Variable
        ) {
            self.get_checked(cx, slot.index)?;
        }
        match slot.kind {
            SlotKind::Variable | SlotKind::Let | SlotKind::LazyFunction => {
                // SAFETY: the code that declares it knows that it is assigned to.
                unsafe { self.set(slot.index, Some(value)) };
                Ok(())
            }
            SlotKind::Callee if !is_strict => Ok(()),
            _ => Err(cx.throw_type_error(errors::READ_ONLY_PROPERTY_WRITE)),
        }
    }
}

impl Cx {
    /// The instruction `resolve_dynamic`.
    pub fn resolve_dynamic(
        &mut self,
        scope: Gc<JsCell>,
        name: PropertyName<'_>,
    ) -> JsResult<Gc<JsCell>> {
        let mut scope = scope;
        while let Some(environment) = scope.downcast::<Environment>() {
            if let Some(object) = environment.with_object() {
                if with_has_binding(self, &object, name)? {
                    return Ok(scope);
                }
            } else if environment.template().scope().find(name).is_some() {
                return Ok(scope);
            } else if let Some(variables) = environment.eval_variables()
                && variables.get_direct(name).is_some()
            {
                return Ok(variables.cell());
            }
            scope = environment.parent();
        }
        Ok(self.scope_extension_for(name)?.map_or(scope, Gc::cell))
    }

    /// The instruction `get_dynamic`.
    pub fn get_dynamic(
        &mut self,
        holder: Gc<JsCell>,
        name: PropertyName<'_>,
        unresolved: Unresolved,
        is_strict: bool,
    ) -> JsResult<Value> {
        let value = match Holder::of(holder) {
            Holder::Slots(environment) => return environment.get_named(self, name),
            Holder::Global => return self.get_global(name, unresolved),
            Holder::With(object) => object.get_if_has_property(self, name)?,
            Holder::EvalVariables(variables) => variables.get_direct(name),
        };
        match value {
            None if is_strict => Err(errors::throw_undefined_variable(self, name)),
            _ => Ok(value.unwrap_or_default()),
        }
    }

    /// The instruction `put_dynamic`.
    pub fn put_dynamic(
        &mut self,
        holder: Gc<JsCell>,
        name: PropertyName<'_>,
        value: Value,
        is_strict: bool,
    ) -> JsResult<()> {
        match Holder::of(holder) {
            Holder::Slots(environment) => environment.put_named(self, name, value, is_strict),
            Holder::Global => self.put_global(name, value, is_strict),
            Holder::With(object) => self.put_to_binding_object(&object, name, value, is_strict),
            Holder::EvalVariables(variables) => {
                if is_strict && variables.get_direct(name).is_none() {
                    return Err(errors::throw_undefined_variable(self, name));
                }
                variables.put_direct(self, name, value, Attributes::NONE);
                Ok(())
            }
        }
    }

    /// SetMutableBinding of an Object Environment Record.
    pub(crate) fn put_to_binding_object(
        &mut self,
        object: &JsObject,
        name: PropertyName<'_>,
        value: Value,
        is_strict: bool,
    ) -> JsResult<()> {
        if !object.has_property(self, name)? && is_strict {
            return Err(errors::throw_undefined_variable(self, name));
        }
        object.set(self, name, value, is_strict).map(|_| ())
    }

    /// The instruction `delete_dynamic`.
    pub fn delete_dynamic(&mut self, holder: Gc<JsCell>, name: PropertyName<'_>) -> JsResult<bool> {
        match Holder::of(holder) {
            Holder::Slots(_) => Ok(false),
            Holder::Global => self.delete_global(name),
            Holder::With(object) | Holder::EvalVariables(object) => object.delete(self, name),
        }
    }
}

/// The instruction `get_dynamic_this`.
pub fn dynamic_this(holder: Gc<JsCell>) -> Value {
    match Holder::of(holder) {
        Holder::With(object) => object.into(),
        _ => Value::UNDEFINED,
    }
}
