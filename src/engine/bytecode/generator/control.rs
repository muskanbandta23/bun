//! Jumps out of constructs that have to clean up after themselves.

use bun_engine_syntax::ast::{BindingKind, ScopeFlags, ScopeId, StmtId, StmtKind};
use bun_engine_syntax::{FunctionKind, Name};
use smallvec::SmallVec;

use super::{Dest, Generator, Suspension, This};
use crate::assembler::{Label, SuspendedTries};
use crate::code::{CodeKind, HandlerKind};
use crate::operand::{CloseMode, DebugHook, Operand, Register, StaticError};

#[derive(Clone, Copy)]
pub(super) enum Control {
    Loop {
        on_break: Label,
        on_continue: Label,
    },
    Switch {
        on_break: Label,
    },
    Labeled {
        name: Name,
        on_break: Label,
    },
    /// To be closed on the way out.
    Iterator {
        state: Register,
        /// Outside of the handler that closes it.
        try_depth: u32,
        range: (u32, u32, u32),
    },
    /// An index into `finallys`.
    Finally(usize),
}

#[derive(Clone, Copy)]
pub(super) enum Exit {
    /// Leaves the controls from `depth` on.
    Jump {
        target: Label,
        depth: usize,
    },
    Return(Operand),
}

pub(super) struct Finally {
    entry: Label,
    /// Empty, an exception, or the index of an exit.
    completion: Register,
    /// What is to be returned.
    value: Option<Register>,
    exits: SmallVec<[Exit; 2]>,
}

/// The start of a handler that joins code which is reached in other ways too.
pub(super) struct DeferredHandler {
    handler: Label,
    exception: Register,
    value: Register,
    target: Label,
}

#[derive(Clone, Copy)]
struct Resource {
    value: Register,
    method: Register,
    is_async: bool,
}

pub(super) struct UsingScope {
    resources: SmallVec<[Resource; 2]>,
    declared: usize,
}

impl Generator<'_> {
    // Leaving

    pub(super) fn break_target(&self, label: Option<Name>) -> Option<Exit> {
        let controls = self.b.controls.iter().enumerate().rev();
        controls.into_iter().find_map(|(index, control)| {
            let target = match (*control, label) {
                (Control::Loop { on_break, .. } | Control::Switch { on_break }, None) => on_break,
                (Control::Labeled { name, on_break }, Some(label)) if name == label => on_break,
                _ => return None,
            };
            Some(Exit::Jump {
                target,
                depth: index + 1,
            })
        })
    }

    pub(super) fn continue_target(&self, label: Option<Name>) -> Option<Exit> {
        let mut outermost = None;
        for (index, control) in self.b.controls.iter().enumerate().rev() {
            match *control {
                Control::Loop { on_continue, .. } => {
                    outermost = Some(Exit::Jump {
                        target: on_continue,
                        depth: index + 1,
                    });
                    if label.is_none() {
                        break;
                    }
                }
                Control::Labeled { name, .. } if Some(name) == label => break,
                _ => {}
            }
        }
        outermost
    }

    /// The label, if nothing has to be done on the way there.
    pub(super) fn plain_jump(&self, exit: Exit) -> Option<Label> {
        let Exit::Jump { target, depth } = exit else {
            return None;
        };
        let needs_work =
            |it: &Control| matches!(it, Control::Iterator { .. } | Control::Finally(_));
        (!self.b.controls[depth..].iter().any(needs_work)).then_some(target)
    }

    /// Where the value of a `return` waits while `finally` code runs.
    pub(super) fn return_dest(&self) -> Dest {
        let value = self.b.finallys.last().and_then(|finally| finally.value);
        value.map_or(Dest::Any, Dest::Prefer)
    }

    pub(super) fn exit(&mut self, exit: Exit) {
        let depth = match exit {
            Exit::Jump { depth, .. } => depth,
            Exit::Return(_) => 0,
        };
        let mut suspended: SmallVec<[SuspendedTries; 2]> = SmallVec::new();
        let mut finally = None;
        for index in (depth..self.b.controls.len()).rev() {
            match self.b.controls[index] {
                Control::Iterator {
                    state,
                    try_depth,
                    range,
                } => {
                    suspended.push(self.asm().suspend_tries(try_depth));
                    self.position(range.0, range.1, range.2);
                    self.asm().iterator_close(state, CloseMode::Normal);
                }
                Control::Finally(index) => {
                    finally = Some(index);
                    break;
                }
                _ => {}
            }
        }
        match (finally, exit) {
            (Some(index), _) => self.exit_through(index, exit),
            (None, Exit::Jump { target, .. }) => self.asm().jump(target),
            (None, Exit::Return(value)) => {
                let base = self.base_try_depth;
                suspended.push(self.asm().suspend_tries(base));
                self.debug_hook(DebugHook::WillLeaveCallFrame, self.ast.root.end);
                self.leave_frame(value);
            }
        }
        for tries in suspended.into_iter().rev() {
            self.asm().resume_tries(tries);
        }
    }

    fn exit_through(&mut self, index: usize, mut exit: Exit) {
        if !self.asm().is_reachable() {
            return;
        }
        let finally = &mut self.b.finallys[index];
        let (entry, completion) = (finally.entry, finally.completion);
        let mut returned = None;
        if let (Exit::Return(value), Some(register)) = (exit, finally.value) {
            returned = Some((register, value));
            exit = Exit::Return(register.into());
        }
        let known = finally.exits.iter().position(|it| match (*it, exit) {
            (Exit::Return(_), Exit::Return(_)) => true,
            (Exit::Jump { target: a, .. }, Exit::Jump { target: b, .. }) => a.word() == b.word(),
            _ => false,
        });
        let id = known.unwrap_or_else(|| {
            finally.exits.push(exit);
            finally.exits.len() - 1
        });
        if let Some((register, value)) = returned {
            self.mov(register, value);
        }
        let id = self.int32(id as i32);
        self.asm().mov(completion, id);
        self.asm().jump(entry);
    }

    pub(super) fn leave_frame(&mut self, value: Operand) {
        match (self.suspension, self.generator.as_register()) {
            (Suspension::Generator | Suspension::AsyncGenerator, Some(generator)) => {
                self.asm().generator_return(generator, value);
            }
            (Suspension::AsyncFunction, _) => {
                let generator = self.generator;
                self.asm().async_return(generator, value);
            }
            _ if self.kind == CodeKind::Construct
                && self.function_kind != FunctionKind::NakedConstructor =>
            {
                self.leave_constructor(value);
            }
            _ => self.asm().ret(value),
        }
    }

    /// The last steps of [[Construct]].
    fn leave_constructor(&mut self, value: Operand) {
        let is_undefined = value == self.undefined();
        if !is_undefined && !matches!(self.this, This::Register(this) if value == this) {
            let not_object = self.asm().label();
            self.asm().jump_if_not_object(value, not_object);
            self.asm().ret(value);
            self.asm().bind(not_object);
            if self.function_kind == FunctionKind::DerivedConstructor {
                let (undefined, valid) = (self.undefined(), self.asm().label());
                self.asm().jump_if_identical(value, undefined, valid);
                self.throw_naming_function(StaticError::DerivedConstructorResult);
                self.asm().bind(valid);
            }
        }
        let this = self.this_value();
        self.asm().ret(this);
    }

    // Handlers

    pub(super) fn close_iterator_on_throw(&mut self, handler: Label, state: Register) {
        let (mark, exception, value) = (self.top, self.temp(), self.temp());
        self.asm().bind(handler);
        self.asm().catch(exception, value);
        self.asm().iterator_close(state, CloseMode::Throw);
        self.asm().rethrow(exception);
        self.top = mark;
    }

    pub(super) fn emit_deferred_handlers(&mut self) {
        for index in 0..self.b.deferred_handlers.len() {
            let DeferredHandler {
                handler,
                exception,
                value,
                target,
            } = self.b.deferred_handlers[index];
            self.asm().bind(handler);
            self.asm().catch(exception, value);
            self.asm().jump(target);
        }
    }

    /// Whether control may return from the function while it is inside of the statement.
    pub(super) fn may_return(&mut self, id: StmtId) -> bool {
        if let Suspension::Generator | Suspension::AsyncGenerator = self.suspension {
            return true;
        }
        if !self.can_recurse() {
            return true;
        }
        let ast = self.ast;
        match ast[id].kind {
            StmtKind::Return(_) => true,
            StmtKind::Block { body, .. } => ast.iter(body).any(|it| self.may_return(it)),
            StmtKind::If {
                consequent,
                alternate,
                ..
            } => self.may_return(consequent) || alternate.is_some_and(|it| self.may_return(it)),
            StmtKind::DoWhile { body, .. }
            | StmtKind::While { body, .. }
            | StmtKind::For { body, .. }
            | StmtKind::With { body, .. }
            | StmtKind::Labeled { body, .. } => self.may_return(body),
            StmtKind::ForIn(it) | StmtKind::ForOf(it) | StmtKind::ForAwait(it) => {
                self.may_return(it.body)
            }
            StmtKind::Switch { cases, .. } => {
                (ast.iter(cases)).any(|case| ast.iter(ast[case].body).any(|it| self.may_return(it)))
            }
            StmtKind::Try {
                block,
                handler,
                finalizer,
                ..
            } => [Some(block), handler, finalizer]
                .into_iter()
                .flatten()
                .any(|it| self.may_return(it)),
            _ => false,
        }
    }

    /// From here on, exits go through the code that follows [`Generator::enter_finally`].
    pub(super) fn open_finally(&mut self, may_return: bool) {
        let completion = self.temp();
        let empty = self.empty();
        self.asm().mov(completion, empty);
        let value = may_return.then(|| {
            let (value, undefined) = (self.temp(), self.undefined());
            self.asm().mov(value, undefined);
            value
        });
        let entry = self.asm().label();
        (self.b.controls).push(Control::Finally(self.b.finallys.len()));
        self.b.finallys.push(Finally {
            entry,
            completion,
            value,
            exits: SmallVec::new(),
        });
        self.tail_blockers += 1;
    }

    pub(super) fn finally_entry(&mut self) -> Label {
        match self.b.finallys.last() {
            Some(finally) => finally.entry,
            None => self.asm().label(),
        }
    }

    /// `handler` catches what the protected code throws, and leaves the value in `thrown`. The control of the
    /// innermost open one has been popped.
    pub(super) fn enter_finally(&mut self, handler: Label, thrown: Register) -> Option<Finally> {
        let finally = self.b.finallys.pop()?;
        self.tail_blockers -= 1;
        if self.asm().is_referenced(handler) {
            if self.asm().is_reachable() || self.asm().is_referenced(finally.entry) {
                self.b.deferred_handlers.push(DeferredHandler {
                    handler,
                    exception: finally.completion,
                    value: thrown,
                    target: finally.entry,
                });
            } else {
                self.asm().bind(handler);
                self.asm().catch(finally.completion, thrown);
            }
        }
        self.asm().bind(finally.entry);
        Some(finally)
    }

    /// Goes on with what was under way when the code was entered.
    pub(super) fn leave_finally(&mut self, finally: &Finally) {
        if finally.exits.is_empty() {
            return self.asm().rethrow_if_any(finally.completion);
        }
        let (empty, done) = (self.empty(), self.asm().label());
        self.asm()
            .jump_if_identical(finally.completion, empty, done);
        for (id, &exit) in (0..).zip(&finally.exits) {
            let (id, next) = (self.int32(id), self.asm().label());
            self.asm()
                .jump_if_not_identical(finally.completion, id, next);
            self.exit(exit);
            self.asm().bind(next);
        }
        self.asm().rethrow(finally.completion);
        self.asm().bind(done);
    }

    /// The end of code that runs under `handler`, where an error does not replace an exception that is under way.
    pub(super) fn leave_finally_keeping_exception(&mut self, finally: &Finally, handler: Label) {
        self.leave_finally(finally);
        if !self.asm().is_referenced(handler) {
            return;
        }
        let (mark, exception, value) = (self.top, self.temp(), self.temp());
        let (done, replaces) = (self.asm().label(), self.asm().label());
        self.asm().jump(done);
        self.asm().bind(handler);
        self.asm().catch(exception, value);
        let empty = self.empty();
        (self.asm()).jump_if_identical(finally.completion, empty, replaces);
        self.asm().jump_if_number(finally.completion, replaces);
        self.asm().rethrow(finally.completion);
        self.asm().bind(replaces);
        self.asm().rethrow(exception);
        self.asm().bind(done);
        self.top = mark;
    }

    // `using`

    /// Runs `body` and then disposes of the resources that the scope declares.
    pub(super) fn with_disposal(&mut self, scope: Option<ScopeId>, body: impl FnOnce(&mut Self)) {
        match scope {
            Some(scope) if self.ast[scope].flags.contains(ScopeFlags::HAS_USING) => {
                let handler = self.open_disposal(scope);
                body(self);
                self.close_disposal(handler);
            }
            _ => body(self),
        }
    }

    #[cold]
    fn open_disposal(&mut self, scope: ScopeId) -> (Label, Register, u32) {
        let mark = self.top;
        let (empty, undefined) = (self.empty(), self.undefined());
        let mut resources = SmallVec::new();
        for binding in self.ast.bindings(scope) {
            if let BindingKind::Using | BindingKind::AwaitUsing = binding.kind {
                let (value, method) = (self.temp(), self.temp());
                self.asm().mov(value, undefined);
                self.asm().mov(method, empty);
                resources.push(Resource {
                    value,
                    method,
                    is_async: binding.kind == BindingKind::AwaitUsing,
                });
            }
        }
        self.b.usings.push(UsingScope {
            resources,
            declared: 0,
        });
        let error = self.temp();
        self.asm().mov(error, empty);
        self.open_finally(true);
        let handler = self.asm().label();
        self.asm()
            .push_try(handler, HandlerKind::SynthesizedFinally);
        (handler, error, mark)
    }

    pub(super) fn add_disposable(&mut self, value: Operand, position: u32, is_async: bool) {
        let Some(using) = self.b.usings.last_mut() else {
            return;
        };
        let Some(&resource) = using.resources.get(using.declared) else {
            return;
        };
        using.declared += 1;
        self.asm().mov(resource.value, value);
        self.position_at(position);
        (self.asm()).get_dispose_method(resource.method, value, is_async);
    }

    /// DisposeResources. The method of a resource is empty if its declaration was not reached.
    #[cold]
    fn close_disposal(&mut self, (handler, error, mark): (Label, Register, u32)) {
        self.asm().pop_try();
        self.b.controls.pop();
        let (Some(using), Some(finally)) =
            (self.b.usings.pop(), self.enter_finally(handler, error))
        else {
            return;
        };
        let (empty, undefined) = (self.empty(), self.undefined());
        let (no, yes) = (self.boolean(false), self.boolean(true));
        let position = self.ast.root.end;
        let has_thrown = self.temp();
        self.asm().mov(has_thrown, no);
        // Undefined while no `await` is called for, true while one is due, false once one has taken place.
        let awaits = self.temp();
        if using.resources.iter().any(|resource| resource.is_async) {
            self.asm().mov(awaits, undefined);
        }
        let (result, caught) = (self.temp(), self.temp());
        let mut may_be_due = false;
        for resource in using.resources.iter().rev() {
            let (next, failed) = (self.asm().label(), self.asm().label());
            self.asm().jump_if_identical(resource.method, empty, next);
            if resource.is_async {
                let has_method = self.asm().label();
                (self.asm()).jump_if_not_identical(resource.method, undefined, has_method);
                self.asm().jump_if_identical(awaits, no, next);
                self.asm().mov(awaits, yes);
                self.asm().jump(next);
                self.asm().bind(has_method);
                may_be_due = true;
            } else {
                if may_be_due {
                    self.await_if_due(awaits, result, position);
                }
                self.asm()
                    .jump_if_identical(resource.method, undefined, next);
            }
            self.asm().push_try(failed, HandlerKind::SynthesizedCatch);
            self.position_at(position);
            self.call_method(result, resource.method, resource.value.into(), None);
            if resource.is_async {
                self.asm().mov(awaits, no);
                self.await_value(result, result.into(), position);
            }
            self.asm().pop_try();
            self.asm().jump(next);
            self.asm().bind(failed);
            self.asm().catch(result, caught);
            let is_first = self.asm().label();
            self.asm().jump_if_identical(error, empty, is_first);
            self.asm().new_suppressed_error(caught, caught, error);
            self.asm().bind(is_first);
            self.asm().mov(error, caught);
            self.asm().mov(has_thrown, yes);
            self.asm().bind(next);
        }
        if may_be_due {
            self.await_if_due(awaits, result, position);
        }
        let disposed = self.asm().label();
        self.asm().jump_if_false(has_thrown, disposed);
        self.asm().throw(error);
        self.asm().bind(disposed);
        self.leave_finally(&finally);
        self.top = mark;
    }

    fn await_if_due(&mut self, awaits: Register, scratch: Register, position: u32) {
        let (yes, undefined, done) = (self.boolean(true), self.undefined(), self.asm().label());
        self.asm().jump_if_not_identical(awaits, yes, done);
        self.await_value(scratch, undefined, position);
        self.asm().mov(awaits, undefined);
        self.asm().bind(done);
    }
}
