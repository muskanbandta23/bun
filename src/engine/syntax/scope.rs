//! Finds the binding of each name while the parser goes along.
//!
//! References and declarations are kept on stacks, a range of each for every open scope, and the entries for one
//! name are chained from the innermost outwards. When a scope closes it takes the references to what it declares and
//! leaves the others to its parent.

use crate::ast::{
    Ast, Binding, BindingFlags, BindingKind, FunctionId, List, Scope, ScopeFlags, ScopeId,
    ScopeKind,
};
use crate::names::Name;
use crate::options::{FunctionFlags, FunctionKind};

bitflags::bitflags! {
    #[derive(Clone, Copy, PartialEq, Eq, Default)]
    pub(crate) struct Access: u8 {
        const CAPTURED = 1 << 0;
        const WRITTEN = 1 << 1;
        const REWRITTEN = 1 << 2;
    }
}

impl Access {
    #[inline(always)]
    fn merged(self, other: Access) -> Access {
        if self.intersection(other).contains(Access::WRITTEN) {
            self | other | Access::REWRITTEN
        } else {
            self | other
        }
    }

    /// As seen from outside of code that can run many times.
    #[inline(always)]
    fn repeated(self) -> Access {
        if self.contains(Access::WRITTEN) {
            self | Access::REWRITTEN
        } else {
            self
        }
    }
}

/// An index plus one, or zero.
type Link = u32;

#[derive(Clone, Copy)]
pub(crate) struct Reference {
    pub(crate) name: Name,
    pub(crate) access: Access,
    /// Where the first one is that no closure makes.
    first: u32,
    outer: Link,
}

#[derive(Clone, Copy)]
struct Declaration {
    binding: Binding,
    /// Where the last `var` for it is.
    position: u32,
    outer: Link,
}

#[derive(Clone, Copy, Default)]
struct Heads {
    reference: Link,
    lexical: Link,
    variable: Link,
    is_exported: bool,
    is_marked: bool,
}

/// A function in a block that may get a variable too.
#[derive(Clone, Copy)]
struct Candidate {
    function: FunctionId,
    name: Name,
    has_left_its_block: bool,
}

#[derive(Clone, Copy)]
struct Frame {
    kind: ScopeKind,
    flags: ScopeFlags,
    /// Of a scope of kind `Function`, unless it is the top level of `eval` code.
    function: Option<FunctionKind>,
    is_catch_block: bool,
    start: u32,
    loop_depth: u32,
    references: u32,
    lexicals: u32,
    variables: u32,
    functions: u32,
    candidates: u32,
    /// The index of the frame that `var` declares in.
    variable_frame: u32,
    outer_floor: u32,
}

/// What to go back to when a guess about the syntax was wrong.
#[derive(Clone, Copy)]
pub(crate) struct Mark {
    frames: u32,
    references: u32,
    floor: u32,
    loop_depth: u32,
}

/// See [`Scopes::begin_tentative`].
#[derive(Clone, Copy)]
pub(crate) struct Tentative {
    outer_floor: u32,
    start: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PrivateDeclaration {
    Valid,
    Duplicate,
    /// One accessor of a pair is static and the other is not.
    StaticMismatch,
}

/// What is known of a function once it has been parsed.
#[derive(Clone, Copy)]
pub(crate) struct Closed {
    pub(crate) scope: Option<ScopeId>,
    pub(crate) contains_eval: bool,
    /// An arrow function or a direct `eval` inside needs the home object.
    pub(crate) captures_home: bool,
}

#[derive(Default)]
pub(crate) struct Scopes {
    frames: Vec<Frame>,
    heads: Vec<Heads>,
    references: Vec<Reference>,
    lexicals: Vec<Declaration>,
    variables: Vec<Declaration>,
    functions: Vec<FunctionId>,
    candidates: Vec<Candidate>,
    /// A reference joins an earlier one to the same name from this index on.
    floor: u32,
    loop_depth: u32,
    capture_everything: bool,
}

impl Scopes {
    pub(crate) fn reset(&mut self, capture_everything: bool) {
        self.frames.clear();
        self.heads.clear();
        self.references.clear();
        self.lexicals.clear();
        self.variables.clear();
        self.functions.clear();
        self.candidates.clear();
        self.floor = 0;
        self.loop_depth = 0;
        self.capture_everything = capture_everything;
    }

    #[inline(always)]
    fn heads(&mut self, name: Name) -> &mut Heads {
        if name.index() >= self.heads.len() {
            self.grow_heads(name);
        }
        &mut self.heads[name.index()]
    }

    #[cold]
    fn grow_heads(&mut self, name: Name) {
        self.heads
            .resize(name.index() * 3 / 2 + 32, Heads::default());
    }

    #[inline(always)]
    fn frame(&self) -> &Frame {
        &self.frames[self.frames.len() - 1]
    }

    #[inline(always)]
    fn frame_mut(&mut self) -> &mut Frame {
        let last = self.frames.len() - 1;
        &mut self.frames[last]
    }

    #[inline]
    pub(crate) fn kind(&self) -> ScopeKind {
        self.frame().kind
    }

    /// How many scopes are open.
    pub(crate) fn depth(&self) -> usize {
        self.frames.len()
    }

    /// Whether the innermost scope is the one that `var` declares in.
    #[inline]
    pub(crate) fn is_at_top_level(&self) -> bool {
        self.frame().variable_frame as usize == self.frames.len() - 1
    }

    #[inline]
    pub(crate) fn push(&mut self, kind: ScopeKind, start: u32) {
        let index = self.frames.len() as u32;
        let variable_frame = match kind {
            ScopeKind::Script
            | ScopeKind::Module
            | ScopeKind::Function
            | ScopeKind::FunctionBody => index,
            _ => self.frame().variable_frame,
        };
        let references = self.references.len() as u32;
        self.frames.push(Frame {
            kind,
            flags: ScopeFlags::empty(),
            function: None,
            is_catch_block: false,
            start,
            loop_depth: self.loop_depth,
            references,
            lexicals: self.lexicals.len() as u32,
            variables: self.variables.len() as u32,
            functions: self.functions.len() as u32,
            candidates: self.candidates.len() as u32,
            variable_frame,
            outer_floor: self.floor,
        });
        self.floor = references;
    }

    #[inline]
    pub(crate) fn push_function(&mut self, kind: FunctionKind, start: u32) {
        self.push(ScopeKind::Function, start);
        self.frame_mut().function = Some(kind);
    }

    /// The block after `catch (..)`, which may not declare the parameters again.
    pub(crate) fn push_catch_block(&mut self, start: u32) {
        self.push(ScopeKind::Block, start);
        self.frame_mut().is_catch_block = true;
    }

    #[inline(always)]
    pub(crate) fn enter_loop(&mut self) {
        self.loop_depth += 1;
    }

    #[inline(always)]
    pub(crate) fn leave_loop(&mut self) {
        self.loop_depth -= 1;
    }

    // References

    #[inline(always)]
    fn refer(&mut self, name: Name, access: Access, first: u32) {
        let (floor, next) = (self.floor, self.references.len() as u32 + 1);
        let heads = self.heads(name);
        let outer = heads.reference;
        if outer > floor {
            let reference = &mut self.references[outer as usize - 1];
            reference.access = reference.access.merged(access);
            reference.first = reference.first.min(first);
        } else {
            heads.reference = next;
            self.references.push(Reference {
                name,
                access,
                first,
                outer,
            });
        }
    }

    #[inline(always)]
    pub(crate) fn read(&mut self, name: Name, position: u32) {
        self.refer(name, Access::empty(), position);
    }

    #[inline]
    pub(crate) fn write(&mut self, name: Name, position: u32) {
        let access = if self.loop_depth > self.frame().loop_depth {
            Access::WRITTEN | Access::REWRITTEN
        } else {
            Access::WRITTEN
        };
        self.refer(name, access, position);
    }

    /// For the target of a loop, and whatever else is assigned again and again.
    pub(crate) fn rewrite(&mut self, name: Name, position: u32) {
        self.refer(name, Access::WRITTEN | Access::REWRITTEN, position);
    }

    /// A reference that an inner function makes.
    #[inline]
    pub(crate) fn capture(&mut self, name: Name, access: Access) {
        self.refer(name, access.repeated() | Access::CAPTURED, u32::MAX);
    }

    pub(crate) fn note_direct_eval(&mut self, is_strict: bool) {
        self.note_contains_eval();
        if !is_strict {
            let variable_frame = self.frame().variable_frame as usize;
            self.frames[variable_frame].flags |= ScopeFlags::EXTENSIBLE;
        }
        self.capture_context();
    }

    /// What code that is evaluated here later on may ask for without a declaration.
    pub(crate) fn capture_context(&mut self) {
        self.capture(Name::THIS_VALUE, Access::WRITTEN);
        for name in [
            Name::NEW_TARGET,
            Name::HOME,
            Name::ACTIVE_FUNCTION,
            Name::FIELD_INITIALIZER,
            Name::ARGUMENTS,
        ] {
            self.capture(name, Access::empty());
        }
    }

    /// For an inner function that has a direct `eval` and is not parsed again.
    pub(crate) fn note_contains_eval(&mut self) {
        self.frame_mut().flags |= ScopeFlags::CONTAINS_EVAL;
    }

    /// Whether a closure has been seen in the innermost scope.
    pub(crate) fn has_captures(&self) -> bool {
        let frame = self.frame();
        frame.flags.contains(ScopeFlags::CONTAINS_EVAL)
            || self.references[frame.references as usize..]
                .iter()
                .any(|reference| reference.access.contains(Access::CAPTURED))
    }

    pub(crate) fn contains_eval(&self) -> bool {
        self.frame().flags.contains(ScopeFlags::CONTAINS_EVAL)
    }

    /// Those of the innermost scope that no inner one has taken.
    pub(crate) fn pending_references(&self) -> &[Reference] {
        &self.references[self.frame().references as usize..]
    }

    /// Keeps the references that follow apart, so that all trace of them can be dropped.
    #[inline(always)]
    pub(crate) fn begin_tentative(&mut self) -> Tentative {
        let start = self.references.len() as u32;
        Tentative {
            outer_floor: core::mem::replace(&mut self.floor, start),
            start,
        }
    }

    #[inline]
    pub(crate) fn commit_tentative(&mut self, tentative: Tentative) {
        self.floor = tentative.outer_floor;
        self.hand_up(tentative.start, |reference| reference);
    }

    /// The code has turned out to run many times.
    pub(crate) fn commit_tentative_as_repeated(&mut self, tentative: Tentative) {
        self.floor = tentative.outer_floor;
        self.hand_up(tentative.start, |reference| Reference {
            access: reference.access.repeated(),
            ..reference
        });
    }

    fn drop_references(&mut self, start: u32) {
        for reference in self.references.drain(start as usize..).rev() {
            self.heads[reference.name.index()].reference = reference.outer;
        }
    }

    /// Joins the references from `start` on, no two of which have the same name, with those above `floor`.
    #[inline(always)]
    fn hand_up(&mut self, start: u32, map: impl Fn(Reference) -> Reference) {
        let mut kept = start as usize;
        for index in start as usize..self.references.len() {
            let reference = map(self.references[index]);
            if reference.outer > self.floor {
                let outer = &mut self.references[reference.outer as usize - 1];
                outer.access = outer.access.merged(reference.access);
                outer.first = outer.first.min(reference.first);
                self.heads[reference.name.index()].reference = reference.outer;
            } else {
                self.references[kept] = reference;
                kept += 1;
                self.heads[reference.name.index()].reference = kept as Link;
            }
        }
        self.references.truncate(kept);
    }

    #[inline(always)]
    pub(crate) fn mark(&self) -> Mark {
        Mark {
            frames: self.frames.len() as u32,
            references: self.references.len() as u32,
            floor: self.floor,
            loop_depth: self.loop_depth,
        }
    }

    pub(crate) fn rewind(&mut self, mark: Mark) {
        if let Some(&frame) = self.frames.get(mark.frames as usize) {
            self.drop_lexicals(frame.lexicals);
            self.drop_variables(frame.variables);
            self.functions.truncate(frame.functions as usize);
            self.candidates.truncate(frame.candidates as usize);
            self.frames.truncate(mark.frames as usize);
        }
        self.drop_references(mark.references.min(self.references.len() as u32));
        self.floor = mark.floor;
        self.loop_depth = mark.loop_depth;
    }

    // Declarations

    fn declaration(name: Name, kind: BindingKind, position: u32, outer: Link) -> Declaration {
        Declaration {
            binding: Binding {
                name,
                kind,
                flags: BindingFlags::empty(),
                initialized_from: 0,
                initialized_until: u32::MAX,
            },
            position,
            outer,
        }
    }

    fn add_lexical(&mut self, name: Name, kind: BindingKind) -> &mut Binding {
        let next = self.lexicals.len() as Link + 1;
        let outer = core::mem::replace(&mut self.heads(name).lexical, next);
        self.lexicals.push(Self::declaration(name, kind, 0, outer));
        &mut self.lexicals[next as usize - 1].binding
    }

    fn add_variable(&mut self, name: Name, kind: BindingKind, position: u32) -> &mut Binding {
        let next = self.variables.len() as Link + 1;
        let outer = core::mem::replace(&mut self.heads(name).variable, next);
        self.variables
            .push(Self::declaration(name, kind, position, outer));
        &mut self.variables[next as usize - 1].binding
    }

    /// In the innermost scope.
    fn own_lexical(&mut self, name: Name) -> Option<&mut Binding> {
        let link = self.heads(name).lexical;
        (link > self.frame().lexicals).then(|| &mut self.lexicals[link as usize - 1].binding)
    }

    /// In the scope that `var` declares in.
    fn own_variable(&mut self, name: Name) -> Option<&mut Declaration> {
        let link = self.heads(name).variable;
        let start = self.frames[self.frame().variable_frame as usize].variables;
        (link > start).then(|| &mut self.variables[link as usize - 1])
    }

    fn is_parameter(&mut self, name: Name) -> bool {
        let mut frame = self.frame().variable_frame as usize;
        if self.frames[frame].kind == ScopeKind::FunctionBody {
            frame -= 1;
        }
        let mut link = self.heads(name).variable;
        while link > self.frames[frame].variables {
            let declaration = &self.variables[link as usize - 1];
            if declaration.binding.kind == BindingKind::Parameter {
                return true;
            }
            link = declaration.outer;
        }
        false
    }

    fn declare_in_variable_scope(&mut self, name: Name, position: u32) -> &mut Binding {
        if self.own_variable(name).is_none() {
            return self.add_variable(name, BindingKind::Var, position);
        }
        let link = self.heads[name.index()].variable;
        let declaration = &mut self.variables[link as usize - 1];
        declaration.position = position;
        if declaration.binding.kind == BindingKind::Callee {
            declaration.binding.kind = BindingKind::Var;
        }
        &mut declaration.binding
    }

    /// False if a lexical binding is in the way.
    pub(crate) fn declare_variable(&mut self, name: Name, position: u32) -> bool {
        let variable_frame = self.frame().variable_frame as usize;
        let mut link = self.heads(name).lexical;
        let mut frame = self.frames.len() - 1;
        while link > self.frames[variable_frame].lexicals {
            while link <= self.frames[frame].lexicals {
                frame -= 1;
            }
            if self.frames[frame].kind != ScopeKind::SimpleCatch {
                return false;
            }
            link = self.lexicals[link as usize - 1].outer;
        }
        self.declare_in_variable_scope(name, position);
        true
    }

    /// Whether a lexical declaration here clashes with a `var` or with a parameter of the function or of `catch`.
    fn clashes_with_outer_declaration(&mut self, name: Name) -> bool {
        let frame = *self.frame();
        if frame.is_catch_block {
            let mut link = self.heads(name).lexical;
            if link > frame.lexicals {
                link = self.lexicals[link as usize - 1].outer;
            }
            if link > self.frames[self.frames.len() - 2].lexicals {
                return true;
            }
        }
        if self.is_at_top_level() {
            return self.is_parameter(name)
                || self
                    .own_variable(name)
                    .is_some_and(|variable| variable.binding.kind == BindingKind::Var);
        }
        self.own_variable(name)
            .is_some_and(|variable| variable.position >= frame.start)
    }

    /// False if the name is taken. It counts as not initialized until [`Scopes::initialize_lexicals`].
    pub(crate) fn declare_lexical(&mut self, name: Name, kind: BindingKind) -> bool {
        if self.clashes_with_outer_declaration(name) || self.own_lexical(name).is_some() {
            return false;
        }
        self.add_lexical(name, kind).initialized_from = u32::MAX;
        match kind {
            BindingKind::Using => self.frame_mut().flags |= ScopeFlags::HAS_USING,
            BindingKind::AwaitUsing => {
                self.frame_mut().flags |= ScopeFlags::HAS_USING | ScopeFlags::HAS_AWAIT_USING;
            }
            _ => {}
        }
        true
    }

    /// False if the name is taken.
    pub(crate) fn declare_function(&mut self, name: Name, is_plain: bool, is_strict: bool) -> bool {
        if self.is_at_top_level() && self.kind() != ScopeKind::Module {
            self.declare_in_variable_scope(name, 0).flags |= BindingFlags::FUNCTION;
            return self.own_lexical(name).is_none();
        }
        if self.clashes_with_outer_declaration(name) {
            return false;
        }
        if let Some(earlier) = self.own_lexical(name) {
            return !is_strict && is_plain && earlier.flags.contains(BindingFlags::PLAIN_FUNCTION);
        }
        self.add_lexical(name, BindingKind::LexicalFunction)
            .flags
            .set(BindingFlags::PLAIN_FUNCTION, is_plain);
        true
    }

    /// The node of what [`Scopes::declare_function`] has just declared.
    pub(crate) fn add_function(&mut self, function: FunctionId, name: Name, may_hoist: bool) {
        self.functions.push(function);
        if may_hoist && !self.is_at_top_level() {
            self.candidates.push(Candidate {
                function,
                name,
                has_left_its_block: false,
            });
        }
    }

    /// False if there is one of that name already.
    pub(crate) fn declare_parameter(&mut self, name: Name) -> bool {
        let is_new = self.own_variable(name).is_none();
        if is_new {
            self.add_variable(name, BindingKind::Parameter, 0);
        }
        is_new
    }

    /// After the parameters, which hide it, as the `arguments` object does.
    pub(crate) fn declare_callee(&mut self, name: Name) {
        if name != Name::ARGUMENTS && self.own_variable(name).is_none() {
            self.add_variable(name, BindingKind::Callee, 0);
        }
    }

    /// A name of the engine in the scope of a class.
    pub(crate) fn declare_class_internal(&mut self, name: Name) {
        self.add_lexical(name, BindingKind::Internal).flags =
            BindingFlags::USED | BindingFlags::CAPTURED;
    }

    /// `accessor` is `HAS_GETTER` or `HAS_SETTER` for a `PrivateAccessor`.
    pub(crate) fn declare_private(
        &mut self,
        name: Name,
        kind: BindingKind,
        is_static: bool,
        accessor: BindingFlags,
    ) -> PrivateDeclaration {
        let Some(earlier) = self.own_lexical(name) else {
            let binding = self.add_lexical(name, kind);
            binding.flags = BindingFlags::USED | BindingFlags::CAPTURED | accessor;
            binding.flags.set(BindingFlags::STATIC, is_static);
            return PrivateDeclaration::Valid;
        };
        if kind != BindingKind::PrivateAccessor
            || earlier.kind != kind
            || earlier.flags.contains(accessor)
        {
            PrivateDeclaration::Duplicate
        } else if earlier.flags.contains(BindingFlags::STATIC) != is_static {
            PrivateDeclaration::StaticMismatch
        } else {
            earlier.flags |= accessor;
            PrivateDeclaration::Valid
        }
    }

    #[inline(always)]
    pub(crate) fn lexical_count(&self) -> u32 {
        self.lexicals.len() as u32
    }

    #[inline(always)]
    pub(crate) fn variable_count(&self) -> u32 {
        self.variables.len() as u32
    }

    /// Those declared since there were `count` are initialized from `position` on.
    #[inline]
    pub(crate) fn initialize_lexicals(&mut self, count: u32, position: u32) {
        for declaration in &mut self.lexicals[count as usize..] {
            declaration.binding.initialized_from = position;
        }
    }

    pub(crate) fn initialize_parameters(&mut self, count: u32, position: u32) {
        for declaration in &mut self.variables[count as usize..] {
            declaration.binding.initialized_from = position;
        }
    }

    /// The last lexical declaration has no initializer.
    pub(crate) fn note_starts_undefined(&mut self) {
        if let Some(declaration) = self.lexicals.last_mut() {
            declaration.binding.flags |= BindingFlags::STARTS_UNDEFINED;
        }
    }

    /// A jump to a later clause of the `switch` passes by what has been declared since there were `count`.
    pub(crate) fn end_clause(&mut self, count: u32, position: u32) {
        for declaration in &mut self.lexicals[count as usize..] {
            if declaration.binding.kind != BindingKind::LexicalFunction {
                declaration.binding.initialized_until = position;
            }
        }
    }

    /// False if the module exports something by that name already.
    pub(crate) fn export_name(&mut self, name: Name) -> bool {
        !core::mem::replace(&mut self.heads(name).is_exported, true)
    }

    /// False if it is marked already.
    pub(crate) fn mark_name(&mut self, name: Name) -> bool {
        !core::mem::replace(&mut self.heads(name).is_marked, true)
    }

    pub(crate) fn unmark_name(&mut self, name: Name) {
        self.heads(name).is_marked = false;
    }

    /// For `eval` code that has turned out to be strict.
    pub(crate) fn keep_variables_in_root(&mut self) {
        self.frame_mut().kind = ScopeKind::Function;
    }

    /// False if the top level of the module declares no such thing.
    pub(crate) fn export_binding(&mut self, name: Name) -> bool {
        let binding = match self.own_variable(name) {
            Some(variable) => &mut variable.binding,
            None => match self.own_lexical(name) {
                Some(lexical) => lexical,
                None => return false,
            },
        };
        binding.flags |= BindingFlags::EXPORTED | BindingFlags::USED | BindingFlags::CAPTURED;
        true
    }

    // Closing scopes

    fn drop_lexicals(&mut self, start: u32) {
        for declaration in self.lexicals.drain(start as usize..).rev() {
            self.heads[declaration.binding.name.index()].lexical = declaration.outer;
        }
    }

    fn drop_variables(&mut self, start: u32) {
        for declaration in self.variables.drain(start as usize..).rev() {
            self.heads[declaration.binding.name.index()].variable = declaration.outer;
        }
    }

    /// Closes the innermost scope, which is no function. Without a tree there is no node for it; with one there is
    /// none if it declares nothing.
    #[inline]
    pub(crate) fn pop(&mut self, ast: Option<&mut Ast>) -> Option<ScopeId> {
        self.close(ast).scope
    }

    /// Takes what the function binds from the references. [`Scopes::pending_references`] are now its free
    /// names, until [`Scopes::leave_function`].
    pub(crate) fn close_function(&mut self, ast: Option<&mut Ast>) -> Closed {
        self.resolve(ast)
    }

    pub(crate) fn leave_function(&mut self) {
        let Some(frame) = self.frames.pop() else {
            return;
        };
        self.floor = frame.outer_floor;
        self.frame_mut().flags |= frame.flags & ScopeFlags::CONTAINS_EVAL;
        self.hand_up(frame.references, |reference| Reference {
            access: reference.access.repeated() | Access::CAPTURED,
            first: u32::MAX,
            ..reference
        });
    }

    /// Closes the outermost scope. [`Scopes::pending_references`] are what is left for the code around it.
    pub(crate) fn close_root(&mut self, ast: &mut Ast) -> Option<ScopeId> {
        self.resolve(Some(ast)).scope
    }

    fn close(&mut self, ast: Option<&mut Ast>) -> Closed {
        let closed = self.resolve(ast);
        let Some(frame) = self.frames.pop() else {
            return closed;
        };
        self.floor = frame.outer_floor;
        let parent = self.frame_mut();
        parent.flags |= frame.flags & ScopeFlags::CONTAINS_EVAL;
        let is_repeated = frame.loop_depth > parent.loop_depth;
        let is_with = frame.kind == ScopeKind::With;
        self.hand_up(frame.references, |mut reference| {
            if is_repeated {
                reference.access = reference.access.repeated();
            }
            if is_with && !reference.name.is_internal() {
                reference.access |= Access::CAPTURED;
            }
            reference
        });
        closed
    }

    fn resolve(&mut self, mut ast: Option<&mut Ast>) -> Closed {
        let frame = *self.frame();
        let declares_variables = self.is_at_top_level();
        if let Some(ast) = ast.as_deref_mut()
            && self.candidates.len() > frame.candidates as usize
        {
            if declares_variables {
                self.hoist_candidates(&frame, ast);
            } else {
                self.filter_candidates(&frame);
            }
        }
        let mut closed = Closed {
            scope: None,
            contains_eval: frame.flags.contains(ScopeFlags::CONTAINS_EVAL),
            captures_home: false,
        };
        if declares_variables || self.lexicals.len() > frame.lexicals as usize {
            self.bind_references(&frame, declares_variables, &mut closed);
        }
        if let Some(ast) = ast {
            closed.scope = self.record(&frame, declares_variables, ast);
        }
        self.drop_lexicals(frame.lexicals);
        if declares_variables {
            self.drop_variables(frame.variables);
        }
        self.functions.truncate(frame.functions as usize);
        closed
    }

    fn bind_references(&mut self, frame: &Frame, declares_variables: bool, closed: &mut Closed) {
        let mut kept = frame.references as usize;
        for index in frame.references as usize..self.references.len() {
            let reference = self.references[index];
            let heads = &mut self.heads[reference.name.index()];
            let binding = if heads.lexical > frame.lexicals {
                &mut self.lexicals[heads.lexical as usize - 1].binding
            } else if declares_variables && heads.variable > frame.variables {
                &mut self.variables[heads.variable as usize - 1].binding
            } else if let Some(kind) = Self::implicit_binding(frame, reference.name) {
                closed.captures_home |= reference.name == Name::HOME;
                heads.reference = reference.outer;
                match kind {
                    Some(kind) => self.add_variable(reference.name, kind, 0),
                    None => continue,
                }
            } else {
                self.references[kept] = reference;
                kept += 1;
                heads.reference = kept as Link;
                continue;
            };
            binding.flags |= BindingFlags::USED;
            if reference.access.contains(Access::CAPTURED) {
                binding.flags |= BindingFlags::CAPTURED;
            }
            if reference.access.contains(Access::WRITTEN) {
                binding.flags |= BindingFlags::WRITTEN;
            }
            if reference.access.contains(Access::REWRITTEN) {
                binding.flags |= BindingFlags::REWRITTEN;
            }
            if reference.first < binding.initialized_from
                || (reference.first != u32::MAX && binding.initialized_until != u32::MAX)
            {
                binding.flags |= BindingFlags::USED_EARLY;
            }
            self.heads[reference.name.index()].reference = reference.outer;
        }
        self.references.truncate(kept);
    }

    /// Whether a reference to `name` ends at this scope though nothing declares it, and what it finds if anything.
    #[inline(always)]
    fn implicit_binding(frame: &Frame, name: Name) -> Option<Option<BindingKind>> {
        if name != Name::ARGUMENTS && !name.is_internal() {
            return None;
        }
        let function = match frame.function {
            Some(FunctionKind::Arrow) => return None,
            Some(function) => function,
            None => {
                let provided = match frame.kind {
                    ScopeKind::Script | ScopeKind::Function => Name::THIS_VALUE,
                    ScopeKind::Module => Name::IMPORT_META,
                    _ => return None,
                };
                return (name == provided).then_some(Some(BindingKind::Internal));
            }
        };
        let exists = match name {
            Name::ARGUMENTS => {
                let has_arguments = function != FunctionKind::FieldInitializer;
                return Some(has_arguments.then_some(BindingKind::Arguments));
            }
            Name::THIS_VALUE | Name::NEW_TARGET => true,
            Name::HOME => function.has_home_object(),
            Name::ACTIVE_FUNCTION | Name::FIELD_INITIALIZER => {
                function == FunctionKind::DerivedConstructor
            }
            _ => return None,
        };
        Some(exists.then_some(BindingKind::Internal))
    }

    fn filter_candidates(&mut self, frame: &Frame) {
        let mut kept = frame.candidates as usize;
        for index in frame.candidates as usize..self.candidates.len() {
            let candidate = self.candidates[index];
            if candidate.has_left_its_block
                && frame.kind != ScopeKind::SimpleCatch
                && self.own_lexical(candidate.name).is_some()
            {
                continue;
            }
            self.candidates[kept] = Candidate {
                has_left_its_block: true,
                ..candidate
            };
            kept += 1;
        }
        self.candidates.truncate(kept);
    }

    /// ECMA-262 B.3.2.1 to B.3.2.3.
    fn hoist_candidates(&mut self, frame: &Frame, ast: &mut Ast) {
        for index in frame.candidates as usize..self.candidates.len() {
            let Candidate { function, name, .. } = self.candidates[index];
            if self.own_lexical(name).is_some() || self.is_parameter(name) {
                continue;
            }
            ast[function].flags |= FunctionFlags::HOISTED_AS_VARIABLE;
            let is_new = self
                .own_variable(name)
                .is_none_or(|variable| variable.binding.kind == BindingKind::Callee);
            if is_new && name == Name::ARGUMENTS && self.has_own_arguments() {
                self.rewrite(name, u32::MAX - 1);
                continue;
            }
            let binding = self.declare_in_variable_scope(name, 0);
            binding.flags |= BindingFlags::USED | BindingFlags::WRITTEN | BindingFlags::REWRITTEN;
            if is_new {
                binding.flags |= BindingFlags::HOISTED;
            }
        }
        self.candidates.truncate(frame.candidates as usize);
    }

    fn has_own_arguments(&self) -> bool {
        let is_body = self.kind() == ScopeKind::FunctionBody;
        self.frames[self.frames.len() - 1 - usize::from(is_body)]
            .function
            .is_some_and(|function| function != FunctionKind::Arrow)
    }

    fn record(
        &mut self,
        frame: &Frame,
        declares_variables: bool,
        ast: &mut Ast,
    ) -> Option<ScopeId> {
        let variables = if declares_variables {
            frame.variables as usize
        } else if self.lexicals.len() == frame.lexicals as usize {
            return None;
        } else {
            self.variables.len()
        };
        let mut flags = frame.flags;
        let is_open = self.capture_everything || flags.contains(ScopeFlags::CONTAINS_EVAL);
        let first_binding = ast.bindings.len() as u32;
        for declaration in self.variables[variables..]
            .iter()
            .chain(&self.lexicals[frame.lexicals as usize..])
        {
            let mut binding = declaration.binding;
            if is_open {
                binding.flags |= BindingFlags::USED
                    | BindingFlags::CAPTURED
                    | BindingFlags::WRITTEN
                    | BindingFlags::REWRITTEN;
            }
            if binding.flags.contains(BindingFlags::CAPTURED) {
                flags |= ScopeFlags::HAS_CAPTURED;
            }
            ast.bindings.push(binding);
        }
        if frame.kind == ScopeKind::FunctionBody {
            flags |= self.relate_body_to_parameters(first_binding, ast);
        }
        let functions = if self.functions.len() > frame.functions as usize {
            let open = ast.open_list();
            for &function in &self.functions[frame.functions as usize..] {
                ast.push_to_list(function);
            }
            ast.close_list(open)
        } else {
            List::EMPTY
        };
        Some(ast.push_scope(Scope {
            kind: frame.kind,
            flags,
            first_binding,
            binding_count: ast.bindings.len() as u32 - first_binding,
            functions,
            start: frame.start,
        }))
    }

    /// A variable starts with the value of the parameter, or of the `arguments`, that it is named after.
    fn relate_body_to_parameters(&mut self, first_binding: u32, ast: &Ast) -> ScopeFlags {
        let parameters = self.frames[self.frames.len() - 2];
        let mut is_hidden = parameters.flags.contains(ScopeFlags::CONTAINS_EVAL);
        for binding in &ast.bindings[first_binding as usize..] {
            let link = self.heads[binding.name.index()].reference;
            is_hidden |= link > parameters.references
                && self.references[link as usize - 1]
                    .access
                    .contains(Access::CAPTURED);
            if binding.kind == BindingKind::Var
                && !binding.flags.contains(BindingFlags::FUNCTION)
                && (self.is_parameter(binding.name)
                    || (binding.name == Name::ARGUMENTS && self.has_own_arguments()))
            {
                let next = self.references.len() as Link + 1;
                let outer =
                    core::mem::replace(&mut self.heads[binding.name.index()].reference, next);
                self.references.push(Reference {
                    name: binding.name,
                    access: Access::empty(),
                    first: u32::MAX - 1,
                    outer,
                });
            }
        }
        if is_hidden {
            ScopeFlags::HIDDEN_FROM_PARAMETERS
        } else {
            ScopeFlags::empty()
        }
    }

    /// A scope for the methods of an object literal, which code between them does not see.
    pub(crate) fn record_home_scope(start: u32, ast: &mut Ast) -> ScopeId {
        ast.bindings.push(Binding {
            name: Name::CLASS_HOME,
            kind: BindingKind::Internal,
            flags: BindingFlags::USED | BindingFlags::CAPTURED,
            initialized_from: 0,
            initialized_until: u32::MAX,
        });
        ast.push_scope(Scope {
            kind: ScopeKind::Class,
            flags: ScopeFlags::HAS_CAPTURED,
            first_binding: ast.bindings.len() as u32 - 1,
            binding_count: 1,
            functions: List::EMPTY,
            start,
        })
    }
}
