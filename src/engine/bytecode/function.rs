//! A function whose code is generated when it is first called.

use core::cell::Cell;
use core::ops::Range;
use std::rc::Rc;

use bun_engine_syntax::{EvalContext, FieldSource, FunctionFlags, FunctionKind, Name};
use bun_engine_text::{Identifier, Text};

use crate::code::Code;
use crate::scope::StaticScope;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Specialization {
    Call,
    Construct,
}

/// Whether stack traces show the frames of a function.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(u8)]
pub enum Visibility {
    Public,
    Private,
    /// Nor those of the functions inside it.
    PrivateRecursive,
}

bitflags::bitflags! {
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct Origin: u8 {
        const BUILTIN = 1 << 0;
        const MODULE = 1 << 1;
        /// The constructor of a class that has none. Its positions are in [`FunctionInfo::default_constructor_source`].
        const DEFAULT_CONSTRUCTOR = 1 << 2;
        /// Errors say nothing of where in the source they come from.
        const NO_POSITIONS = 1 << 3;
        /// In code that a debugger has evaluated in a frame.
        const DEBUGGER = 1 << 4;
    }
}

#[derive(Clone, Debug)]
pub(crate) enum FunctionName {
    Source {
        start: u32,
        length: u32,
    },
    /// A constant of `Name`.
    Known(Name),
    Owned(Identifier),
}

#[derive(Debug)]
pub(crate) struct Fields {
    pub(crate) sources: Box<[FieldSource]>,
    /// For each of `sources`, where the scope of the class keeps its key if that is computed.
    pub(crate) key_slots: Box<[u32]>,
    pub(crate) sets_brand: bool,
}

pub struct FunctionInfo {
    pub(crate) name: FunctionName,
    pub(crate) kind: FunctionKind,
    pub(crate) flags: FunctionFlags,
    pub(crate) origin: Origin,
    pub(crate) visibility: Visibility,
    /// Whether `name` stands in the source as the name of the function or class.
    pub(crate) has_own_name: bool,
    pub(crate) context: EvalContext,
    pub(crate) instance_fields: u8,
    pub(crate) text: Range<u32>,
    pub(crate) start: u32,
    pub(crate) parameters_start: u32,
    pub(crate) end: u32,
    pub(crate) length: u32,
    pub(crate) scope: Option<Rc<StaticScope>>,
    pub(crate) created_at: u32,
    pub(crate) fields: Option<Box<Fields>>,
    pub(crate) code: [Cell<Option<Rc<Code>>>; 2],
}

impl FunctionInfo {
    pub fn kind(&self) -> FunctionKind {
        self.kind
    }

    pub fn is_strict(&self) -> bool {
        self.flags.contains(FunctionFlags::STRICT)
    }

    pub fn is_async(&self) -> bool {
        self.flags.contains(FunctionFlags::ASYNC)
    }

    pub fn is_generator(&self) -> bool {
        self.flags.contains(FunctionFlags::GENERATOR)
    }

    pub fn is_constructor(&self) -> bool {
        match self.kind {
            FunctionKind::Normal => !self.is_async() && !self.is_generator(),
            _ => self.needs_new(),
        }
    }

    /// Whether a call without `new` throws.
    pub fn needs_new(&self) -> bool {
        self.kind.is_class_constructor() || self.kind == FunctionKind::NakedConstructor
    }

    /// Of a constructor: how many fields its class declares for instances, as far as a byte counts.
    pub fn instance_fields(&self) -> u8 {
        self.instance_fields
    }

    /// Whether the name stands in the source as that of the function or class.
    pub fn has_own_name(&self) -> bool {
        self.has_own_name
    }

    pub fn has_no_outer_scope(&self) -> bool {
        self.scope.is_none()
    }

    /// What lies between the function and the global scope.
    pub fn outer_scope(&self) -> Option<&Rc<StaticScope>> {
        self.scope.as_ref()
    }

    pub fn origin(&self) -> Origin {
        self.origin
    }

    pub fn visibility(&self) -> Visibility {
        self.visibility
    }

    /// What a direct `eval` at the top level of the function may do.
    pub fn eval_context(&self) -> EvalContext {
        let (kind, mut context) = (self.kind, self.context);
        if kind != FunctionKind::Arrow {
            context |= EvalContext::IN_FUNCTION;
            context.set(EvalContext::IN_METHOD, kind.has_home_object());
            context.set(
                EvalContext::IN_DERIVED_CONSTRUCTOR,
                kind == FunctionKind::DerivedConstructor,
            );
            context.set(
                EvalContext::IN_FIELD_INITIALIZER,
                kind == FunctionKind::FieldInitializer,
            );
        }
        context.set(EvalContext::STRICT, self.is_strict());
        context.set(
            EvalContext::BY_DEBUGGER,
            self.origin.contains(Origin::DEBUGGER),
        );
        context
    }

    /// Without the `get` or `set` of an accessor. Empty if it has none or it is computed.
    pub fn name(&self, source: Text<'_>) -> Identifier {
        match &self.name {
            FunctionName::Owned(name) => name.clone(),
            _ => Identifier::new(self.name_text(source)),
        }
    }

    pub fn name_text<'a>(&'a self, source: Text<'a>) -> Text<'a> {
        match &self.name {
            &FunctionName::Source { start, length } => {
                source.slice(start as usize..start as usize + length as usize)
            }
            FunctionName::Known(name) => {
                Text::from_ascii(Name::known_text(name.index()).unwrap_or_default())
            }
            FunctionName::Owned(name) => name.name().text(),
        }
    }

    /// The value of its `length` property.
    pub fn length(&self) -> u32 {
        self.length
    }

    /// What `toString` returns. Of a constructor, the class.
    pub fn text_range(&self) -> Range<u32> {
        self.text.clone()
    }

    /// From its first token to the end of its body.
    pub fn range(&self) -> Range<u32> {
        self.start..self.end
    }

    /// Whether it stands in the source as a function.
    pub fn is_written(&self) -> bool {
        !self.origin.contains(Origin::DEFAULT_CONSTRUCTOR)
            && self.kind != FunctionKind::FieldInitializer
    }

    /// The position of an error before the first one that the code records.
    pub fn parameters_start(&self) -> u32 {
        self.parameters_start
    }

    pub fn default_constructor_source(&self) -> &'static str {
        match self.kind {
            FunctionKind::DerivedConstructor => "(function (...args) { super(...args); })",
            _ => "(function () { })",
        }
    }

    /// The code, if it has been generated and not dropped.
    pub fn code(&self, specialization: Specialization) -> Option<Rc<Code>> {
        let cell = &self.code[specialization as usize];
        let code = cell.take();
        cell.set(code.clone());
        code
    }

    pub fn set_code(&self, specialization: Specialization, code: Option<Rc<Code>>) {
        self.code[specialization as usize].set(code);
    }
}

/// One of the functions of a table, which it keeps alive.
#[derive(Clone)]
pub struct FunctionRef {
    table: Rc<[FunctionInfo]>,
    index: u32,
}

impl FunctionRef {
    /// Panics if there is no such function.
    pub fn new(table: Rc<[FunctionInfo]>, index: u32) -> FunctionRef {
        assert!((index as usize) < table.len());
        FunctionRef { table, index }
    }

    /// It and the other functions of the code that has it.
    pub fn table(&self) -> &Rc<[FunctionInfo]> {
        &self.table
    }
}

impl core::ops::Deref for FunctionRef {
    type Target = FunctionInfo;

    #[inline]
    fn deref(&self) -> &FunctionInfo {
        &self.table[self.index as usize]
    }
}
