//! Functions that are written in JavaScript with `@names`: those of the standard library, and those of the embedder.

use core::cell::{OnceCell, RefCell};
use std::rc::Rc;

use bun_engine_bytecode::{
    BuiltinFunction, BuiltinValue, FunctionInfo, FunctionKind, FunctionRef, Host, Visibility,
};
use bun_engine_macros::FieldOffsets;
use bun_engine_text::{Atom, Identifier, PropertyName, StringRef, Text};

use super::builtin_values::BUILTIN_VALUES;
use crate::{
    BuiltinId, FastHashMap, FunctionExecutable, Gc, RealmSlot, SourceCode, SourceProvider,
    SourceProviderRef, SourceType, Vm,
};

#[derive(Default)]
pub(super) struct Builtins {
    functions: RefCell<FastHashMap<u16, (SourceProviderRef, FunctionRef)>>,
    /// By the address of the text.
    embedder_functions: RefCell<FastHashMap<usize, UnlinkedFunction>>,
    /// By the address of the symbol.
    values: OnceCell<FastHashMap<usize, BuiltinValue>>,
    embedder_names: RefCell<FastHashMap<Identifier, Identifier>>,
    /// Of base and of derived classes.
    default_constructors: [OnceCell<SourceProviderRef>; 2],
}

impl Host for Vm {
    fn private_name(&self, name: Text<'_>) -> Option<Identifier> {
        let names = self.services.code.builtins.embedder_names.borrow();
        names.get(&Atom::find(name)?.name()).cloned()
    }

    fn builtin_value(&self, name: PropertyName<'_>) -> Option<BuiltinValue> {
        let values = self.services.code.builtins.values.get_or_init(|| {
            (RealmSlot::PRIVATE_NAMES.iter())
                .map(|&(name, slot)| (name, BuiltinValue::LinkTime(slot.as_constant())))
                .chain(BUILTIN_VALUES.iter().copied())
                .map(|(name, value)| (name.address(), value))
                .collect()
        });
        values.get(&name.address()).copied()
    }
}

/// What can be told of `(function (..) { .. })` without parsing it. The offsets count from the first parenthesis.
#[derive(Clone, Copy, FieldOffsets)]
#[repr(C)]
pub struct BuiltinOutline {
    pub parameters_start: u32,
    pub length: u32,
    /// After the closing brace.
    pub end: u32,
    pub is_async: bool,
    pub is_generator: bool,
    pub is_strict: bool,
}

impl BuiltinOutline {
    /// No parameter has a default. Panics if the text is no such function.
    pub fn of(text: &[u8]) -> BuiltinOutline {
        let find = |byte, from: usize| {
            from + bun_highway::index_of_char(&text[from..], byte).expect("a function")
        };
        let parameters_start = find(b'(', 1);
        let (mut at, mut depth, mut length, mut is_in_parameter) =
            (parameters_start + 1, 0, 0, false);
        loop {
            match text[at] {
                b')' => break,
                b'/' => at = find(b'/', at + 1),
                b',' if depth == 0 => is_in_parameter = false,
                b'.' if !is_in_parameter => is_in_parameter = true,
                b' ' | b'\n' => {}
                byte => {
                    length += u32::from(!is_in_parameter);
                    is_in_parameter = true;
                    match byte {
                        b'{' | b'[' => depth += 1,
                        b'}' | b']' => depth -= 1,
                        _ => {}
                    }
                }
            }
            at += 1;
        }
        let body = text[find(b'{', at) + 1..].trim_ascii_start();
        let after_brace = text.iter().rev().take_while(|&&byte| byte != b'}').count();
        BuiltinOutline {
            parameters_start: parameters_start as u32,
            length,
            end: (text.len() - after_brace) as u32,
            is_async: text.starts_with(b"(async "),
            is_generator: text[..parameters_start].trim_ascii_end().ends_with(b"*"),
            is_strict: body
                .get(1..)
                .is_some_and(|it| it.starts_with(b"use strict")),
        }
    }
}

/// Errors and debuggers point into the source if it has a URL.
pub fn describe_builtin(
    source: &SourceCode,
    outline: &BuiltinOutline,
    name: Identifier,
    kind: FunctionKind,
    visibility: Visibility,
) -> FunctionRef {
    BuiltinFunction {
        name,
        range: source.start + 1..source.start + outline.end,
        parameters_start: source.start + outline.parameters_start,
        length: outline.length,
        kind,
        is_strict: outline.is_strict,
        is_async: outline.is_async,
        is_generator: outline.is_generator,
        // The frame is that of the body too, which only `PrivateRecursive` hides.
        visibility: match visibility {
            Visibility::Private if outline.is_async => Visibility::Public,
            _ => visibility,
        },
        has_positions: source.provider.url().is_some(),
    }
    .into_function()
}

/// A function that refers to nothing but globals, and its code.
pub struct UnlinkedFunction {
    pub provider: SourceProviderRef,
    pub function: FunctionRef,
}

impl UnlinkedFunction {
    /// A new one, whose code is linked to the realm that calls it first.
    pub fn link(&self, vm: &Vm) -> Gc<FunctionExecutable> {
        FunctionExecutable::create(vm, self.provider.clone(), None, self.function.clone())
    }
}

/// One of the embedder, whose source has no URL.
pub struct StaticBuiltin {
    pub text: &'static [u8],
    pub outline: BuiltinOutline,
    pub name: &'static str,
    pub kind: FunctionKind,
    pub visibility: Visibility,
}

impl Vm {
    /// See `UnlinkedFunction::link`.
    pub fn link_static_builtin(&self, builtin: StaticBuiltin) -> Gc<FunctionExecutable> {
        let _defer = self.heap.defer_collection();
        let mut functions = self.services.code.builtins.embedder_functions.borrow_mut();
        let function = functions.entry(builtin.text.as_ptr().addr());
        let function = function.or_insert_with(|| {
            let text = StringRef::from_static_units(builtin.text);
            let provider = SourceProvider::build(text, None, SourceType::Program);
            let source = SourceCode::whole(provider.finish());
            let (name, kind) = (Identifier::from_literal(builtin.name), builtin.kind);
            UnlinkedFunction {
                function: describe_builtin(
                    &source,
                    &builtin.outline,
                    name,
                    kind,
                    builtin.visibility,
                ),
                provider: source.provider,
            }
        });
        function.link(self)
    }

    pub(super) fn forget_static_builtins(&self) {
        self.services.code.builtins.embedder_functions.take();
    }

    /// Makes `@name` stand for `symbol`.
    pub fn register_private_name(&self, name: Atom, symbol: Identifier) {
        let mut names = self.services.code.builtins.embedder_names.borrow_mut();
        names.insert(name.into(), symbol);
    }

    fn builtin_function(&self, id: BuiltinId) -> (SourceProviderRef, FunctionRef) {
        let mut functions = self.services.code.builtins.functions.borrow_mut();
        let function = functions.entry(id.0).or_insert_with(|| {
            let builtin = (self.hooks().builtin_source)(id);
            let text = StringRef::from_literal(builtin.text);
            let provider = SourceProvider::build(text, None, SourceType::Program);
            (provider.finish(), builtin.to_function())
        });
        function.clone()
    }

    pub(super) fn default_constructor_provider(&self, function: &FunctionRef) -> SourceProviderRef {
        let index = usize::from(function.kind() == FunctionKind::DerivedConstructor);
        let provider = &self.services.code.builtins.default_constructors[index];
        let provider = provider.get_or_init(|| {
            let text = StringRef::from_literal(function.default_constructor_source());
            SourceProvider::build(text, None, SourceType::Program).finish()
        });
        provider.clone()
    }

    pub(super) fn builtin_function_tables(&self) -> Vec<Rc<[FunctionInfo]>> {
        let functions = self.services.code.builtins.functions.borrow();
        (functions.values().map(|it| Rc::clone(it.1.table()))).collect()
    }

    /// A new one, whose code is linked to the realm that calls it first.
    pub fn builtin_executable(&self, id: BuiltinId) -> Gc<FunctionExecutable> {
        let (provider, function) = self.builtin_function(id);
        let executable = FunctionExecutable::create(self, provider, None, function);
        executable.set_intrinsic((self.hooks().builtin_source)(id).intrinsic);
        executable
    }
}

impl FunctionExecutable {
    pub fn is_for_builtin(&self, vm: &Vm, id: BuiltinId) -> bool {
        core::ptr::eq(
            &raw const **self.info(),
            &raw const *vm.builtin_function(id).1,
        )
    }
}
