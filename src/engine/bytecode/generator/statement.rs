//! Statements. `completion` is where the value goes that a script or `eval` code ends with.

use bun_engine_number::exact_int32;
use bun_engine_syntax::ast::{
    BindingFlags, BindingKind, CaseId, ExprId, ExprKind, FunctionId, List, PatternId, ScopeId,
    Stmt, StmtId, StmtKind,
};
use bun_engine_syntax::{FunctionFlags, Name};
use smallvec::SmallVec;

use super::control::{Control, Exit};
use super::operator::Branch;
use super::{Dest, Generator, Suspension, Tail, This};
use crate::assembler::Label;
use crate::code::{CodeKind, HandlerKind};
use crate::operand::{DebugHook, Operand, Register, Unresolved};

const FEWEST_CASES_IN_A_TABLE: u32 = 3;

impl Generator<'_> {
    fn has_completion_value(&mut self, id: StmtId) -> bool {
        let ast = self.ast;
        match ast[id].kind {
            StmtKind::Empty
            | StmtKind::Debugger
            | StmtKind::Variables(_)
            | StmtKind::Function(_)
            | StmtKind::Class(_)
            | StmtKind::Continue(_)
            | StmtKind::Break(_)
            | StmtKind::ModuleItem
            | StmtKind::Export(_) => false,
            StmtKind::Block { body, .. } => {
                self.can_recurse() && ast.iter(body).any(|it| self.has_completion_value(it))
            }
            StmtKind::Labeled { body, .. } => self.can_recurse() && self.has_completion_value(body),
            _ => true,
        }
    }

    /// Whether it may jump away before anything in it has a value.
    fn jumps_early(&mut self, id: StmtId) -> bool {
        let ast = self.ast;
        match ast[id].kind {
            StmtKind::Continue(_) | StmtKind::Break(_) => true,
            StmtKind::Block { body, .. } if self.can_recurse() => {
                for statement in ast.iter(body) {
                    if self.jumps_early(statement) {
                        return true;
                    }
                    if self.has_completion_value(statement) {
                        return false;
                    }
                }
                false
            }
            StmtKind::Labeled { body, .. } => self.can_recurse() && self.jumps_early(body),
            _ => false,
        }
    }

    fn reset_completion(&mut self, completion: Option<Register>) {
        if let Some(completion) = completion {
            let undefined = self.undefined();
            self.asm().mov(completion, undefined);
        }
    }

    #[inline]
    fn reset_if_jumps_early(&mut self, completion: Option<Register>, body: StmtId) {
        if completion.is_some() && self.jumps_early(body) {
            self.reset_completion(completion);
        }
    }

    pub(super) fn statements(
        &mut self,
        list: List<StmtId>,
        completion: Option<Register>,
        end: u32,
    ) {
        let ast = self.ast;
        let outer_end = core::mem::replace(&mut self.statements_end, end);
        let last_with_value =
            completion.and_then(|_| ast.iter(list).rfind(|&it| self.has_completion_value(it)));
        for (index, statement) in (0..).zip(ast.iter(list)) {
            if Some(statement) == last_with_value {
                self.reset_completion(completion);
            }
            if self.covers {
                self.covered_statement(statement, list.skip(index + 1), completion);
            } else {
                self.statement(statement, completion);
            }
        }
        self.statements_end = outer_end;
    }

    /// What follows a statement that control may leave, or never come back to, is a block of its own.
    #[cold]
    fn covered_statement(&mut self, id: StmtId, rest: List<StmtId>, completion: Option<Register>) {
        let (ast, suspensions) = (self.ast, self.suspensions);
        self.statement(id, completion);
        let runs_through = matches!(
            ast[id].kind,
            StmtKind::Empty
                | StmtKind::Debugger
                | StmtKind::Expression(_)
                | StmtKind::Variables(_)
                | StmtKind::Function(_)
                | StmtKind::Class(_)
                | StmtKind::ModuleItem
                | StmtKind::Export(_)
                | StmtKind::DefineField { .. }
                | StmtKind::StaticBlock { .. }
        ) || matches!(ast[id].kind, StmtKind::Block { body, .. } if body.is_empty());
        let runs_here = |it: StmtId| !matches!(ast[it].kind, StmtKind::Function(_));
        if (!runs_through || self.suspensions != suspensions) && ast.iter(rest).any(runs_here) {
            self.cover(ast[id].end, self.statements_end);
        }
    }

    pub(super) fn statement(&mut self, id: StmtId, completion: Option<Register>) {
        if !self.can_recurse() {
            return;
        }
        let node = &self.ast[id];
        if self.debugs && node.pause && !matches!(node.kind, StmtKind::Debugger) {
            self.debug_hook(DebugHook::WillExecuteStatement, node.start);
        }
        let mark = self.top;
        match node.kind {
            StmtKind::Empty | StmtKind::ModuleItem => {}
            StmtKind::Debugger => self.debug_hook(DebugHook::DidReachDebuggerStatement, node.start),
            StmtKind::Expression(expression) => {
                self.expr(expression, completion.map_or(Dest::Ignore, Dest::Local));
            }
            StmtKind::Variables(expression) | StmtKind::Class(expression) => {
                self.effect(expression);
            }
            StmtKind::Block { body, scope } => self.block(node, body, scope, completion),
            StmtKind::Function(function) => self.hoist_function(function, node.start),
            StmtKind::If { .. } => self.if_statement(id, completion),
            StmtKind::Return(value) => self.return_statement(node.start, value),
            StmtKind::DoWhile { body, test } => self.do_while(node, body, test, completion),
            StmtKind::While { test, body } => {
                self.reset_if_jumps_early(completion, body);
                self.test_first_loop(Some(test), None, body, None, completion);
            }
            StmtKind::For { .. } => self.for_statement(node, completion),
            StmtKind::ForIn(ref it) => {
                self.reset_if_jumps_early(completion, it.body);
                self.for_in(it, completion);
            }
            StmtKind::ForOf(ref it) => {
                self.reset_if_jumps_early(completion, it.body);
                self.for_of(it, completion);
            }
            StmtKind::ForAwait(ref it) => {
                self.reset_if_jumps_early(completion, it.body);
                self.for_await(it, completion);
            }
            StmtKind::Continue(label) => {
                if let Some(exit) = self.continue_target(label) {
                    self.exit(exit);
                }
            }
            StmtKind::Break(label) => {
                if let Some(exit) = self.break_target(label) {
                    self.exit(exit);
                }
            }
            StmtKind::With {
                object,
                body,
                object_start,
                object_end,
            } => {
                let object = self.expr(object, Dest::Any);
                self.position(object_end, object_start, object_end);
                self.enter_with_scope(object);
                self.reset_if_jumps_early(completion, body);
                self.statement(body, completion);
                self.close_scope();
            }
            StmtKind::Switch {
                discriminant,
                cases,
                scope,
            } => self.switch(discriminant, cases, scope, completion),
            StmtKind::Labeled { label, body } => {
                let on_break = self.asm().label();
                self.b.controls.push(Control::Labeled {
                    name: label,
                    on_break,
                });
                self.statement(body, completion);
                self.b.controls.pop();
                self.asm().bind(on_break);
            }
            StmtKind::Throw { value, end } => {
                let value = self.expr(value, Dest::Any);
                self.position(end, node.start, end);
                self.asm().throw(value);
            }
            StmtKind::Try { .. } => self.try_statement(node, completion),
            StmtKind::Export(declaration) => self.statement(declaration, None),
            StmtKind::DefineField { index, key, value } => {
                self.define_field(node.start, index, key, value);
            }
            StmtKind::StaticBlock { function } => self.run_static_block(node, function),
        }
        self.top = mark;
    }

    #[inline(never)]
    fn block(
        &mut self,
        node: &Stmt,
        body: List<StmtId>,
        scope: Option<ScopeId>,
        completion: Option<Register>,
    ) {
        let end = node.end.min(self.statements_end);
        let Some(id) = scope else {
            return self.statements(body, completion, end);
        };
        self.enter_block_scope(id);
        self.with_disposal(scope, |generator| {
            generator.statements(body, completion, end);
        });
        self.close_scope();
    }

    /// The statements of a function, which tell when `this` comes to be bound.
    pub(super) fn constructor_body(&mut self, list: List<StmtId>) {
        let ast = self.ast;
        for statement in ast.iter(list) {
            self.statement(statement, None);
            if let StmtKind::Expression(expression) = ast[statement].kind
                && let ExprKind::Call { callee, .. } = ast[expression].kind
                && self.is_super(callee)
                && let This::Unbound(this) | This::UnboundInScope(this) = self.this
            {
                self.this = This::Register(this);
            }
        }
    }

    #[inline(never)]
    fn hoist_function(&mut self, function: FunctionId, position: u32) {
        let node = &self.ast[function];
        if !node.flags.contains(FunctionFlags::HOISTED_AS_VARIABLE) {
            return;
        }
        let name = self.declared_name(function);
        let variable = self.resolve(name, position);
        let value = self.load(&variable, None, Dest::Any, Unresolved::Throw);
        if let CodeKind::Script | CodeKind::Eval = self.kind {
            let (value, name) = (self.in_register(value), self.identifier(name));
            self.asm().put_hoisted(Register::SCOPE, name, value);
        } else {
            let place = self.place_below_lexicals(name);
            self.put(place, name, None, value);
        }
    }

    /// Where `if (..) break;` jumps, if that is all there is to it.
    fn jump_only(&self, mut id: StmtId) -> Option<Label> {
        if self.debugs || self.covers {
            return None;
        }
        if let StmtKind::Block { body, .. } = self.ast[id].kind
            && body.len() == 1
        {
            id = self.ast.at(body, 0);
        }
        self.plain_jump(match self.ast[id].kind {
            StmtKind::Break(label) => self.break_target(label)?,
            StmtKind::Continue(label) => self.continue_target(label)?,
            _ => return None,
        })
    }

    #[inline(never)]
    fn if_statement(&mut self, mut id: StmtId, completion: Option<Register>) {
        let (ast, done) = (self.ast, self.asm().label());
        while let StmtKind::If {
            test,
            consequent,
            alternate,
        } = ast[id].kind
        {
            if completion.is_some()
                && (self.jumps_early(consequent)
                    || alternate.is_some_and(|it| self.jumps_early(it)))
            {
                self.reset_completion(completion);
            }
            if let Some(target) = self.jump_only(consequent) {
                self.enter_if_false(test, target);
            } else {
                let otherwise = match alternate {
                    Some(_) => self.asm().label(),
                    None => done,
                };
                self.enter_if_true(test, otherwise);
                self.cover(ast[consequent].start, ast[consequent].end);
                self.statement(consequent, completion);
                if alternate.is_some() {
                    self.asm().jump(done);
                    self.asm().bind(otherwise);
                }
            }
            let Some(alternate) = alternate else {
                break;
            };
            self.cover(ast[consequent].end, ast[alternate].end);
            if !matches!(ast[alternate].kind, StmtKind::If { .. }) {
                self.statement(alternate, completion);
                break;
            }
            id = alternate;
        }
        self.asm().bind(done);
    }

    fn allows_tail_calls(&self) -> bool {
        self.kind == CodeKind::Call
            && self.function_flags.contains(FunctionFlags::STRICT)
            && self.suspension == Suspension::None
            && self.tail_blockers == 0
    }

    #[inline(never)]
    fn return_statement(&mut self, start: u32, value: Option<ExprId>) {
        let value = match value {
            None => self.undefined(),
            Some(value) if self.allows_tail_calls() => self.expr_in(value, Dest::Any, Tail::Yes),
            Some(value) if self.suspension == Suspension::AsyncGenerator => {
                let result = self.temp();
                let value = self.expr(value, Dest::Prefer(result));
                self.await_value(result, value, start);
                result.into()
            }
            Some(value) => self.expr(value, self.return_dest()),
        };
        self.exit(Exit::Return(value));
    }

    // Loops

    #[inline(never)]
    fn do_while(&mut self, node: &Stmt, body: StmtId, test: ExprId, completion: Option<Register>) {
        self.reset_if_jumps_early(completion, body);
        let [top, on_continue, on_break] = core::array::from_fn(|_| self.asm().label());
        self.asm().bind_loop_head(top);
        self.asm().loop_hint();
        self.plain_loop_body(body, on_break, on_continue, completion);
        self.cover(self.ast[body].end, node.end);
        let branch = Branch {
            if_true: top,
            if_false: on_break,
            falls_to: false,
        };
        self.condition(test, branch);
        self.asm().bind(on_break);
    }

    fn plain_loop_body(
        &mut self,
        body: StmtId,
        on_break: Label,
        on_continue: Label,
        completion: Option<Register>,
    ) {
        self.cover(self.ast[body].start, self.ast[body].end);
        self.b.controls.push(Control::Loop {
            on_break,
            on_continue,
        });
        self.statement(body, completion);
        self.b.controls.pop();
        self.asm().bind(on_continue);
    }

    /// The test comes twice, so that a turn takes one jump.
    #[inline(never)]
    fn test_first_loop(
        &mut self,
        test: Option<ExprId>,
        update: Option<ExprId>,
        body: StmtId,
        copied_scope: Option<Register>,
        completion: Option<Register>,
    ) {
        let [top, on_continue, on_break] = core::array::from_fn(|_| self.asm().label());
        self.cover_loop_header(test);
        if let Some(test) = test {
            self.enter_if_true(test, on_break);
        }
        self.asm().bind_loop_head(top);
        self.asm().loop_hint();
        self.plain_loop_body(body, on_break, on_continue, completion);
        if let Some(scope) = copied_scope {
            self.asm().copy_scope(scope);
        }
        self.cover_loop_header(update);
        if let Some(update) = update {
            self.effect(update);
        }
        self.cover_loop_header(test);
        match test {
            Some(test) => {
                let branch = Branch {
                    if_true: top,
                    if_false: on_break,
                    falls_to: false,
                };
                self.condition(test, branch);
            }
            None => self.asm().jump(top),
        }
        self.asm().bind(on_break);
    }

    #[inline(never)]
    fn for_statement(&mut self, node: &Stmt, completion: Option<Register>) {
        let StmtKind::For {
            init,
            test,
            update,
            body,
            scope,
            init_has_closure,
        } = node.kind
        else {
            return;
        };
        self.reset_if_jumps_early(completion, body);
        let mut copied_scope = None;
        if let Some(scope) = scope {
            self.enter_block_scope(scope);
            let is_per_turn = self.ast.bindings(scope).iter().any(|binding| {
                binding.kind == BindingKind::Let && binding.flags.contains(BindingFlags::CAPTURED)
            });
            copied_scope = is_per_turn.then_some(self.scope);
        }
        self.with_disposal(scope, |generator| {
            if let Some(init) = init {
                generator.effect(init);
            }
            if let Some(scope) = copied_scope.filter(|_| init_has_closure) {
                generator.asm().copy_scope(scope);
            }
            generator.test_first_loop(test, update, body, copied_scope, completion);
        });
        if scope.is_some() {
            self.close_scope();
        }
    }

    // `switch`

    #[inline(never)]
    fn switch(
        &mut self,
        discriminant: ExprId,
        cases: List<CaseId>,
        scope: Option<ScopeId>,
        completion: Option<Register>,
    ) {
        let ast = self.ast;
        self.reset_completion(completion);
        let tests = || ast.iter(cases).filter_map(|case| ast[case].test);
        let tests_assign = tests().any(|test| self.may_assign(test));
        let value = self.stable_expr(discriminant, tests_assign);
        if let Some(scope) = scope {
            self.enter_block_scope(scope);
        }
        let on_break = self.asm().label();
        let labels: SmallVec<[Label; 16]> = ast.iter(cases).map(|_| self.asm().label()).collect();
        let default = (ast.iter(cases).position(|case| ast[case].test.is_none()))
            .map_or(on_break, |index| labels[index]);
        if !self.switch_by_table(value, cases, &labels, default) {
            let was_in_case_test = core::mem::replace(&mut self.is_in_case_test, true);
            for (case, &label) in ast.iter(cases).zip(&labels) {
                if let Some(test) = ast[case].test {
                    let mark = self.top;
                    let test = self.expr(test, Dest::Any);
                    if self.has_identity(test) {
                        self.asm().jump_if_identical(test, value, label);
                    } else {
                        self.asm().jump_if_strict_equal(test, value, label);
                    }
                    self.top = mark;
                }
            }
            self.is_in_case_test = was_in_case_test;
            self.asm().jump(default);
        }
        self.b.controls.push(Control::Switch { on_break });
        for (case, &label) in ast.iter(cases).zip(&labels) {
            let case = &ast[case];
            self.asm().bind(label);
            self.cover(case.start, case.end);
            self.statements(case.body, completion, case.end);
        }
        self.b.controls.pop();
        if scope.is_some() {
            self.close_scope();
        }
        self.asm().bind(on_break);
    }

    #[inline(never)]
    fn switch_by_table(
        &mut self,
        value: Operand,
        cases: List<CaseId>,
        labels: &[Label],
        default: Label,
    ) -> bool {
        let ast = self.ast;
        let tests = || {
            (ast.iter(cases).zip(labels))
                .filter_map(|(case, &label)| Some((ast[ast[case].test?].kind, label)))
        };
        if (tests().count() as u32) < FEWEST_CASES_IN_A_TABLE {
            return false;
        }
        let integers = tests().map(|(test, label)| match test {
            ExprKind::Number { value, .. } => Some((exact_int32(value.get() + 0.0)?, label)),
            _ => None,
        });
        if let Some(integers) = integers.collect::<Option<SmallVec<[_; 16]>>>() {
            let table = self.asm().add_int_switch(default, &integers);
            self.asm().switch_int(value, table);
            return true;
        }
        let strings = tests().map(|(test, label)| match test {
            ExprKind::String(name) => Some((name, label)),
            _ => None,
        });
        let Some(strings) = strings.collect::<Option<SmallVec<[(Name, Label); 16]>>>() else {
            return false;
        };
        if strings.iter().all(|&(name, _)| self.name_length(name) == 1) {
            let units: SmallVec<[_; 16]> = (strings.iter())
                .map(|&(name, label)| (i32::from(self.text_of(name).at(0)), label))
                .collect();
            let table = self.asm().add_int_switch(default, &units);
            self.asm().switch_char(value, table);
            return true;
        }
        let keys = strings.iter().map(|&(name, _)| self.atom(name)).collect();
        self.b.string_switches.push(keys);
        let targets: SmallVec<[_; 16]> = strings.iter().map(|&(_, label)| label).collect();
        let table = self.asm().add_string_switch(default, &targets);
        self.asm().switch_string(value, table);
        true
    }

    // `try`

    #[inline(never)]
    fn try_statement(&mut self, node: &Stmt, completion: Option<Register>) {
        let StmtKind::Try {
            block,
            parameter,
            parameter_scope,
            handler,
            finalizer,
        } = node.kind
        else {
            return;
        };
        let Some(finalizer) = finalizer else {
            self.reset_if_jumps_early(completion, block);
            let done = self.asm().label();
            self.try_catch(block, parameter, parameter_scope, handler, completion, done);
            return self.asm().bind(done);
        };
        let inner_completion = completion.map(|_| self.temp());
        self.reset_completion(inner_completion);
        let may_return = self.may_return(block) || handler.is_some_and(|it| self.may_return(it));
        self.open_finally(may_return);
        let (entry, on_throw) = (self.finally_entry(), self.asm().label());
        self.asm().push_try(on_throw, HandlerKind::Finally);
        self.try_catch(
            block,
            parameter,
            parameter_scope,
            handler,
            inner_completion,
            entry,
        );
        self.asm().pop_try();
        self.b.controls.pop();
        let thrown = self.temp();
        let Some(finally) = self.enter_finally(on_throw, thrown) else {
            return;
        };
        let before = &self.ast[handler.unwrap_or(block)];
        self.cover(before.end, self.ast[finalizer].end);
        self.reset_if_jumps_early(completion, finalizer);
        self.statement(finalizer, completion);
        if let (Some(completion), Some(inner)) = (completion, inner_completion) {
            self.asm().mov(completion, inner);
        }
        self.leave_finally(&finally);
    }

    fn try_catch(
        &mut self,
        block: StmtId,
        parameter: Option<PatternId>,
        parameter_scope: Option<ScopeId>,
        handler: Option<StmtId>,
        completion: Option<Register>,
        done: Label,
    ) {
        let Some(handler) = handler else {
            return self.statement(block, completion);
        };
        let on_throw = self.asm().label();
        self.asm().push_try(on_throw, HandlerKind::Catch);
        self.tail_blockers += 1;
        self.statement(block, completion);
        self.tail_blockers -= 1;
        self.asm().pop_try();
        self.asm().jump(done);
        self.asm().bind(on_throw);
        let (mark, value) = (self.top, self.temp());
        let exception = self.temp();
        self.asm().catch(exception, value);
        self.top -= 1;
        let catch = self.asm().last();
        let scope = parameter.and(parameter_scope);
        if let Some(scope) = scope {
            self.enter_block_scope(scope);
        }
        if let Some(parameter) = parameter {
            match (self.resolve_pattern_register(parameter), catch) {
                (Some(register), Some((site, _))) => {
                    use crate::operand::Field;
                    self.asm().set_operand(site, 0, value.word());
                    self.asm().set_operand(site, 1, register.word());
                }
                _ => self.bind_pattern(parameter, value.into()),
            }
        }
        self.cover(self.ast[block].end, self.ast[handler].end);
        self.reset_completion(completion);
        self.statement(handler, completion);
        if scope.is_some() {
            self.close_scope();
        }
        self.top = mark;
    }
}
