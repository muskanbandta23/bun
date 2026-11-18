use bun_engine_bytecode::{ErrorKind, StaticError};
use bun_engine_text::PropertyName;
use bun_engine_vm::code::metadata::{Counter, Flags};
use bun_engine_vm::{Cx, ErrorType, JsString, Thrown, errors};

use super::{Next, execute};

fn error_type(kind: ErrorKind) -> ErrorType {
    match kind {
        ErrorKind::TypeError => ErrorType::TypeError,
        ErrorKind::RangeError | ErrorKind::OutOfMemory => ErrorType::RangeError,
        ErrorKind::ReferenceError => ErrorType::ReferenceError,
        ErrorKind::SyntaxError => ErrorType::SyntaxError,
    }
}

fn throw_static(cx: &Cx, error: StaticError, name: PropertyName<'_>) -> Thrown {
    let (error_type, message) = (error_type(error.kind()), error.message());
    match error.message_around_name() {
        Some((before, after)) if !name.text().is_empty() => {
            errors::throw_naming(cx, error_type, before, name, after, message)
        }
        _ => cx.throw_error(error_type, message),
    }
}

execute! {
    Wide16Prefix(op, cx, f) {
        unreachable!()
    }
    Wide32Prefix(op, cx, f) {
        unreachable!()
    }
    Goto(op, cx, f) {
        Next::FirstTarget
    }
    LoopHint(op, cx, f) {
        cx.check_traps()?;
    }
    JumpIfBuiltin(op, cx, f) {
        let is_builtin = f.reg(op.value) == f.get(op.builtin);
        if !is_builtin {
            f.entry::<Flags>(op.metadata).0.set(1);
        }
        Next::jump_if(is_builtin)
    }
    SwitchInt(op, cx, f) {
        let (table, value) = (&f.code_block.body().int_switches()[op.table.index()], f.get(op.value));
        let number = value.as_number().filter(|&it| f64::from(it as i32) == it);
        Next::Offset(number.map_or(table.default, |number| table.target(number as i32)))
    }
    SwitchChar(op, cx, f) {
        let table = &f.code_block.body().int_switches()[op.table.index()];
        Next::Offset(match f.get(op.value).as_string() {
            Some(string) if string.len() == 1 => table.target(string.unit_at(cx, 0)?.into()),
            _ => table.default,
        })
    }
    SwitchString(op, cx, f) {
        let table = &f.code_block.body().string_switches()[op.table.index()];
        let keys = &f.code_block.code().string_switches[op.table.index()];
        let key = match f.get(op.value).as_string() {
            Some(string) => string.to_existing_property_key(cx)?,
            None => None,
        };
        let case = key.and_then(|key| keys.iter().position(|it| *it == key.name()));
        Next::Offset(case.map_or(table.default, |case| table.targets[case]))
    }
    Catch(op, cx, f) {
        let exception = cx.clear_exception().expect("what the handler was found for");
        f.entry::<Counter>(op.metadata).0.update(|count| count + 1);
        f.set(op.exception, exception);
        f.set(op.value, exception.value());
    }
    Throw(op, cx, f) {
        return Err(cx.throw(f.get(op.value)))
    }
    Rethrow(op, cx, f) {
        return Err(cx.throw_exception(f.cell(op.exception)))
    }
    RethrowIfAny(op, cx, f) {
        if let Some(exception) = f.reg_or_empty(op.exception) {
            return Err(cx.throw_exception(exception.cast()));
        }
    }
    ThrowError(op, cx, f) {
        if op.kind == ErrorKind::OutOfMemory {
            return Err(cx.throw_out_of_memory());
        }
        let message = f.get(op.message).cast::<JsString>().to_string_ref(cx)?;
        return Err(cx.throw_error(error_type(op.kind), message))
    }
    ThrowStatic(op, cx, f) {
        return Err(throw_static(cx, op.error, f.name(op.name)))
    }
    ThrowIfNotObject(op, cx, f) {
        if !f.get(op.value).is_object() {
            return Err(throw_static(cx, op.error, PropertyName::EMPTY));
        }
    }
    ThrowIfNullish(op, cx, f) {
        if f.get(op.value).is_undefined_or_null() {
            return Err(throw_static(cx, op.error, f.name(op.name)));
        }
    }
    Debug(op, cx, f) {
        (cx.hooks().debug)(cx, f.header, op.hook, f.reg(op.scope))?;
    }
    Cover(op, cx, f) {
        (cx.hooks().cover)(cx, f.code_block, op.block, f.entry(op.metadata));
    }
}
