use core::mem::offset_of;

use bun_engine_bytecode::Specialization;
use bun_engine_macros::{cell, object_ops};
use bun_engine_text::{PropertyName, StringRef, names};

use crate::call::native_not_a_constructor;
use crate::{
    Attributes, CallData, Cx, Executable, Field, Gc, GlobalObject, JsBoundFunction, JsFunction,
    JsObject, JsObjectInit, JsResult, JsString, NativeFunction, ProxyObject, Shape, TypeFlags,
    Value,
};

/// A native function that is a class of its own, such as a constructor of the library.
#[cell(name = "Function", cell_type = InternalFunction, ops, export = "engine_class_InternalFunction",
    flags = TypeFlags::IMPLEMENTS_HAS_INSTANCE.union(TypeFlags::IMPLEMENTS_DEFAULT_HAS_INSTANCE))]
pub struct InternalFunction {
    base: JsObject,
    global: Gc<GlobalObject>,
    executable: Gc<Executable>,
    function: NativeFunction,
    constructor: NativeFunction,
    pub(self) original_name: Field<Option<Gc<JsString>>>,
}

const _: () = {
    assert!(offset_of!(InternalFunction, global) == JsFunction::OFFSET_OF_SCOPE);
    assert!(offset_of!(InternalFunction, executable) == JsFunction::OFFSET_OF_EXECUTABLE);
};

#[object_ops]
impl InternalFunction {
    fn call_data(&self) -> CallData {
        CallData::native(self.function)
    }

    fn construct_data(&self) -> CallData {
        self.native_function(Specialization::Construct)
            .map_or(CallData::NONE, CallData::native)
    }
}

impl InternalFunction {
    pub const OFFSET_OF_FUNCTION: usize = offset_of!(InternalFunction, function);
    pub const OFFSET_OF_CONSTRUCTOR: usize = offset_of!(InternalFunction, constructor);

    /// For a type that extends this one. `define_length_and_name` follows.
    pub fn init(
        cx: &Cx,
        shape: Gc<Shape>,
        function: NativeFunction,
        constructor: Option<NativeFunction>,
    ) -> InternalFunctionInit {
        InternalFunctionInit {
            base: JsObjectInit::new(cx, shape),
            global: cx.global(),
            executable: cx.internal_function_executable(),
            function,
            constructor: constructor.unwrap_or(native_not_a_constructor),
            original_name: None,
        }
    }

    pub fn define_length_and_name(&self, vm: &crate::Vm, length: u32, name: Gc<JsString>) {
        let attributes = Attributes::READ_ONLY | Attributes::DONT_ENUM;
        self.set_original_name(Some(name));
        self.put_direct(vm, names::length, length.into(), attributes);
        self.put_direct(vm, names::name, name.into(), attributes);
    }

    /// Defines `length`, `name` and `prototype`, and `constructor` on the prototype.
    pub fn finish_constructor(
        &self,
        cx: &Cx,
        length: u32,
        name: PropertyName<'_>,
        prototype: &JsObject,
    ) {
        self.define_length_and_name(cx, length, JsString::from_name(cx, name));
        self.put_direct(cx, names::prototype, prototype.into(), Attributes::CONSTANT);
        let this = self.as_value();
        prototype.put_direct(cx, names::constructor, this, Attributes::DONT_ENUM);
    }

    pub fn global(&self) -> Gc<GlobalObject> {
        self.global
    }

    pub fn native_function(&self, specialization: Specialization) -> Option<NativeFunction> {
        let none = native_not_a_constructor as NativeFunction;
        match specialization {
            Specialization::Call => Some(self.function),
            Specialization::Construct => {
                (!core::ptr::fn_addr_eq(self.constructor, none)).then_some(self.constructor)
            }
        }
    }

    pub fn name(&self) -> StringRef {
        self.original_name()
            .map_or_default(|name| name.to_string_without_resolving())
    }
}

/// GetFunctionRealm
pub fn function_realm(cx: &Cx, object: Gc<JsObject>) -> JsResult<Gc<GlobalObject>> {
    let mut object = object;
    loop {
        if let Some(bound) = object.downcast::<JsBoundFunction>() {
            object = bound.target_function();
        } else if let Some(proxy) = object.downcast::<ProxyObject>() {
            if proxy.is_revoked() {
                return Err(cx.throw_type_error("Cannot get function realm from revoked Proxy"));
            }
            object = proxy.target();
        } else if let Some(function) = object.downcast::<JsFunction>() {
            return Ok(function.function_realm());
        } else {
            return Ok(object.realm().unwrap_or_else(|| cx.global()));
        }
    }
}

impl Cx {
    /// A shape like `base` whose prototype is `new_target.prototype`, if that is an object.
    pub fn subclass_shape(
        &mut self,
        new_target: Gc<JsObject>,
        base: Gc<Shape>,
    ) -> JsResult<Gc<Shape>> {
        let global = base.global().unwrap_or_else(|| self.global());
        let cache = new_target
            .downcast::<JsFunction>()
            .filter(|function| function.can_use_allocation_profile())
            .map(|function| function.ensure_rare_data(self));
        if let Some(shape) = cache.and_then(|it| it.subclass_shape())
            && shape.class().is(base.class())
            && shape.instance_header() == base.instance_header()
            && shape.global() == base.global()
        {
            return Ok(shape);
        }
        let Some(prototype) = new_target.get(self, names::prototype)?.as_object() else {
            return Ok(base);
        };
        let shape = if base.prototype() == prototype.into() {
            base
        } else {
            base.with_prototype(self, global, prototype)
        };
        if let Some(rare_data) = cache {
            rare_data.set_subclass_shape(Some(shape));
            rare_data.allocation_watchpoints().start_watching();
        }
        Ok(shape)
    }

    /// OrdinaryCreateFromConstructor, as far as the shape goes. `base` picks it from a realm.
    #[inline]
    pub fn shape_from_new_target(
        &mut self,
        new_target: Value,
        callee: &JsObject,
        base: impl FnOnce(&GlobalObject, &crate::Vm) -> Gc<Shape>,
    ) -> JsResult<Gc<Shape>> {
        match new_target.as_object() {
            Some(new_target) if !new_target.is_same(callee) => {
                let realm = function_realm(self, new_target)?;
                let base = base(&realm, self);
                self.subclass_shape(new_target, base)
            }
            _ => Ok(base(&self.global(), self)),
        }
    }
}
