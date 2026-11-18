//! Functions: those inside the code, and the one that the code is.

use bun_engine_syntax::ast::{
    Binding, BindingFlags, BindingKind, Features, Function, FunctionId, PatternKind, ScopeFlags,
    ScopeId, StmtKind,
};
use bun_engine_syntax::{EvalContext, FunctionFlags, FunctionKind, Name, NameKind};

use super::control::Exit;
use super::scope::{Layout, Place};
use super::{Dest, Generator, Suspension, This};
use crate::assembler::Label;
use crate::code::{CodeFlags, CodeKind, HandlerKind};
use crate::function::{FunctionInfo, FunctionName, Origin, Visibility};
use crate::operand::{DebugHook, FunctionIndex, Register, StaticError, SuspendKind, Unresolved};
use crate::scope::ScopeKind;

/// How the `arguments` object comes to be.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Arguments {
    None,
    /// Not at all. The register only stands for it.
    OnlyLength(Register),
    Lazy {
        register: Register,
        is_mapped: bool,
    },
    Unmapped,
    Direct(Register),
    Scoped,
}

const NO_REGISTER: Register = Register::from_offset(4);

fn has(binding: &Binding, flag: BindingFlags) -> bool {
    binding.flags.contains(flag)
}

impl<'a> Generator<'a> {
    // Inner functions

    pub(super) fn creation_position(&self, position: u32) -> u32 {
        if self.is_chain_outer() {
            self.created_at
        } else {
            self.closure_position.unwrap_or(position)
        }
    }

    pub(super) fn function_name_of(&mut self, name: Option<Name>) -> FunctionName {
        let (names, name) = (&self.ast.names, name.unwrap_or(Name::EMPTY));
        if Name::known_text(name.index()).is_some() {
            return FunctionName::Known(name);
        }
        match names.source_start(name) {
            Some(start) if names.kind(name) == NameKind::Ordinary => FunctionName::Source {
                start,
                length: names.length(name),
            },
            _ => FunctionName::Owned(self.make_identifier(name)),
        }
    }

    pub(super) fn synthetic_function_info(
        &mut self,
        kind: FunctionKind,
        position: u32,
    ) -> FunctionInfo {
        FunctionInfo {
            name: FunctionName::Known(Name::EMPTY),
            kind,
            flags: FunctionFlags::STRICT,
            origin: self.origin - Origin::DEFAULT_CONSTRUCTOR,
            visibility: match self.visibility {
                Visibility::PrivateRecursive => Visibility::PrivateRecursive,
                _ => Visibility::Public,
            },
            has_own_name: false,
            context: match kind {
                FunctionKind::Arrow => self.context,
                _ => EvalContext::empty(),
            },
            instance_fields: 0,
            text: 0..0,
            start: 0,
            parameters_start: 0,
            end: 0,
            length: 0,
            scope: self.chain.clone(),
            created_at: self.creation_position(position),
            fields: None,
            code: Default::default(),
        }
    }

    pub(super) fn function_info(&mut self, function: &Function, position: u32) -> FunctionInfo {
        FunctionInfo {
            name: self.function_name_of(function.name),
            flags: function.flags - FunctionFlags::HOISTED_AS_VARIABLE,
            has_own_name: function.own_name.is_some(),
            text: function.start..function.end,
            start: function.start,
            parameters_start: function.parameters_start,
            end: function.end,
            length: function.length,
            ..self.synthetic_function_info(function.kind, position)
        }
    }

    pub(super) fn add_function(&mut self, info: FunctionInfo) -> FunctionIndex {
        self.b.functions.push(info);
        FunctionIndex(self.b.functions.len() as u32 - 1)
    }

    pub(super) fn new_function(&mut self, dst: Register, id: FunctionId, position: u32) {
        if !self.asm().is_reachable() && !self.covers {
            return;
        }
        let info = self.function_info(&self.ast[id], position);
        let (function, scope) = (self.add_function(info), self.scope);
        self.asm().new_function(dst, scope, function);
    }

    // The function that is being generated

    pub(super) fn suspends(&self) -> bool {
        self.flags.contains(CodeFlags::SUSPENDS)
    }

    pub(super) fn throw_naming_function(&mut self, error: StaticError) {
        let name = match self.function.filter(|it| it.has_own_name) {
            Some(function) => self.symbol_identifier(function.name(self.text).name()),
            None => self.identifier(Name::EMPTY),
        };
        self.asm().throw_static(error, name);
    }

    pub(super) fn constructor_called_without_new(&mut self) {
        self.asm().enter(0);
        self.throw_naming_function(match self.function_kind {
            FunctionKind::NakedConstructor => StaticError::ConstructorCalledWithoutNew,
            _ => StaticError::ClassCalledWithoutNew,
        });
    }

    fn create_this(&mut self) {
        self.asm().create_this(Register::THIS, Register::THIS, 0);
        if let Some((site, _)) = self.asm().last() {
            let fields = self.function.map_or(0, |it| it.instance_fields);
            self.track_allocation(Register::THIS, site, 2, u32::from(fields));
        }
    }

    pub(super) fn default_constructor(&mut self) {
        self.asm().enter(0);
        if self.function_kind == FunctionKind::BaseConstructor {
            self.create_this();
            self.position_at(10);
            self.initialize_fields(Register::THIS);
        } else {
            let (parent, forwarded) = (self.temp(), self.empty());
            self.asm().get_prototype_of(parent, Register::CALLEE);
            self.position(27, 22, 36);
            (self.asm()).super_construct_varargs(Register::THIS, parent, Register::THIS, forwarded);
            self.initialize_fields(Register::THIS);
        }
        self.asm().ret(Register::THIS);
    }

    fn uses(&self, feature: Features) -> bool {
        self.ast.root.features.contains(feature)
    }

    /// `this` and `new.target` of an arrow function or of `eval` code.
    pub(super) fn inherit_context(&mut self) {
        let this_slot = self.resolve_outer_internal(Name::THIS_VALUE);
        self.this = match this_slot {
            Some(_) if self.context.contains(EvalContext::IN_DERIVED_CONSTRUCTOR) => {
                let (this, empty) = (self.temp(), self.empty());
                self.asm().mov(this, empty);
                This::UnboundInScope(this)
            }
            Some(place) if self.uses(Features::USES_THIS) => {
                let this = self.temp();
                self.load_internal(place, this);
                This::Register(this)
            }
            None if self.kind == CodeKind::Eval && !self.is_in_module() => {
                This::Register(Register::THIS)
            }
            _ => This::Undefined,
        };
        if self.uses(Features::USES_NEW_TARGET) {
            let new_target = self.temp();
            self.load_outer_internal(Name::NEW_TARGET, new_target);
            self.new_target = new_target.into();
        }
    }

    fn establish_this(&mut self, bindings: &[Binding]) {
        let captured = |name| bindings.iter().find(|it| it.name == name);
        if self.function_kind == FunctionKind::Arrow {
            return self.inherit_context();
        }
        let uses_this = self.uses(Features::USES_THIS) || captured(Name::THIS_VALUE).is_some();
        if self.kind != CodeKind::Construct {
            if uses_this && !self.is_strict {
                self.asm().to_this(Register::THIS);
            }
            if uses_this && self.suspends() {
                let this = self.temp();
                self.asm().mov(this, Register::THIS);
                self.this = This::Register(this);
            }
            return;
        }
        if self.uses(Features::USES_NEW_TARGET) || captured(Name::NEW_TARGET).is_some() {
            let new_target = self.temp();
            self.asm().mov(new_target, Register::THIS);
            self.new_target = new_target.into();
        }
        match self.function_kind {
            FunctionKind::NakedConstructor => {}
            FunctionKind::DerivedConstructor => {
                let empty = self.empty();
                self.asm().mov(Register::THIS, empty);
                self.this = match captured(Name::THIS_VALUE) {
                    Some(this) if has(this, BindingFlags::WRITTEN) => {
                        This::UnboundInScope(Register::THIS)
                    }
                    _ => This::Unbound(Register::THIS),
                };
            }
            _ => self.create_this(),
        }
    }

    pub(super) fn initialize_internal(&mut self, binding: &Binding) {
        let mark = self.top;
        let value = match (binding.name, self.this) {
            (Name::THIS_VALUE, This::Register(this)) => this.into(),
            (Name::THIS_VALUE, This::Undefined) => self.undefined(),
            (Name::NEW_TARGET, _) => self.new_target,
            (Name::ACTIVE_FUNCTION, _) => Register::CALLEE.into(),
            (Name::HOME, _) => {
                let home = self.temp();
                self.load_home(home);
                home.into()
            }
            (Name::FIELD_INITIALIZER, _)
                if self.context.contains(EvalContext::HAS_FIELD_INITIALIZER) =>
            {
                let initializer = self.temp();
                self.load_outer_internal(Name::CLASS_FIELD_INITIALIZER, initializer);
                initializer.into()
            }
            _ => return,
        };
        self.initialize(binding.name, value);
        self.top = mark;
    }

    /// The binding that starts out as the `arguments` object.
    fn arguments_binding(&self, bindings: &'a [Binding]) -> Option<&'a Binding> {
        let binding = bindings.iter().find(|it| it.name == Name::ARGUMENTS)?;
        let is_variable = binding.kind == BindingKind::Var
            && has(binding, BindingFlags::USED)
            && !has(binding, BindingFlags::FUNCTION)
            && self.function_kind != FunctionKind::Arrow;
        (is_variable || binding.kind == BindingKind::Arguments).then_some(binding)
    }

    fn plan_arguments(&mut self, bindings: &'a [Binding]) -> Arguments {
        let Some(binding) = self.arguments_binding(bindings) else {
            return Arguments::None;
        };
        let is_mapped = !self.is_strict && self.uses(Features::SIMPLE_PARAMETERS);
        let has_mappings = is_mapped && !self.ast.root.parameters.is_empty();
        let is_only_read = binding.kind == BindingKind::Arguments
            && !has(binding, BindingFlags::CAPTURED)
            && !has(binding, BindingFlags::WRITTEN)
            && !self.suspends();
        let body = self.ast.root.body_scope.map(|it| self.ast.bindings(it));
        let is_redeclared = body.is_some_and(|it| it.iter().any(|it| it.name == Name::ARGUMENTS));
        if is_only_read && !is_redeclared && self.uses(Features::ONLY_ARGUMENTS_LENGTH) {
            Arguments::OnlyLength(NO_REGISTER)
        } else if is_only_read && !has_mappings {
            let (register, empty) = (self.temp(), self.empty());
            self.asm().mov(register, empty);
            Arguments::Lazy {
                register,
                is_mapped,
            }
        } else if !is_mapped {
            Arguments::Unmapped
        } else if has_mappings
            && (self.suspends()
                || (bindings.iter())
                    .any(|it| it.kind == BindingKind::Parameter && has(it, BindingFlags::CAPTURED)))
        {
            Arguments::Scoped
        } else {
            Arguments::Direct(self.temp())
        }
    }

    /// The name of a parameter that has neither a pattern nor a default.
    fn plain_parameter(&self, index: u32) -> Option<Name> {
        let ast = self.ast;
        let element = &ast[ast.at(ast.root.parameters, index)];
        match ast[element.pattern?].kind {
            PatternKind::Binding { name, .. } if element.default.is_none() && !element.is_rest => {
                Some(name)
            }
            _ => None,
        }
    }

    /// How many parameters have a slot in every frame.
    fn count_frame_parameters(&self) -> u32 {
        let ast = self.ast;
        if self.suspends() {
            return 0;
        }
        (ast.iter(ast.root.parameters))
            .take_while(|&it| ast[it].default.is_none() && !ast[it].is_rest)
            .count() as u32
    }

    fn load_argument(&mut self, index: u32, dst: Dest) -> Register {
        if index + 1 < self.parameters {
            let value = self.deliver(dst, Register::argument(index));
            return self.in_register(value);
        }
        let dst = self.final_dst(dst);
        self.asm().get_argument(dst, index);
        dst
    }

    /// Variables that start as undefined come first among the locals.
    pub(super) fn reserve_variables(&mut self, scopes: [Option<ScopeId>; 2]) -> u32 {
        let ast = self.ast;
        let bindings = scopes.into_iter().flatten().flat_map(|it| ast.bindings(it));
        let count = bindings.filter(|it| Self::starts_in_frame(it)).count() as u32;
        self.temps(count);
        count
    }

    pub(super) fn starts_in_frame(binding: &Binding) -> bool {
        binding.kind == BindingKind::Var
            && has(binding, BindingFlags::USED)
            && !has(binding, BindingFlags::CAPTURED)
            && !has(binding, BindingFlags::FUNCTION)
    }

    pub(super) fn function_code(&mut self) {
        let ast = self.ast;
        let root = &ast.root;
        let Some(scope) = root.scope else {
            return;
        };
        let bindings = ast.bindings(scope);
        self.parameters = 1 + self.count_frame_parameters();
        let variables = self.reserve_variables([Some(scope), root.body_scope]);
        self.asm().enter(variables);
        if self.suspends() {
            self.asm().resume_dispatch();
            self.generator = self.temp().into();
        }
        self.establish_this(bindings);
        self.arguments = self.plan_arguments(bindings);

        let mut next_variable = 1;
        let merged_body = self.enter_function_scope(scope, &mut next_variable);
        self.create_arguments(bindings);
        if self.kind == CodeKind::Construct && self.function_kind == FunctionKind::BaseConstructor {
            self.position_at(root.start);
            self.initialize_fields(Register::THIS);
        }
        let mut handler = None;
        if self.suspension == Suspension::AsyncFunction {
            handler = Some(self.begin_async_function());
        }
        if !self.uses(Features::SIMPLE_PARAMETERS) {
            self.bind_parameters();
        }
        if self.function_kind != FunctionKind::FieldInitializer {
            self.cover(root.start, 0);
        }
        if let (Suspension::Generator | Suspension::AsyncGenerator, Some(generator)) =
            (self.suspension, self.generator.as_register())
        {
            handler = Some(self.begin_generator(generator));
        }
        if let Some(body) = ast.get(root.body, 0).filter(|_| self.suspends()) {
            self.cover(ast[body].start, root.end);
        }
        if let Some(body) = root.body_scope {
            self.enter_body_scope(scope, body, merged_body, &mut next_variable);
        }
        let body_scope = root.body_scope.unwrap_or(scope);
        self.instantiate_functions(body_scope);

        self.debug_hook(DebugHook::DidEnterCallFrame, root.start);
        if (self.function.and_then(|it| it.fields.as_deref())).is_some_and(|it| it.sets_brand) {
            self.set_brand();
        }
        self.with_disposal(Some(body_scope), Self::function_body);
        if self.asm().is_reachable() {
            let undefined = self.undefined();
            self.exit(Exit::Return(undefined));
        }
        if let Some(handler) = handler {
            self.end_suspending_code(handler);
        }
    }

    fn function_body(&mut self) {
        let root = &self.ast.root;
        if self.function_kind == FunctionKind::DerivedConstructor
            && !self.covers
            && let Some(block) = self.ast.get(root.body, 0)
            && let StmtKind::Block { body, .. } = self.ast[block].kind
        {
            return self.constructor_body(body);
        }
        self.statements(root.body, None, root.end.saturating_sub(1));
    }

    fn push_base_try(&mut self, kind: HandlerKind) -> Label {
        let handler = self.asm().label();
        self.asm().push_try(handler, kind);
        self.base_try_depth += 1;
        handler
    }

    pub(super) fn begin_async_function(&mut self) -> Label {
        if let Some(generator) = self.generator.as_register() {
            let empty = self.empty();
            self.asm().mov(generator, empty);
        }
        self.push_base_try(HandlerKind::SynthesizedCatch)
    }

    fn begin_generator(&mut self, generator: Register) -> Label {
        if self.suspension == Suspension::Generator {
            self.asm().create_generator(generator);
        } else {
            self.asm().create_async_generator(generator);
        }
        self.suspend(generator.into(), SuspendKind::Start);
        let handler = self.push_base_try(HandlerKind::SynthesizedFinally);
        let (mark, ignored) = (self.top, self.temp());
        self.resume_or_return(ignored);
        self.top = mark;
        handler
    }

    pub(super) fn end_suspending_code(&mut self, handler: Label) {
        self.asm().pop_try();
        let (exception, value) = (self.temp(), self.temp());
        self.asm().bind(handler);
        self.asm().catch(exception, value);
        let generator = self.generator;
        match (self.suspension, generator.as_register()) {
            (Suspension::AsyncFunction, _) | (_, None) => self.asm().async_throw(generator, value),
            (_, Some(generator)) => self.asm().generator_throw(generator, exception),
        }
    }

    // FunctionDeclarationInstantiation

    fn is_shadowed(binding: &Binding, others: impl IntoIterator<Item = &'a Binding>) -> bool {
        matches!(binding.kind, BindingKind::Callee | BindingKind::Arguments)
            && (others.into_iter()).any(|it| it.name == binding.name && it.kind != binding.kind)
    }

    /// Returns the slot from which on the bindings of the body follow each other, if they share the scope.
    fn enter_function_scope(&mut self, id: ScopeId, next_variable: &mut u32) -> Option<Shared> {
        let ast = self.ast;
        let bindings = ast.bindings(id);
        let body = ast.root.body_scope;
        let is_extensible = |it: ScopeId| ast[it].flags.contains(ScopeFlags::EXTENSIBLE);
        let is_body_apart = body.is_some_and(|body| {
            ast[body].flags.contains(ScopeFlags::HIDDEN_FROM_PARAMETERS) || is_extensible(body)
        });
        let shared: &[Binding] = match body {
            Some(body) if !is_body_apart => ast.bindings(body),
            _ => &[],
        };

        let mut callee = bindings.iter().find(|it| it.kind == BindingKind::Callee);
        if is_extensible(id) {
            if let Some(callee) = callee.take() {
                let (register, mut layout) = (self.temp(), Layout::new(ScopeKind::FunctionName));
                let slot = self.add_slot(&mut layout, callee);
                self.create_scope(layout, register);
                self.asm().put_scoped(register, slot, Register::CALLEE);
                self.bind_slot(callee, register, slot);
            }
            self.raise_barrier();
            if body.is_some() {
                let (register, mut layout) = (self.temp(), Layout::new(ScopeKind::Function));
                layout.make_extensible();
                self.create_scope(layout, register);
            }
        }

        let captures = |it: ScopeId| ast[it].flags.contains(ScopeFlags::HAS_CAPTURED);
        let needs_scope = captures(id)
            || self.arguments == Arguments::Scoped
            || (body.is_none() && is_extensible(id))
            || (!is_body_apart && body.is_some_and(captures));
        let register = if needs_scope {
            self.temp()
        } else {
            NO_REGISTER
        };
        let mut layout = Layout::new(if is_body_apart {
            ScopeKind::Parameters
        } else {
            ScopeKind::Function
        });
        let is_simple = self.uses(Features::SIMPLE_PARAMETERS);
        if self.arguments == Arguments::Scoped {
            self.lay_out_mapped_parameters(bindings, &mut layout, register);
        }
        let is_parameter = |it: &Binding| it.kind == BindingKind::Parameter;
        let in_slot = |it: &Binding| {
            has(it, BindingFlags::CAPTURED)
                || (is_parameter(it)
                    && (shared.iter()).any(|redeclared| {
                        redeclared.name == it.name && has(redeclared, BindingFlags::CAPTURED)
                    }))
        };
        for binding in bindings {
            if binding.kind == BindingKind::Var && has(binding, BindingFlags::CAPTURED) {
                let slot = self.add_slot(&mut layout, binding);
                self.bind_slot(binding, register, slot);
            }
        }
        let has_parameter = |name| {
            bindings
                .iter()
                .any(|it| is_parameter(it) && it.name == name)
        };
        let first_variable = layout.slot_count();
        for binding in shared {
            if binding.kind == BindingKind::Var
                && has(binding, BindingFlags::CAPTURED)
                && !has_parameter(binding.name)
            {
                self.add_slot(&mut layout, &Self::as_initialized(binding));
            }
        }
        layout.end_undefined_slots();
        let first_lexical = layout.slot_count();
        for binding in shared {
            if binding.kind != BindingKind::Var && has(binding, BindingFlags::CAPTURED) {
                self.add_slot(&mut layout, binding);
            }
        }

        for binding in bindings {
            match binding.kind {
                BindingKind::Var if has(binding, BindingFlags::CAPTURED) => {}
                BindingKind::Var => self.bind_frame_variable(binding, next_variable),
                BindingKind::Callee if callee.is_none() => {}
                BindingKind::Parameter if self.arguments == Arguments::Scoped => {}
                _ if in_slot(binding)
                    && !Self::is_shadowed(binding, bindings.iter().chain(shared)) =>
                {
                    let mut binding = *binding;
                    if let Some(redeclared) = shared.iter().find(|it| it.name == binding.name) {
                        binding.flags |= redeclared.flags;
                        if has(redeclared, BindingFlags::FUNCTION) {
                            binding.flags |= BindingFlags::WRITTEN;
                        }
                    }
                    let slot = self.add_slot(&mut layout, &binding);
                    self.bind_slot(&binding, register, slot);
                }
                BindingKind::Callee => self.bind(binding, Place::Register(Register::CALLEE)),
                BindingKind::Parameter => {}
                BindingKind::Arguments => match self.arguments {
                    Arguments::Lazy { register, .. } | Arguments::OnlyLength(register) => {
                        self.bind(binding, Place::Register(register));
                    }
                    _ => {
                        self.bind_register(binding);
                    }
                },
                _ => {
                    self.bind_register(binding);
                }
            }
        }
        if body.is_none() && is_extensible(id) {
            layout.make_extensible();
        }
        if needs_scope {
            self.create_scope(layout, register);
        }
        self.bind_frame_parameters(bindings, shared, is_simple, in_slot);
        for binding in bindings {
            match binding.kind {
                BindingKind::Internal => self.initialize_internal(binding),
                BindingKind::Callee if callee.is_some() => {
                    if let place @ Place::Slot { .. } = self.place_below_lexicals(binding.name) {
                        self.put(place, binding.name, None, Register::CALLEE.into());
                    }
                }
                _ => {}
            }
        }
        (!is_body_apart).then_some(Shared {
            register,
            first_variable,
            first_lexical,
        })
    }

    /// For a variable that may get a value before the body runs.
    fn as_initialized(binding: &Binding) -> Binding {
        let mut binding = *binding;
        if binding.name == Name::ARGUMENTS && has(&binding, BindingFlags::WRITTEN) {
            binding.flags |= BindingFlags::REWRITTEN;
        }
        binding
    }

    pub(super) fn bind_frame_variable(&mut self, binding: &Binding, next_variable: &mut u32) {
        if Self::starts_in_frame(binding) {
            self.bind(binding, Place::Register(Register::local(*next_variable)));
            *next_variable += 1;
        } else {
            self.bind_register(binding);
        }
    }

    fn lay_out_mapped_parameters(
        &mut self,
        bindings: &[Binding],
        layout: &mut Layout,
        register: Register,
    ) {
        let count = self.ast.root.parameters.len();
        for index in 0..count {
            let name = self.plain_parameter(index);
            let is_last = (index + 1..count).all(|later| self.plain_parameter(later) != name);
            match bindings.iter().find(|it| Some(it.name) == name) {
                Some(binding) if is_last => {
                    let mut binding = *binding;
                    binding.flags |= BindingFlags::WRITTEN | BindingFlags::REWRITTEN;
                    let slot = self.add_slot(layout, &binding);
                    self.bind_slot(&binding, register, slot);
                }
                _ => {
                    layout.anonymous_slot();
                }
            }
        }
        layout.end_mapped_parameters();
    }

    /// Gives the parameters that are in no scope a place, and those of a simple list their values.
    fn bind_frame_parameters(
        &mut self,
        bindings: &[Binding],
        shared: &[Binding],
        is_simple: bool,
        in_slot: impl Fn(&Binding) -> bool,
    ) {
        let count = self.ast.root.parameters.len();
        if self.arguments == Arguments::Scoped {
            let (mark, scope) = (self.top, self.scope);
            for index in 0..count {
                let value = self.load_argument(index, Dest::Any);
                self.asm().put_scoped(scope, index, value);
                self.top = mark;
            }
            return;
        }
        let keeps_frame_intact = matches!(self.arguments, Arguments::Lazy { .. });
        let names = bindings
            .iter()
            .filter(|it| it.kind == BindingKind::Parameter);
        let has_duplicates = is_simple && names.count() != count as usize;
        let mut index = 0;
        for binding in bindings {
            if binding.kind != BindingKind::Parameter {
                continue;
            }
            let is_named = |&it: &u32| self.plain_parameter(it) == Some(binding.name);
            let position = if has_duplicates {
                (0..count).rfind(is_named)
            } else {
                (index..count).find(is_named)
            };
            index = position.map_or(index, |it| it + 1);
            let changes = |it: &Binding| {
                it.name == binding.name
                    && (it.flags).intersects(BindingFlags::WRITTEN | BindingFlags::FUNCTION)
            };
            let is_changed = changes(binding) || shared.iter().any(changes);
            if in_slot(binding) {
                if let Some(position) = position
                    && is_simple
                    && !has(binding, BindingFlags::FUNCTION)
                {
                    let mark = self.top;
                    let value = self.load_argument(position, Dest::Any);
                    self.initialize(binding.name, value.into());
                    self.top = mark;
                }
            } else if let (Arguments::Direct(arguments), Some(index)) = (self.arguments, position) {
                self.bind(binding, Place::Argument { arguments, index });
            } else if let Some(position) = position
                && position + 1 < self.parameters
                && !(keeps_frame_intact && is_changed)
                && !has(binding, BindingFlags::USED_EARLY)
            {
                self.bind(binding, Place::Register(Register::argument(position)));
            } else {
                let register = self.bind_register(binding);
                if let Some(position) = position
                    && is_simple
                    && has(binding, BindingFlags::USED)
                    && !has(binding, BindingFlags::FUNCTION)
                {
                    self.load_argument(position, Dest::Local(register));
                }
            }
        }
    }

    fn create_arguments(&mut self, bindings: &'a [Binding]) {
        let Some(binding) = self.arguments_binding(bindings) else {
            return;
        };
        let (mark, place) = (self.top, self.place_below_lexicals(Name::ARGUMENTS));
        let dst = match place {
            Place::Register(register) => register,
            _ => self.temp(),
        };
        let object = match self.arguments {
            Arguments::None | Arguments::OnlyLength(_) | Arguments::Lazy { .. } => return,
            Arguments::Direct(object) => {
                self.asm().create_direct_arguments(object);
                object
            }
            Arguments::Unmapped => {
                self.asm().create_unmapped_arguments(dst);
                dst
            }
            Arguments::Scoped => {
                let scope = self.scope;
                self.asm().create_scoped_arguments(dst, scope);
                dst
            }
        };
        self.put(place, binding.name, None, object.into());
        self.top = mark;
    }

    fn bind_parameters(&mut self) {
        let ast = self.ast;
        for (index, element) in (0..).zip(ast.iter(ast.root.parameters)) {
            let element = &ast[element];
            let Some(pattern) = element.pattern else {
                continue;
            };
            if element.is_rest {
                self.bind_rest(pattern, index);
                continue;
            }
            let mark = self.top;
            let Some(default) = element.default else {
                let dst = self.resolve_pattern_register(pattern);
                let value = self.load_argument(index, dst.map_or(Dest::Any, Dest::Local));
                self.bind_pattern(pattern, value.into());
                self.top = mark;
                continue;
            };
            let value = match ast[pattern].kind {
                PatternKind::Binding { name, .. } if !self.is_used_early(name) => {
                    self.resolve_pattern_register(pattern)
                }
                _ => None,
            };
            let value = value.unwrap_or_else(|| self.temp());
            self.asm().get_argument(value, index);
            self.apply_default(value, default);
            self.bind_pattern(pattern, value.into());
            self.top = mark;
        }
    }

    fn is_used_early(&self, name: Name) -> bool {
        let bindings = self
            .ast
            .root
            .scope
            .map_or(&[][..], |it| self.ast.bindings(it));
        (bindings.iter()).any(|it| it.name == name && has(it, BindingFlags::USED_EARLY))
    }

    fn enter_body_scope(
        &mut self,
        parameters: ScopeId,
        id: ScopeId,
        shared: Option<Shared>,
        next_variable: &mut u32,
    ) {
        let ast = self.ast;
        let bindings = ast.bindings(id);
        let outer_kind = |name| {
            let outer = ast.bindings(parameters).iter().find(|it| it.name == name);
            outer.map(|it| it.kind)
        };
        let starts_with_outer_value = |it: &Binding| {
            it.kind == BindingKind::Var
                && !has(it, BindingFlags::FUNCTION)
                && matches!(
                    outer_kind(it.name),
                    Some(BindingKind::Parameter | BindingKind::Arguments)
                )
        };
        let mark = self.top;
        let copied = |it: &&Binding| {
            starts_with_outer_value(it)
                && (shared.is_none() || outer_kind(it.name) == Some(BindingKind::Arguments))
        };
        for binding in bindings.iter().filter(copied) {
            let variable = self.resolve_for_use(binding.name, ast[id].start);
            let value = self.temp();
            self.load(&variable, None, Dest::Temp(value), Unresolved::Throw);
        }

        let is_extensible = ast[id].flags.contains(ScopeFlags::EXTENSIBLE);
        if is_extensible {
            self.raise_barrier();
        }
        if let Some(mut shared) = shared {
            for binding in bindings {
                let is_variable = binding.kind == BindingKind::Var;
                if is_variable && outer_kind(binding.name) == Some(BindingKind::Parameter) {
                    *next_variable += u32::from(Self::starts_in_frame(binding));
                } else if !has(binding, BindingFlags::CAPTURED) {
                    self.bind_frame_variable(binding, next_variable);
                } else {
                    let slot = if is_variable {
                        &mut shared.first_variable
                    } else {
                        &mut shared.first_lexical
                    };
                    self.bind_slot(binding, shared.register, *slot);
                    *slot += 1;
                }
            }
        } else {
            let (register, mut layout) = (self.temp(), Layout::new(ScopeKind::Function));
            for is_variable in [true, false] {
                for binding in bindings {
                    if has(binding, BindingFlags::CAPTURED)
                        && (binding.kind == BindingKind::Var) == is_variable
                    {
                        let mut binding = *binding;
                        if starts_with_outer_value(&binding) && has(&binding, BindingFlags::WRITTEN)
                        {
                            binding.flags |= BindingFlags::REWRITTEN;
                        }
                        let slot = self.add_slot(&mut layout, &binding);
                        self.bind_slot(&binding, register, slot);
                    }
                }
                if is_variable {
                    layout.end_undefined_slots();
                }
            }
            if is_extensible {
                layout.make_extensible();
            }
            if !layout.is_empty() {
                self.create_scope(layout, register);
            }
            for binding in bindings {
                if !has(binding, BindingFlags::CAPTURED) {
                    self.bind_frame_variable(binding, next_variable);
                }
            }
        }
        for (index, binding) in (mark..).zip(bindings.iter().filter(copied)) {
            self.initialize(binding.name, Register::local(index).into());
        }
    }
}

/// Where the bindings of the body are in the scope of the parameters.
#[derive(Clone, Copy)]
struct Shared {
    register: Register,
    first_variable: u32,
    first_lexical: u32,
}
