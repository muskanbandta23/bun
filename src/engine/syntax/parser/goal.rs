//! The whole of what is parsed.

use bun_engine_text::StringRef;

use super::function::Site;
use super::{Context, Facts, Failed, In, Parsed, Session, Statement};
use crate::ast::{
    BindingFlags, BindingKind, Features, List, PropertyKey, ScopeKind, Stmt, StmtId, StmtKind,
};
use crate::error::Complaint;
use crate::names::Name;
use crate::options::{
    EvalContext, FieldKind, FieldSource, FunctionFlags, FunctionKind, FunctionSource, Goal,
};
use crate::token::TokenKind;

fn context_of_eval(allowed: EvalContext) -> Context {
    let mut context = Context::empty();
    for (condition, consequence) in [
        (EvalContext::STRICT, Context::STRICT),
        (EvalContext::IN_FUNCTION, Context::NEW_TARGET),
        (EvalContext::IN_METHOD, Context::SUPER_PROPERTY),
        (
            EvalContext::IN_DERIVED_CONSTRUCTOR,
            Context::SUPER_PROPERTY | Context::SUPER_CALL,
        ),
        (
            EvalContext::IN_FIELD_INITIALIZER,
            Context::NO_ARGUMENTS_IN_EVAL,
        ),
    ] {
        if allowed.contains(condition) {
            context |= consequence;
        }
    }
    context
}

impl Session<'_> {
    pub(super) fn root(&mut self) -> Parsed<()> {
        let body = match self.options.goal {
            Goal::Function(function) => self.function_goal(function)?,
            goal => self.program_goal(goal)?,
        };
        if self.options.capture_everything {
            self.scopes.capture_context();
        }
        let scope = self.scopes.close_root(self.ast);
        if self.facts.contains(Facts::PRIVATE_NAMES)
            && !self.context.contains(Context::CHECKED_BEFORE)
        {
            self.check_private_names()?;
        }
        let mut features = Features::empty();
        for (fact, feature) in [
            (Facts::USES_THIS, Features::USES_THIS),
            (Facts::USES_NEW_TARGET, Features::USES_NEW_TARGET),
            (Facts::AWAITS, Features::AWAITS),
            (Facts::TAGGED_TEMPLATE, Features::TAGGED_TEMPLATE),
        ] {
            features.set(feature, self.facts.contains(fact));
        }
        features.set(Features::STRICT, self.is_strict());
        features.set(
            Features::SIMPLE_PARAMETERS,
            !self.facts.contains(Facts::ELABORATE_PARAMETERS),
        );
        let uses = self.facts & (Facts::READS_ARGUMENTS_LENGTH | Facts::USES_ARGUMENTS);
        features.set(
            Features::ONLY_ARGUMENTS_LENGTH,
            uses == Facts::READS_ARGUMENTS_LENGTH
                && scope.is_some_and(|scope| {
                    self.ast.bindings(scope).iter().any(|binding| {
                        binding.kind == BindingKind::Arguments
                            && !binding.flags.contains(BindingFlags::CAPTURED)
                    })
                }),
        );
        let root = &mut self.ast.root;
        root.features = features;
        root.end = self.lexer.end();
        root.scope = scope;
        root.body = body;
        Ok(())
    }

    fn program_goal(&mut self, goal: Goal<'_>) -> Parsed<List<StmtId>> {
        let (kind, context) = match goal {
            Goal::Module => (
                ScopeKind::Module,
                Context::STRICT | Context::MODULE | Context::TOP_LEVEL_OF_MODULE,
            ),
            Goal::Script { strict: true } => (ScopeKind::Script, Context::STRICT),
            Goal::Eval { context, .. } => (ScopeKind::Script, context_of_eval(context)),
            _ => (ScopeKind::Script, Context::empty()),
        };
        self.set_context(context);
        self.scopes.push(kind, self.start());
        self.next();
        self.ast.root.start = self.start();
        let body = match goal {
            Goal::Module => self.module_items()?,
            Goal::SingleFunction { parameters_end } => self.single_function(parameters_end)?,
            _ => self.statements_with_directives::<true>()?,
        };
        if !self.at(TokenKind::End) {
            return Err(Failed);
        }
        if matches!(goal, Goal::Eval { .. }) && self.is_strict() {
            self.scopes.keep_variables_in_root();
        }
        let source = self.lexer.source;
        let text = |(start, end)| StringRef::try_narrowed(source.slice(start, end)).ok();
        self.ast.root.source_url = self.lexer.source_url.and_then(text);
        self.ast.root.source_mapping_url = self.lexer.source_mapping_url.and_then(text);
        Ok(body)
    }

    fn list_of_one(&mut self, statement: Statement) -> List<StmtId> {
        let list = self.open_list();
        self.ast.push_to_list(statement);
        self.ast.close_list(list)
    }

    fn single_function(&mut self, parameters_end: Option<u32>) -> Parsed<List<StmtId>> {
        let start = self.start();
        if self.at_contextual(Name::ASYNC) {
            self.next();
            if !self.at(TokenKind::Function) || self.follows_newline() {
                return Err(self.fail("Cannot parse the async function"));
            }
        } else if !self.at(TokenKind::Function) {
            return Err(self.unexpected());
        }
        let function = self.function_declaration::<true>(start, Site::Ordinary, parameters_end)?;
        Ok(self.list_of_one(function))
    }

    pub(super) fn is_private_name_declared_outside(&self, name: Name) -> bool {
        match self.options.goal {
            Goal::Eval {
                is_private_name_declared,
                ..
            } => is_private_name_declared(self.lexer.names.text(name, self.lexer.source.text)),
            _ => false,
        }
    }

    #[cold]
    fn check_private_names(&mut self) -> Parsed<()> {
        let undeclared = (self.scopes.pending_references().iter())
            .map(|reference| reference.name)
            .find(|&name| {
                let text = self.lexer.names.text(name, self.lexer.source.text);
                text.get(0) == Some(u16::from(b'#')) && !self.is_private_name_declared_outside(name)
            });
        match undeclared {
            Some(name) => Err(self.complain(Complaint::UndeclaredPrivateName(name))),
            None => Ok(()),
        }
    }

    fn function_goal(&mut self, function: FunctionSource<'_>) -> Parsed<List<StmtId>> {
        let FunctionSource { kind, flags, .. } = function;
        let mut outer = Context::CHECKED_BEFORE
            | Context::NEW_TARGET
            | Context::SUPER_PROPERTY
            | Context::SUPER_CALL;
        outer.set(Context::STRICT, flags.contains(FunctionFlags::STRICT));
        outer.set(Context::MODULE, self.options.in_module);
        self.scopes.push_function(kind, self.start());
        if kind == FunctionKind::FieldInitializer {
            self.set_context(outer.of_body(kind, flags));
            return self.field_definitions(function.fields);
        }
        self.set_context(outer.of_parameters(kind, flags));
        self.next();
        self.ast.root.start = self.start();
        if kind != FunctionKind::StaticBlock {
            self.ast.root.parameters = self.parameters::<true>(kind, flags)?.list;
        }
        self.context = outer.of_body(kind, flags);
        let own_name = (flags.contains(FunctionFlags::BINDS_OWN_NAME))
            .then(|| (self.lexer.names).intern_text(self.lexer.source, function.own_name));
        let has_scope = self.begin_body(own_name, flags);
        if kind == FunctionKind::Arrow {
            self.expect(TokenKind::Arrow, "Expected an arrow")?;
        }
        let body = if self.at(TokenKind::OpenBrace) {
            let start = self.start();
            let body = self.block_contents::<true>()?;
            self.statement_node::<true>(start, StmtKind::Block { body, scope: None })
        } else {
            self.expression_body::<true>(In::Allowed)?
        };
        if !self.at(TokenKind::End) {
            return Err(Failed);
        }
        if has_scope {
            self.ast.root.body_scope = self.pop_scope::<true>();
        }
        Ok(self.list_of_one(body))
    }

    fn field_definitions(&mut self, fields: &[FieldSource]) -> Parsed<List<StmtId>> {
        self.ast.root.start = self.start();
        let list = self.open_list();
        for (index, field) in (0..).zip(fields) {
            self.lexer.jump(field.key_start);
            let kind = if field.kind == FieldKind::StaticBlock {
                let function = self.static_block_function(field.start)?;
                StmtKind::StaticBlock { function }
            } else {
                self.field_definition(index, field)?
            };
            let statement = self.ast.push_statement(Stmt {
                start: field.start,
                end: self.lexer.previous_end,
                pause: field.kind == FieldKind::StaticBlock,
                kind,
            });
            self.ast.push_to_list(statement);
        }
        Ok(self.ast.close_list(list))
    }

    /// At the key.
    fn field_definition(&mut self, index: u32, field: &FieldSource) -> Parsed<StmtKind> {
        let token = self.token();
        let key = match field.kind {
            FieldKind::Private => {
                self.refer_to_private_name(token);
                PropertyKey::Private(token.name())
            }
            FieldKind::Named if matches!(token.kind, TokenKind::Number | TokenKind::BigInt) => {
                PropertyKey::Named(self.numeric_key::<true>()?)
            }
            FieldKind::Named => PropertyKey::Named(self.word_or_string(token)),
            FieldKind::Computed | FieldKind::StaticBlock => PropertyKey::None,
        };
        if field.initializer_start == 0 {
            self.lexer.previous_end = token.end;
            let value = None;
            return Ok(StmtKind::DefineField { index, key, value });
        }
        self.lexer.jump(field.initializer_start);
        let value = self.assignment::<true>()?;
        self.end_expression::<true>(value);
        if let PropertyKey::Named(name) | PropertyKey::Private(name) = key {
            self.name_anonymous(value.id(), name);
        }
        let value = Some(value.id());
        Ok(StmtKind::DefineField { index, key, value })
    }
}
