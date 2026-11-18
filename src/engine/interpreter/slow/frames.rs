use bun_engine_bytecode::Register;
use bun_engine_text::names;
use bun_engine_vm::code::metadata::WeakCell;
use bun_engine_vm::{
    Cx, ElementKind, Gc, JsArray, JsFinalObject, JsFunction, JsObject, JsResult, Value, errors,
    function_realm,
};

use super::{Next, execute};

fn create_this(cx: &mut Cx, new_target: Gc<JsObject>, capacity: u32) -> JsResult<Gc<JsObject>> {
    if let Some(function) = new_target.downcast::<JsFunction>()
        && function.can_use_allocation_profile()
    {
        let shape = function.shape_for_construction(cx, capacity as usize);
        return Ok(JsFinalObject::create(cx, shape).upcast());
    }
    let prototype = match new_target.get(cx, names::prototype)?.as_object() {
        Some(prototype) => prototype,
        None => function_realm(cx, new_target)?.object_prototype(),
    };
    Ok(cx.new_object_with_prototype(Some(prototype)))
}

execute! {
    Enter(op, cx, f) {
        // SAFETY: what has a code block is one of these.
        f.set(Register::SCOPE, unsafe { JsFunction::scope_of(f.header.callee().cell()) });
        for index in 1..=op.variables {
            f.set(Register::local(index), Value::UNDEFINED);
        }
    }
    Move(op, cx, f) {
        f.set_or_empty(op.dst, f.get_or_empty(op.src));
    }
    Return(op, cx, f) {
        Next::Return(f.get(op.value))
    }
    ToThis(op, cx, f) {
        f.set_profiled(op.this, op.metadata, f.reg(op.this).to_sloppy_this(cx));
    }
    CreateThis(op, cx, f) {
        let new_target = f.cell::<JsObject>(op.new_target);
        f.entry::<WeakCell>(op.metadata).record(f.code_block, &new_target);
        f.set(op.dst, create_this(cx, new_target, op.capacity)?);
    }
    CheckInitialized(op, cx, f) {
        if f.reg_or_empty(op.value).is_none() {
            return Err(errors::throw_uninitialized_variable(cx, f.name(op.name)));
        }
    }
    CheckThis(op, cx, f) {
        if f.reg_or_empty(op.this).is_none() {
            return Err(cx.throw_reference_error(
                "'super()' must be called in derived constructor before accessing |this| or returning non-object.",
            ));
        }
    }
    BindThis(op, cx, f) {
        if f.reg_or_empty(op.this).is_some() {
            return Err(cx.throw_reference_error("'super()' can't be called more than once in a constructor."));
        }
        f.set(op.this, f.reg(op.value));
    }
    ArgumentCount(op, cx, f) {
        f.set(op.dst, f.header.argument_count());
    }
    GetArgument(op, cx, f) {
        f.set_profiled(op.dst, op.metadata, f.header.argument(op.index as usize));
    }
    CreateRest(op, cx, f) {
        let (shape, rest) = (cx.global().array_shape(ElementKind::Contiguous), f.header.rest(op.skip as usize));
        f.set(op.dst, cx.or_out_of_memory(JsArray::try_from_values_with_shape(cx, shape, rest))?);
    }
    GetArgumentsLength(op, cx, f) {
        let length = match f.reg_or_empty(op.arguments) {
            Some(arguments) => arguments.get(cx, names::length)?,
            None => f.header.argument_count().into(),
        };
        f.set_profiled(op.dst, op.metadata, length);
    }
    GetArgumentByVal(op, cx, f) {
        let index = f.get(op.key).as_index_int32().filter(|_| f.reg_or_empty(op.arguments).is_none());
        let argument = index.and_then(|index| f.header.arguments().get(index as usize).copied());
        if let Some(argument) = argument {
            f.set_profiled(op.dst, op.metadata, argument);
        }
        Next::jump_if(argument.is_some())
    }
}
