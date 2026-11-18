use core::cell::Cell;

use bun_engine_macros::{cell, object_ops};
use bun_engine_text::{PropertyName, StringBuilder, StringRef, names};

use crate::{
    Attributes, Cx, DeletePropertySlot, DontEnumMode, Gc, GlobalObject, JsDestructibleObject,
    JsObject, JsObjectInit, JsResult, JsString, Message, PropertyDescriptor, PropertyNameList,
    PropertySlot, PutPropertySlot, RealmSlot, Shape, SourceAppender, StackFrame, TracedVec,
    TypeFlags, Value, Vm, capture_stack_trace,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum ErrorType {
    Error,
    EvalError,
    RangeError,
    ReferenceError,
    SyntaxError,
    TypeError,
    UriError,
    AggregateError,
    SuppressedError,
}

#[derive(Default)]
pub struct ErrorHooks {
    /// The value of `stack`.
    pub compute_stack: Cell<Option<fn(&mut Cx, Gc<ErrorInstance>) -> JsResult<Value>>>,
}

bitflags::bitflags! {
    #[derive(Clone, Copy)]
    struct Flags: u8 {
        const STACK_OVERFLOW = 1 << 0;
        const OUT_OF_MEMORY = 1 << 1;
        const INFO_IS_MATERIALIZED = 1 << 2;
        const NATIVE_GETTER_TYPE_ERROR = 1 << 3;
        const PARSE_ERROR = 1 << 4;
    }
}

/// `stack`, `line`, `column` and `sourceURL` are defined when something asks for one of them.
#[cell(name = "Error", cell_type = ErrorInstance, ops, export = "engine_class_ErrorInstance",
    flags = TypeFlags::GET_OWN_PROPERTY_SLOT_IS_IMPURE_FOR_ABSENCE)]
pub struct ErrorInstance {
    base: JsDestructibleObject,
    pub stack_trace: TracedVec<StackFrame>,
    error_type: ErrorType,
    flags: Cell<Flags>,
}

#[object_ops]
impl ErrorInstance {
    fn get_own_property_slot(
        &self,
        cx: &mut Cx,
        name: PropertyName<'_>,
        slot: &mut PropertySlot,
    ) -> JsResult<bool> {
        self.materialize_for(cx, name)?;
        self.ordinary_get_own_property_slot(cx, name, slot)
    }

    fn own_special_property_names(
        &self,
        cx: &mut Cx,
        _: &mut PropertyNameList,
        mode: DontEnumMode,
    ) -> JsResult<()> {
        if mode == DontEnumMode::Include {
            self.materialize(cx)?;
        }
        Ok(())
    }

    fn put(
        &self,
        cx: &mut Cx,
        name: PropertyName<'_>,
        value: Value,
        slot: &mut PutPropertySlot,
    ) -> JsResult<bool> {
        if self.materialize_for(cx, name)? {
            slot.disable_caching();
        }
        self.ordinary_put(cx, name, value, slot)
    }

    fn delete_property(
        &self,
        cx: &mut Cx,
        name: PropertyName<'_>,
        slot: &mut DeletePropertySlot,
    ) -> JsResult<bool> {
        if self.materialize_for(cx, name)? {
            slot.disable_caching();
        }
        self.ordinary_delete_property(cx, name, slot)
    }

    fn define_own_property(
        &self,
        cx: &mut Cx,
        name: PropertyName<'_>,
        descriptor: &PropertyDescriptor,
        should_throw: bool,
    ) -> JsResult<bool> {
        self.materialize_for(cx, name)?;
        self.ordinary_define_own_property(cx, name, descriptor, should_throw)
    }

    fn prevent_extensions(&self, cx: &mut Cx) -> JsResult<bool> {
        self.materialize(cx)?;
        self.ordinary_prevent_extensions(cx);
        Ok(true)
    }
}

impl GlobalObject {
    pub fn shape_for_error(&self, vm: &Vm, error_type: ErrorType) -> Gc<Shape> {
        let slot = RealmSlot::error_shape.offset_by(error_type as usize);
        self.slot(vm, slot).cast()
    }
}

impl ErrorInstance {
    /// The innermost `skip` frames are not part of the stack trace, nor those up to a call of `below`.
    pub fn create(
        cx: &Cx,
        shape: Gc<Shape>,
        error_type: ErrorType,
        skip: usize,
        below: Option<Gc<JsObject>>,
    ) -> Gc<ErrorInstance> {
        let error = cx.allocate::<ErrorInstance>(ErrorInstanceInit {
            base: JsObjectInit::new(cx, shape),
            error_type,
            flags: Cell::new(Flags::empty()),
        });
        match cx.global().stack_trace_limit().get() {
            Some(limit) => {
                let frames = capture_stack_trace(cx, skip, limit as usize, below);
                error.stack_trace().extend_from_slice(&frames);
            }
            None => error.set(Flags::INFO_IS_MATERIALIZED),
        }
        error
    }

    pub fn new(cx: &Cx, error_type: ErrorType, message: Option<StringRef>) -> Gc<ErrorInstance> {
        let shape = cx.global().shape_for_error(cx, error_type);
        let error = Self::create(cx, shape, error_type, 0, None);
        if let Some(message) = message {
            error.define_message(cx, message);
        }
        error
    }

    pub fn with_source(
        cx: &Cx,
        error_type: ErrorType,
        message: StringRef,
        appender: SourceAppender,
        value: Value,
    ) -> Gc<ErrorInstance> {
        let error = Self::new(cx, error_type, None);
        let appended = error.stack_trace().get(0).and_then(|frame| {
            appender.append(
                message.text(),
                &*frame.code_block?,
                frame.bytecode_offset,
                value,
            )
        });
        error.define_message(cx, appended.unwrap_or(message));
        error
    }

    pub fn out_of_memory(cx: &Cx, message: StringRef) -> Gc<ErrorInstance> {
        let error = Self::new(cx, ErrorType::RangeError, Some(message));
        error.set(Flags::OUT_OF_MEMORY);
        error
    }

    pub fn stack_overflow(cx: &Cx) -> Gc<ErrorInstance> {
        let message = "Maximum call stack size exceeded.".into_string();
        let error = Self::new(cx, ErrorType::RangeError, Some(message));
        error.set(Flags::STACK_OVERFLOW);
        error
    }

    fn define_message(&self, vm: &Vm, message: StringRef) {
        let message = JsString::new(vm, message).into();
        self.put_direct(vm, names::message, message, Attributes::DONT_ENUM);
    }

    fn set(&self, flags: Flags) {
        self.flags.set(self.flags.get() | flags);
    }

    pub fn error_type(&self) -> ErrorType {
        self.error_type
    }

    pub fn is_stack_overflow(&self) -> bool {
        self.flags.get().contains(Flags::STACK_OVERFLOW)
    }

    pub fn is_out_of_memory(&self) -> bool {
        self.flags.get().contains(Flags::OUT_OF_MEMORY)
    }

    pub fn set_native_getter_type_error(&self) {
        self.set(Flags::NATIVE_GETTER_TYPE_ERROR);
    }

    pub fn set_parse_error(&self) {
        self.set(Flags::PARSE_ERROR);
    }

    /// Whether the properties have just been defined.
    #[inline]
    fn materialize_for(&self, cx: &mut Cx, name: PropertyName<'_>) -> JsResult<bool> {
        if name == names::stack
            || name == names::line
            || name == names::column
            || name == names::sourceURL
        {
            return self.materialize(cx);
        }
        Ok(false)
    }

    pub fn materialize(&self, cx: &mut Cx) -> JsResult<bool> {
        if self.flags.get().contains(Flags::INFO_IS_MATERIALIZED) {
            return Ok(false);
        }
        self.set(Flags::INFO_IS_MATERIALIZED);
        let frames = self.stack_trace().take();
        if let Some(frame) = frames.iter().find(|frame| frame.code_block.is_some())
            && let Some(position) = frame.line_column()
        {
            let attributes = Attributes::DONT_ENUM;
            self.put_direct(cx, names::line, position.line.into(), attributes);
            self.put_direct(cx, names::column, position.column.into(), attributes);
            let url = frame.source_url();
            if !url.is_empty() {
                let url = JsString::new(cx, url).into();
                self.put_direct(cx, names::sourceURL, url, attributes);
            }
        }
        let mut stack = StringBuilder::new();
        for (index, frame) in frames.iter().enumerate() {
            if index != 0 {
                stack.push_latin1(b'\n');
            }
            frame.write(&mut stack);
        }
        let stack = JsString::new(cx, stack.into_string()).into();
        self.put_direct(cx, names::stack, stack, Attributes::DONT_ENUM);
        Ok(true)
    }
}
