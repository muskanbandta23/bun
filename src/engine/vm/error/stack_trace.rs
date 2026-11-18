use bun_engine_bytecode::{CodeKind, Visibility};
use bun_engine_macros::Trace;
use bun_engine_text::{StringBuilder, StringRef};

use crate::{
    CallFrame, CodeBlock, CodeOrigin, Gc, JsCell, JsFunction, JsObject, LineColumn, RootVec, Vm,
    display_name, options, root_vec,
};

/// A frame as the script sees it: a call that was inlined has one too.
#[derive(Clone, Copy)]
pub struct VisitedFrame<'a> {
    pub machine_frame: &'a CallFrame,
    /// The baseline one. `None` for native code.
    pub code_block: Option<Gc<CodeBlock>>,
    pub bytecode_offset: u32,
    pub inline_frame: Option<u32>,
    /// `None` for WebAssembly code.
    pub callee: Option<Gc<JsObject>>,
}

impl VisitedFrame<'_> {
    pub fn is_private(&self) -> bool {
        let function = self
            .callee
            .and_then(|callee| callee.downcast::<JsFunction>());
        function.is_some_and(|it| {
            let executable = it.executable();
            executable.visibility() != Visibility::Public
                || (self.code_block.is_none() && !executable.is_native())
        })
    }
}

pub struct StackVisitor;

impl StackVisitor {
    /// Innermost first.
    pub fn frames(vm: &Vm) -> impl Iterator<Item = VisitedFrame<'_>> {
        vm.machine_frames()
            .flat_map(move |frame| Self::frames_in(vm, frame))
    }

    fn frames_in<'a>(vm: &'a Vm, frame: &'a CallFrame) -> impl Iterator<Item = VisitedFrame<'a>> {
        let code_block = frame.code_block();
        let compiled = code_block
            .filter(|it| it.is_optimized())
            .and_then(|it| it.compiled());
        let mut next = Some(match &compiled {
            Some(compiled) => compiled.origin_of_call_site(frame.call_site()),
            None => CodeOrigin {
                bytecode_offset: code_block
                    .map_or(0, |it| it.bytecode_offset_of(frame.call_site())),
                inline_frame: None,
            },
        });
        core::iter::from_fn(move || {
            let origin = next.take()?;
            let (Some(index), Some(compiled)) = (origin.inline_frame, &compiled) else {
                return Some(VisitedFrame {
                    machine_frame: frame,
                    code_block: code_block.map(|it| it.baseline()),
                    bytecode_offset: origin.bytecode_offset,
                    inline_frame: None,
                    callee: (!frame.has_native_callee()).then(|| frame.callee()),
                });
            };
            let inlined = compiled.inline_frame(vm, index);
            next = Some(inlined.caller);
            Some(VisitedFrame {
                machine_frame: frame,
                code_block: Some(inlined.code_block),
                bytecode_offset: origin.bytecode_offset,
                inline_frame: Some(index),
                // SAFETY: the origin is that of the call site of the frame.
                callee: unsafe { compiled.inline_callee(frame, index) }.as_object(),
            })
        })
    }
}

/// A frame of a stack trace that has been captured.
#[derive(Clone, Copy, Trace)]
#[repr(C)]
pub struct StackFrame {
    /// Of WebAssembly code, what has the names of its functions.
    pub callee: Option<Gc<JsCell>>,
    pub code_block: Option<Gc<CodeBlock>>,
    /// Of WebAssembly code, the index of the function.
    pub bytecode_offset: u32,
    pub is_async: bool,
    pub is_wasm: bool,
}

impl StackFrame {
    pub fn line_column(&self) -> Option<LineColumn> {
        Some(self.code_block?.line_column(self.bytecode_offset))
    }

    pub fn function_name(&self) -> StringRef {
        if self.is_wasm {
            return StringRef::from_literal("wasm-stub");
        }
        match self.code_block.map(|it| it.body().kind()) {
            Some(CodeKind::Eval) => return StringRef::from_literal("eval code"),
            Some(CodeKind::Module) => return StringRef::from_literal("module code"),
            Some(CodeKind::Script) => return StringRef::from_literal("global code"),
            _ => {}
        }
        let callee = self.callee.and_then(|callee| callee.as_object());
        callee.map_or_default(|callee| display_name(&callee))
    }

    pub fn source_url(&self) -> StringRef {
        let Some(code_block) = self.code_block else {
            let text = if self.is_wasm {
                "[wasm code]"
            } else {
                "[native code]"
            };
            return StringRef::from_literal(text);
        };
        let executable = code_block.executable();
        let provider = executable.provider();
        provider
            .source_url_directive()
            .or_else(|| provider.url().cloned())
            .unwrap_or_default()
    }

    pub fn write(&self, out: &mut StringBuilder) {
        use core::fmt::Write;
        let url = self.source_url();
        out.push_text(self.function_name().text());
        out.push_latin1(b'@');
        out.push_text(url.text());
        if let Some(LineColumn { line, column }) = self.line_column().filter(|_| !url.is_empty()) {
            let _ = write!(out, ":{line}:{column}");
        }
    }
}

/// With `below`, the frames up to and including the first call of that function are left out.
pub fn capture_stack_trace(
    vm: &Vm,
    skip: usize,
    limit: usize,
    below: Option<Gc<JsObject>>,
) -> RootVec<StackFrame> {
    let shows_private = options().show_private_scripts_in_stack_traces;
    let mut frames = StackVisitor::frames(vm).skip(skip);
    if below.is_some() {
        frames.find(|frame| frame.callee == below);
    }
    let mut trace = root_vec();
    trace.extend(
        frames
            .filter(|frame| shows_private || !frame.is_private())
            .take(limit)
            .map(|frame| StackFrame {
                callee: frame.callee.map(Gc::cell),
                code_block: frame.code_block,
                bytecode_offset: frame.bytecode_offset,
                is_async: false,
                is_wasm: frame.callee.is_none(),
            }),
    );
    trace
}
