//! From source text to code.

use core::ops::Range;
use std::rc::Rc;

use bun_engine_syntax::ast::{Ast, Features};
use bun_engine_syntax::{
    Error, EvalContext, FunctionFlags, FunctionKind, FunctionSource, Goal, Options, Parser,
    SourceCache,
};
use bun_engine_text::{Identifier, StringRef, Text};

use crate::code::{Code, Directives, Eval, Module, Script};
use crate::function::{
    FunctionInfo, FunctionName, FunctionRef, Origin, Specialization, Visibility,
};
use crate::generator::{Buffers, Environment, Failure};
use crate::host::Host;
use crate::scope::StaticScope;

bitflags::bitflags! {
    /// What code has in it besides what runs the program.
    #[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
    pub struct Instrumentation: u8 {
        const DEBUGGER = 1 << 0;
        const COVERAGE = 1 << 1;
    }
}

pub struct Request<'a> {
    pub source: &'a StringRef,
    pub cache: Option<&'a mut SourceCache>,
    pub host: &'a dyn Host,
    pub instrumentation: Instrumentation,
    /// The request fails before the stack grows below this address.
    pub stack_limit: usize,
}

/// What a built-in function is, which is known without parsing it.
pub struct BuiltinFunction {
    pub name: Identifier,
    /// From `function` or `async` to the closing brace.
    pub range: Range<u32>,
    pub parameters_start: u32,
    pub length: u32,
    pub kind: FunctionKind,
    pub is_strict: bool,
    pub is_async: bool,
    pub is_generator: bool,
    pub visibility: Visibility,
    /// Whether errors and debuggers point into its source.
    pub has_positions: bool,
}

impl BuiltinFunction {
    pub fn into_function(self) -> FunctionRef {
        let mut flags = FunctionFlags::empty();
        flags.set(FunctionFlags::STRICT, self.is_strict);
        flags.set(FunctionFlags::ASYNC, self.is_async);
        flags.set(FunctionFlags::GENERATOR, self.is_generator);
        let mut origin = Origin::BUILTIN;
        origin.set(Origin::NO_POSITIONS, !self.has_positions);
        let info = FunctionInfo {
            name: FunctionName::Owned(self.name),
            kind: self.kind,
            flags,
            origin,
            visibility: self.visibility,
            has_own_name: true,
            context: EvalContext::empty(),
            instance_fields: 0,
            start: self.range.start,
            parameters_start: self.parameters_start,
            end: self.range.end,
            text: self.range,
            length: self.length,
            scope: None,
            created_at: 0,
            fields: None,
            code: Default::default(),
        };
        FunctionRef::new(Rc::new([info]), 0)
    }
}

/// Keeps its buffers from one request to the next.
#[derive(Default)]
pub struct Compiler {
    parser: Parser,
    buffers: Buffers,
    no_tree: Ast,
}

fn directives(ast: &Ast) -> Directives {
    Directives {
        source_url: ast.root.source_url.clone(),
        source_mapping_url: ast.root.source_mapping_url.clone(),
    }
}

impl<'a> Request<'a> {
    fn options<'b>(&self, goal: Goal<'b>, range: Range<u32>, origin: Origin) -> Options<'b> {
        Options {
            goal,
            range,
            builtin: origin.contains(Origin::BUILTIN),
            in_module: origin.contains(Origin::MODULE),
            capture_everything: self.instrumentation.contains(Instrumentation::DEBUGGER)
                && !origin.contains(Origin::NO_POSITIONS),
            stack_limit: self.stack_limit,
        }
    }

    fn whole(&self) -> Range<u32> {
        0..self.source.len() as u32
    }

    fn environment(&self) -> Environment<'a> {
        Environment {
            source: self.source,
            host: self.host,
            stack_limit: self.stack_limit,
            debugs: self.instrumentation.contains(Instrumentation::DEBUGGER),
            covers: self.instrumentation.contains(Instrumentation::COVERAGE),
        }
    }
}

impl Compiler {
    /// `outer` is what lies between the code and the global scope, here and below.
    pub fn script(
        &mut self,
        request: Request<'_>,
        is_strict: bool,
        outer: Option<Rc<StaticScope>>,
    ) -> Result<Script, Error> {
        let goal = Goal::Script { strict: is_strict };
        let options = request.options(goal, request.whole(), Origin::empty());
        let environment = request.environment();
        let ast = (self.parser).parse(request.source.text(), &options, request.cache)?;
        let (code, declarations) = self.buffers.script(&environment, ast, outer)?;
        Ok(Script {
            code,
            declarations,
            directives: directives(ast),
        })
    }

    /// With `keeps_completion_value` the code returns what a script would.
    pub fn module(
        &mut self,
        request: Request<'_>,
        keeps_completion_value: bool,
        outer: Option<Rc<StaticScope>>,
    ) -> Result<Module, Error> {
        let options = request.options(Goal::Module, request.whole(), Origin::MODULE);
        let environment = request.environment();
        let ast = (self.parser).parse(request.source.text(), &options, request.cache)?;
        let module = (self.buffers).module(&environment, ast, outer, keeps_completion_value)?;
        Ok(Module {
            code: module.code,
            scope: module.scope,
            requests: module.requests,
            imports: module.imports,
            exports: module.exports,
            directives: directives(ast),
        })
    }

    pub fn eval(
        &mut self,
        request: Request<'_>,
        context: EvalContext,
        outer: Option<Rc<StaticScope>>,
    ) -> Result<Eval, Error> {
        let is_private_name_declared = |name: Text<'_>| {
            outer
                .as_ref()
                .is_some_and(|it| it.declares_private_name(name))
        };
        let goal = Goal::Eval {
            context,
            is_private_name_declared: &is_private_name_declared,
        };
        let options = request.options(goal, request.whole(), Origin::empty());
        let environment = request.environment();
        let ast = (self.parser).parse(request.source.text(), &options, request.cache)?;
        let (code, declarations) = self.buffers.eval(&environment, ast, outer, context)?;
        Ok(Eval {
            code,
            declarations,
            directives: directives(ast),
            is_reusable: !ast.root.features.contains(Features::TAGGED_TEMPLATE),
        })
    }

    /// For the `Function` constructor and its kin. See [`Goal::SingleFunction`].
    pub fn single_function(
        &mut self,
        request: Request<'_>,
        parameters_end: Option<u32>,
        outer: Option<Rc<StaticScope>>,
    ) -> Result<(FunctionRef, Directives), Error> {
        let goal = Goal::SingleFunction { parameters_end };
        let options = request.options(goal, request.whole(), Origin::empty());
        let environment = request.environment();
        let ast = (self.parser).parse(request.source.text(), &options, request.cache)?;
        let function = self.buffers.single_function(&environment, ast, outer);
        let function = function.ok_or(Failure::TooLarge)?;
        Ok((FunctionRef::new(Rc::new([function]), 0), directives(ast)))
    }

    /// The source is the one that the function came from. Does not look at the code that it has, nor set it.
    pub fn function(
        &mut self,
        request: Request<'_>,
        function: &FunctionInfo,
        specialization: Specialization,
    ) -> Result<Rc<Code>, Error> {
        let environment = request.environment();
        let text = request.source.text();
        let ast = if function.origin.contains(Origin::DEFAULT_CONSTRUCTOR)
            || (function.needs_new() && specialization == Specialization::Call)
        {
            &self.no_tree
        } else {
            let goal = Goal::Function(FunctionSource {
                kind: function.kind,
                flags: function.flags,
                own_name: function.name_text(text),
                fields: function.fields.as_ref().map_or(&[], |it| &it.sources),
            });
            let range = function.parameters_start..function.end;
            let options = request.options(goal, range, function.origin);
            self.parser.parse(text, &options, request.cache)?
        };
        Ok((self.buffers).function(&environment, ast, function, specialization)?)
    }

    pub fn check_syntax(&mut self, request: Request<'_>, is_module: bool) -> Result<(), Error> {
        let (goal, origin) = if is_module {
            (Goal::Module, Origin::MODULE)
        } else {
            (Goal::Script { strict: false }, Origin::empty())
        };
        let options = request.options(goal, request.whole(), origin);
        (self.parser).parse(request.source.text(), &options, request.cache)?;
        Ok(())
    }
}
