//! Functions that are written in JavaScript with `@names`: those of the standard library, and those of the embedder.

use core::cell::{OnceCell, RefCell};

use bun_engine_bytecode::{
    BuiltinFunction, BuiltinValue, FunctionKind, FunctionRef, Host, Visibility,
};
use bun_engine_text::{Atom, Identifier, PropertyName, StringRef, Text};

use super::builtin_values::BUILTIN_VALUES;
use crate::{
    BuiltinId, FastHashMap, FunctionExecutable, Gc, RealmSlot, SourceCode, SourceProvider,
    SourceProviderRef, SourceType, Vm,
};

#[derive(Default)]
pub(super) struct Builtins {
    functions: RefCell<FastHashMap<u16, (SourceProviderRef, FunctionRef)>>,
    /// By the address of the symbol.
    values: OnceCell<FastHashMap<usize, BuiltinValue>>,
    embedder_names: RefCell<FastHashMap<Identifier, Identifier>>,
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
#[derive(Clone, Copy)]
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
        visibility: if outline.is_async {
            visibility.max(Visibility::Private)
        } else {
            visibility
        },
        has_positions: source.provider.url().is_some(),
    }
    .into_function()
}

impl Vm {
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
