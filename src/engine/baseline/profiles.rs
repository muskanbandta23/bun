//! What the code has done so far, which decides the cases that get a place in its templates.

use bun_engine_bytecode::{MetadataKind, RawInstruction};
use bun_engine_vm::code::metadata::{
    ArrayAccess, GetById, GetByVal, GetGlobal, GetLength, GlobalAccess, IteratorNext, NewObject,
};
use bun_engine_vm::ic::{LoadCache, StoreCache};
use bun_engine_vm::{Arithmetic, ArithmeticProfile, ArrayModes, CodeBlock, ValueProfile};

/// What a register is likely to hold.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Likely {
    #[default]
    Unknown,
    Int32,
    Double,
    Boolean,
    Cell,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum InlineLoad {
    None,
    InObject,
    InStorage,
    Constant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum InlineStore {
    None,
    Replace { in_object: bool },
    Transition { in_object: bool },
}

#[derive(Clone, Copy)]
pub(crate) struct Profiles<'a>(&'a CodeBlock);

macro_rules! entry {
    ($self:ident, $word:expr, $type:ty, $($kind:ident)|+) => {{
        let layout = $self.0.body().metadata();
        assert!($(layout.range(MetadataKind::$kind).contains(&$word))||+);
        // SAFETY: among the entries of that type, and from an instruction of verified code.
        unsafe { $self.0.metadata::<$type>($word) }
    }};
}

fn likely(profile: &ValueProfile) -> Likely {
    match profile.last() {
        None => Likely::Unknown,
        Some(value) if value.is_int32() => Likely::Int32,
        Some(value) if value.is_number() => Likely::Double,
        Some(value) if value.is_boolean() => Likely::Boolean,
        Some(value) if value.is_cell() => Likely::Cell,
        Some(_) => Likely::Unknown,
    }
}

fn inline_load(cache: &LoadCache) -> InlineLoad {
    match cache.byte_offset() {
        _ if cache.shape().is_none() => InlineLoad::None,
        0 => InlineLoad::Constant,
        1.. => InlineLoad::InObject,
        _ => InlineLoad::InStorage,
    }
}

impl<'a> Profiles<'a> {
    pub(crate) fn new(code_block: &'a CodeBlock) -> Profiles<'a> {
        Profiles(code_block)
    }

    pub(crate) fn arithmetic(self, word: u32) -> Arithmetic {
        entry!(
            self,
            word,
            ArithmeticProfile,
            UnaryArithmetic | BinaryArithmetic
        )
        .get()
    }

    /// With `this`.
    pub(crate) fn argument(self, index: usize) -> Likely {
        (self.0.argument_profiles().get(index)).map_or(Likely::Unknown, likely)
    }

    /// Of an instruction that has a value profile.
    pub(crate) fn result(self, instruction: &RawInstruction) -> Likely {
        let kind = instruction.opcode.metadata();
        if !kind.is_some_and(MetadataKind::has_value_profile) {
            return Likely::Unknown;
        }
        // SAFETY: such an entry starts with one.
        likely(unsafe { self.0.metadata::<ValueProfile>(instruction.metadata) })
    }

    pub(crate) fn get_by_id(self, word: u32) -> InlineLoad {
        inline_load(&entry!(self, word, GetById, GetById | GetByIdDirect).cache)
    }

    pub(crate) fn get_length(self, word: u32) -> ArrayModes {
        entry!(self, word, GetLength, GetLength).array.modes()
    }

    pub(crate) fn put_by_id(self, word: u32) -> InlineStore {
        let cache = entry!(self, word, StoreCache, PutById);
        let in_object = cache.byte_offset() > 0;
        if cache.shape().is_none() {
            InlineStore::None
        } else if cache.new_shape().is_none() {
            InlineStore::Replace { in_object }
        } else {
            InlineStore::Transition { in_object }
        }
    }

    pub(crate) fn get_by_val(self, word: u32) -> ArrayModes {
        entry!(self, word, GetByVal, GetByVal).array.modes()
    }

    pub(crate) fn array_access(self, word: u32) -> ArrayModes {
        entry!(self, word, ArrayAccess, ArrayAccess).array.modes()
    }

    pub(crate) fn iterated_array(self, word: u32) -> ArrayModes {
        entry!(self, word, IteratorNext, IteratorNext).array.modes()
    }

    /// Whether the name is that of a property in the shape of the global object.
    pub(crate) fn reads_global_property(self, word: u32) -> bool {
        !entry!(self, word, GetGlobal, GetGlobal)
            .access
            .shape
            .get()
            .is_none()
    }

    pub(crate) fn writes_global_property(self, word: u32) -> bool {
        !entry!(self, word, GlobalAccess, PutGlobal)
            .shape
            .get()
            .is_none()
    }

    pub(crate) fn inline_capacity(self, word: u32) -> Option<u32> {
        let entry = entry!(self, word, NewObject, NewObject);
        (!entry.allocator.get().is_null()).then(|| entry.inline_capacity.get())
    }
}
