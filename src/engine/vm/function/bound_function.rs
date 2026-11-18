use core::cell::Cell;

use bun_engine_bytecode::Visibility;
use bun_engine_macros::{cell, native_abi, object_ops};
use bun_engine_text::{StringRef, names};

use crate::{
    CallFrame, Cx, Field, Gc, ImmutableArray, Intrinsic, JsFunction, JsObject, JsResult, JsString,
    NativeExecutable, Shape, TypeFlags, Value, ValueVec, Vm,
};

#[cell(name = "Function", ops, export = "engine_class_BoundFunction",
    cleared_flags = TypeFlags::IMPLEMENTS_DEFAULT_HAS_INSTANCE)]
pub struct JsBoundFunction {
    base: JsFunction,
    target_function: Gc<JsObject>,
    bound_this: Value,
    /// Up to three arguments, or in the first an `ImmutableArray` of all of them.
    arguments: [Option<Value>; JsBoundFunction::MAX_INLINE_ARGUMENTS],
    /// Without the prefix. Until it is needed: the original name of the target.
    pub(self) cached_name: Field<Option<Gc<JsString>>>,
    /// NaN until it is needed: follows from the original length of the target.
    length: Cell<f64>,
    argument_count: u32,
    can_construct: bool,
}

#[object_ops]
impl JsBoundFunction {
    fn custom_has_instance(&self, cx: &mut Cx, value: Value) -> JsResult<bool> {
        self.target_function.has_instance(cx, value)
    }
}

impl JsBoundFunction {
    pub const MAX_INLINE_ARGUMENTS: usize = 3;

    pub(crate) fn create_executable(vm: &Vm) -> Gc<NativeExecutable> {
        let (intrinsic, visibility) = (Intrinsic::BoundFunctionCall, Visibility::Private);
        let name = StringRef::empty();
        NativeExecutable::create(vm, 0, name, call, construct, intrinsic, visibility)
    }

    /// `name` and `length` may be left out if the target is a `JsFunction` that has not defined its own yet.
    pub fn create(
        cx: &mut Cx,
        target: Gc<JsObject>,
        bound_this: Value,
        arguments: &[Value],
        length: Option<f64>,
        name: Option<Gc<JsString>>,
    ) -> JsResult<Gc<JsBoundFunction>> {
        assert!((length.is_some() && name.is_some()) || target.is::<JsFunction>());
        let shape = Self::shape_for(cx, target)?;
        let mut inline = [None; Self::MAX_INLINE_ARGUMENTS];
        if arguments.len() > Self::MAX_INLINE_ARGUMENTS {
            let list = ImmutableArray::try_from_values(cx, arguments);
            inline[0] = Some(cx.or_out_of_memory(list)?.into());
        } else {
            for (slot, &argument) in inline.iter_mut().zip(arguments) {
                *slot = Some(argument);
            }
        }
        Ok(cx.allocate::<JsBoundFunction>(JsBoundFunctionInit {
            base: JsFunction::native_init(cx, shape, cx.bound_function_executable()),
            target_function: target,
            bound_this,
            arguments: inline,
            cached_name: name,
            length: Cell::new(length.unwrap_or(f64::NAN)),
            argument_count: arguments.len() as u32,
            can_construct: target.is_constructor(),
        }))
    }

    fn shape_for(cx: &mut Cx, target: Gc<JsObject>) -> JsResult<Gc<Shape>> {
        let (global, prototype) = (cx.global(), target.get_prototype_of(cx)?);
        let base = global.bound_function_shape(cx);
        if prototype == base.prototype() {
            return Ok(base);
        }
        Ok(match prototype.as_object() {
            Some(prototype) => base.with_prototype(cx, global, prototype),
            None => {
                let spec = crate::ShapeSpec::of::<JsBoundFunction>();
                Shape::create(cx, Some(global), Value::NULL, spec)
            }
        })
    }

    pub fn target_function(&self) -> Gc<JsObject> {
        self.target_function
    }

    pub fn bound_this(&self) -> Value {
        self.bound_this
    }

    pub fn can_construct(&self) -> bool {
        self.can_construct
    }

    pub fn argument_count(&self) -> usize {
        self.argument_count as usize
    }

    pub fn for_each_argument(&self, visit: impl FnMut(Value)) {
        if self.argument_count() > Self::MAX_INLINE_ARGUMENTS {
            let list = self.arguments[0].and_then(Value::downcast::<ImmutableArray>);
            list.iter().flat_map(|list| list.iter()).for_each(visit);
        } else {
            self.arguments.iter().flatten().copied().for_each(visit);
        }
    }

    /// The bound functions from this one down to the first that does not depend on its target.
    fn chain(&self, is_known: impl Fn(&JsBoundFunction) -> bool) -> crate::RootVec<Gc<Self>> {
        let mut chain = crate::root_vec();
        let mut next = Some(Gc::from(self));
        while let Some(function) = next.filter(|function| !is_known(function)) {
            chain.push(function);
            next = function.target_function.downcast();
        }
        chain
    }

    pub fn bound_length(&self) -> f64 {
        for function in self.chain(|it| !it.length.get().is_nan()).iter().rev() {
            let target = function.target_function.cast::<JsFunction>();
            let length = target.original_length() - f64::from(function.argument_count);
            function.length.set(length.max(0.0));
        }
        self.length.get()
    }

    pub fn target_name(&self, cx: &Cx) -> JsResult<Gc<JsString>> {
        for function in self.chain(|it| it.cached_name().is_some()).iter().rev() {
            let target = function.target_function.cast::<JsFunction>();
            function.set_cached_name(Some(target.original_name(cx)?));
        }
        Ok(self
            .cached_name()
            .unwrap_or_else(|| cx.small_strings.empty()))
    }

    pub(super) fn name_without_allocating(&self) -> StringRef {
        let mut builder = bun_engine_text::StringBuilder::new();
        let mut function = Gc::from(self);
        loop {
            if let Some(name) = function.cached_name() {
                builder.push_text(name.to_string_without_resolving().text());
                break;
            }
            match function.target_function.downcast::<JsBoundFunction>() {
                Some(target) => function = target,
                None => {
                    builder.push_text(super::display_name(&function.target_function).text());
                    break;
                }
            }
            builder.push_text(names::boundPrefix.text());
        }
        builder.finish().unwrap_or_default()
    }

    fn all_arguments(&self, cx: &Cx, frame: &CallFrame, all: &mut ValueVec) -> JsResult<()> {
        self.for_each_argument(|argument| all.push(argument));
        cx.or_out_of_memory(all.try_extend_from_slice(frame.arguments()))
    }
}

#[native_abi]
fn call(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let bound = frame.callee().cast::<JsBoundFunction>();
    let target = bound.target_function.into();
    if bound.argument_count == 0 {
        return cx.call(target, bound.bound_this, frame.arguments());
    }
    let mut arguments = ValueVec::new();
    bound.all_arguments(cx, frame, &mut arguments)?;
    cx.call(target, bound.bound_this, &arguments)
}

#[native_abi]
fn construct(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let bound = frame.callee().cast::<JsBoundFunction>();
    let target = bound.target_function.into();
    if !bound.can_construct {
        return Err(crate::errors::throw_not_a_constructor(cx, bound.into()));
    }
    let new_target = if frame.new_target() == bound.into() {
        target
    } else {
        frame.new_target()
    };
    let mut arguments = ValueVec::new();
    bound.all_arguments(cx, frame, &mut arguments)?;
    cx.construct(target, &arguments, new_target).map(Into::into)
}
