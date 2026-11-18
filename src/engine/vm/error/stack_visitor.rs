use core::cell::Cell;
use std::rc::Rc;

use bun_engine_bytecode::{CodeKind, HandlerKind, Opcode, RawInstruction, Visibility};

use crate::{
    CallFrame, CellType, CodeBlock, CodeOrigin, CompiledCode, Cx, EntryRecord, Gc, JsCell,
    JsFunction, JsObject, LineColumn, RealmSlot, Value, ValueVec, Vm,
};

/// What a frame of WebAssembly code runs.
#[derive(Clone, Copy)]
pub struct WasmFrame {
    /// What `Hooks::wasm_names` finds the names in.
    pub names: Option<Gc<JsCell>>,
    pub function_index: u32,
    pub visibility: Visibility,
    /// The function that has inlined this one comes next.
    pub is_inlined: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum FrameKind {
    Script,
    Eval,
    Function,
    Module,
    Native,
    Wasm,
}

/// A frame as the script sees it: a call that was inlined has one too, and so has a direct `eval`.
#[derive(Clone, Copy)]
pub struct VisitedFrame<'a> {
    pub machine_frame: &'a CallFrame,
    /// Where native code last called into generated code on the way to this frame.
    pub entry_frame: usize,
    /// The baseline one. `None` for native code.
    pub code_block: Option<Gc<CodeBlock>>,
    pub bytecode_offset: u32,
    pub inline_frame: Option<u32>,
    /// `None` for WebAssembly code.
    pub callee: Option<Gc<JsObject>>,
    pub wasm: Option<WasmFrame>,
}

impl VisitedFrame<'_> {
    pub fn kind(&self) -> FrameKind {
        match self.code_block.map(|it| it.body().kind()) {
            Some(CodeKind::Script) => FrameKind::Script,
            Some(CodeKind::Eval) => FrameKind::Eval,
            Some(CodeKind::Module) => FrameKind::Module,
            Some(CodeKind::Call | CodeKind::Construct) => FrameKind::Function,
            None if self.wasm.is_some() => FrameKind::Wasm,
            None => FrameKind::Native,
        }
    }

    #[inline]
    pub fn visibility(&self) -> Visibility {
        if let Some(code_block) = self.code_block {
            return code_block.executable().visibility();
        }
        if let Some(wasm) = self.wasm {
            return wasm.visibility;
        }
        let function = self.callee.and_then(Gc::downcast::<JsFunction>);
        function.map_or(Visibility::Public, |it| it.executable().visibility())
    }

    /// Whether stack traces leave it out.
    #[inline]
    pub fn is_private(&self) -> bool {
        self.visibility() != Visibility::Public
            && !crate::options().show_private_scripts_in_stack_traces
    }

    pub fn line_column(&self) -> Option<LineColumn> {
        Some(self.code_block?.line_column(self.bytecode_offset))
    }

    /// What the function was called with, or has since stored in its parameters. Without `this`.
    pub fn copy_arguments(&self, out: &mut ValueVec) {
        let compiled = self.machine_frame.code_block().and_then(|it| it.compiled());
        match (self.inline_frame, compiled) {
            (Some(index), Some(compiled)) => {
                let mut all = ValueVec::new();
                // SAFETY: the visitor has the index from the call site of the frame.
                unsafe { compiled.inline_arguments(self.machine_frame, index, &mut all) };
                all.iter().skip(1).for_each(|&argument| out.push(argument));
            }
            _ => (self.machine_frame.arguments().iter()).for_each(|&argument| out.push(argument)),
        }
    }
}

enum Next {
    /// Nothing of the machine frame has been visited.
    First,
    Origin(CodeOrigin),
    WasmInliner(u32),
}

pub struct StackVisitor<'a> {
    vm: &'a Vm,
    frame: Option<&'a CallFrame>,
    entry_frame: usize,
    /// Into the code of `frame`. Zero if that has called an operation, which has no frame.
    return_address: usize,
    next: Next,
    compiled: Option<Rc<dyn CompiledCode>>,
    visits_tail_deleted_frames: bool,
}

impl<'a> StackVisitor<'a> {
    /// Innermost first.
    #[inline]
    pub fn frames(vm: &'a Vm) -> StackVisitor<'a> {
        StackVisitor {
            vm,
            frame: vm.top_call_frame(),
            entry_frame: vm.top_entry_frame(),
            return_address: 0,
            next: Next::First,
            compiled: None,
            visits_tail_deleted_frames: false,
        }
    }

    /// With those that a tail call would have removed.
    pub(crate) fn frames_for_debugger(vm: &'a Vm) -> StackVisitor<'a> {
        StackVisitor {
            visits_tail_deleted_frames: true,
            ..Self::frames(vm)
        }
    }

    /// Without those that `first` has called.
    pub fn frames_from(vm: &'a Vm, first: &CallFrame) -> StackVisitor<'a> {
        let mut visitor = Self::frames(vm);
        while visitor.frame.is_some_and(|it| !core::ptr::eq(it, first)) {
            visitor.leave_machine_frame();
        }
        visitor
    }

    #[inline(always)]
    fn leave_machine_frame(&mut self) {
        let Some(frame) = self.frame else { return };
        let mut caller = frame.caller_address();
        self.return_address = frame.return_address();
        if caller == self.entry_frame {
            // SAFETY: further up the stack.
            let record = unsafe { EntryRecord::of(caller) };
            caller = record.previous_top_call_frame;
            self.entry_frame = record.previous_top_entry_frame;
            self.return_address = 0;
        }
        // SAFETY: as above.
        self.frame = (caller != 0).then(|| unsafe { CallFrame::from_address(caller) });
        self.next = Next::First;
    }

    #[cold]
    fn visit_wasm(&mut self, frame: &'a CallFrame) -> VisitedFrame<'a> {
        let depth = match self.next {
            Next::WasmInliner(depth) => depth,
            _ => 0,
        };
        let wasm = (self.vm.hooks().wasm_frame)(self.vm, frame, self.return_address, depth);
        let entry_frame = self.entry_frame;
        if wasm.is_inlined {
            self.next = Next::WasmInliner(depth + 1);
        } else {
            self.leave_machine_frame();
        }
        VisitedFrame {
            machine_frame: frame,
            entry_frame,
            code_block: None,
            bytecode_offset: 0,
            inline_frame: None,
            callee: None,
            wasm: Some(wasm),
        }
    }

    #[inline(never)]
    fn visit_inlined(
        &mut self,
        frame: &'a CallFrame,
        bytecode_offset: u32,
        index: u32,
    ) -> VisitedFrame<'a> {
        let compiled = self.compiled.as_deref().expect("only it inlines");
        let mut inlined = compiled.inline_frame(self.vm, index);
        let visited = VisitedFrame {
            machine_frame: frame,
            entry_frame: self.entry_frame,
            code_block: Some(inlined.code_block),
            bytecode_offset,
            inline_frame: Some(index),
            // SAFETY: the origin is that of the call site of the frame.
            callee: unsafe { compiled.inline_callee(frame, index) }.as_object(),
            wasm: None,
        };
        while inlined.is_tail_call {
            let Some(caller) = inlined.caller.inline_frame else {
                self.leave_machine_frame();
                return visited;
            };
            inlined = compiled.inline_frame(self.vm, caller);
        }
        self.next = Next::Origin(inlined.caller);
        visited
    }
}

/// Of a frame without a code block: its code is being linked, or has found no room on the stack.
#[inline]
fn has_not_begun(callee: Gc<JsObject>) -> bool {
    match callee.downcast::<JsFunction>() {
        Some(function) => !function.is_native(),
        None => callee.cell_type() == CellType::Callee,
    }
}

impl<'a> Iterator for StackVisitor<'a> {
    type Item = VisitedFrame<'a>;

    #[inline]
    fn next(&mut self) -> Option<VisitedFrame<'a>> {
        loop {
            let frame = self.frame?;
            if frame.has_native_callee() {
                return Some(self.visit_wasm(frame));
            }
            let mut visited = VisitedFrame {
                machine_frame: frame,
                entry_frame: self.entry_frame,
                code_block: None,
                bytecode_offset: 0,
                inline_frame: None,
                callee: Some(frame.callee()),
                wasm: None,
            };
            let Some(code_block) = frame.code_block() else {
                self.leave_machine_frame();
                if has_not_begun(frame.callee()) {
                    continue;
                }
                return Some(visited);
            };
            let origin = match self.next {
                Next::Origin(origin) => origin,
                _ => {
                    self.compiled = code_block.compiled_if_optimized();
                    let origin = match &self.compiled {
                        Some(compiled) => compiled.origin_of_call_site(frame.call_site()),
                        None => CodeOrigin {
                            bytecode_offset: code_block.bytecode_offset_of(frame.call_site()),
                            inline_frame: None,
                        },
                    };
                    if self.return_address == 0
                        && origin.inline_frame.is_none()
                        && code_block.opcode_at(origin.bytecode_offset) == Some(Opcode::DirectEval)
                        && let Some(eval) =
                            (code_block.global()).slot_if_initialized(RealmSlot::eval_function)
                    {
                        self.next = Next::Origin(origin);
                        visited.callee = eval.as_object();
                        return Some(visited);
                    }
                    origin
                }
            };
            if let Some(index) = origin.inline_frame {
                return Some(self.visit_inlined(frame, origin.bytecode_offset, index));
            }
            let has_called = self.return_address != 0;
            self.leave_machine_frame();
            if has_called
                && code_block.keeps_frame_at(origin.bytecode_offset)
                && !self.visits_tail_deleted_frames
            {
                continue;
            }
            visited.code_block = Some(code_block.baseline());
            visited.bytecode_offset = origin.bytecode_offset;
            return Some(visited);
        }
    }
}

impl CodeBlock {
    #[inline(always)]
    fn compiled_if_optimized(&self) -> Option<Rc<dyn CompiledCode>> {
        if self.is_optimized() {
            self.compiled()
        } else {
            None
        }
    }

    pub fn opcode_at(&self, bytecode_offset: u32) -> Option<Opcode> {
        let bytes = self.body().instructions();
        match Opcode::from_byte(*bytes.get(bytecode_offset as usize)?)? {
            Opcode::Wide16Prefix | Opcode::Wide32Prefix => {
                RawInstruction::read(bytes, bytecode_offset).map(|it| it.opcode)
            }
            opcode => Some(opcode),
        }
    }
}

impl Vm {
    /// Whether what is thrown now ends in a handler that takes the value and forgets where it came from.
    pub(super) fn is_sure_to_catch(&self) -> bool {
        let entry_frame = self.top_entry_frame();
        StackVisitor::frames(self)
            .take_while(|frame| frame.entry_frame == entry_frame && frame.wasm.is_none())
            .filter_map(|frame| Some((frame.code_block?, frame.bytecode_offset)))
            .any(|(code_block, offset)| {
                code_block.body().handlers().iter().any(|handler| {
                    matches!(
                        handler.kind,
                        HandlerKind::Catch | HandlerKind::SynthesizedCatch
                    ) && handler.range.contains(&offset)
                })
            })
    }
}

/// What a call from native code is made on behalf of.
pub(super) struct EntryContext {
    context: Value,
    /// The one around that of the call.
    outer_entry_frame: usize,
    previous: *const EntryContext,
}

pub(super) struct EntryContexts(Cell<*const EntryContext>);

impl Default for EntryContexts {
    fn default() -> EntryContexts {
        EntryContexts(Cell::new(core::ptr::null()))
    }
}

impl Cx {
    /// `enter` makes one call, whose entry frame gets the context: the async function that it resumes, for one.
    #[inline(always)]
    pub fn with_entry_context<R>(&mut self, context: Value, enter: impl FnOnce(&mut Cx) -> R) -> R {
        let entry = EntryContext {
            context,
            outer_entry_frame: self.top_entry_frame(),
            previous: self.services.errors.entry_contexts.0.get(),
        };
        self.services.errors.entry_contexts.0.set(&raw const entry);
        let result = enter(self);
        self.services.errors.entry_contexts.0.set(entry.previous);
        result
    }
}

impl Vm {
    /// # Safety
    /// The address of an entry frame that is on the stack.
    pub unsafe fn entry_context(&self, entry_frame: usize) -> Option<Value> {
        // SAFETY: the caller's.
        let outer = unsafe { EntryRecord::of(entry_frame) }.previous_top_entry_frame;
        let mut next = self.services.errors.entry_contexts.0.get();
        // SAFETY: each is in the frame of a call of `with_entry_context` that has not returned.
        while let Some(entry) = unsafe { next.as_ref() } {
            if entry.outer_entry_frame == outer {
                return Some(entry.context);
            }
            next = entry.previous;
        }
        None
    }
}
