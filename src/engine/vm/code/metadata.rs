//! What the entries of the metadata of a code block look like. All zeros is the initial state of each.

use core::cell::{Cell, OnceCell, RefCell};

use bun_engine_bytecode::MetadataKind;

use super::profile::{ArithmeticProfile, ArrayProfile, ValueProfile};
use crate::heap::{Allocator, Survivors};
use crate::ic::{AccessCases, CallProfile, CallTargetCount, CaseArena, LoadCache, StoreCache};
use crate::{
    ArrayAllocationProfile, CodeBlock, Gc, JsCell, ShapeId, SpeculatedType, Tier, Visitor, options,
};

/// # Safety
/// All zeros is a value, and the size is that of the kinds of entries that it is used for.
pub unsafe trait Entry {
    fn visit(&self, _: &mut Visitor) {}

    /// Marks the targets of transitions from shapes that are marked.
    fn visit_transitions(&self, _: &mut Visitor) {}

    fn reconcile(&self, _: &CodeBlock, _: &Survivors) {}
}

/// The address of a cell that is not kept alive, or a number below `SPECIAL_LIMIT`.
#[repr(transparent)]
pub struct WeakCell(Cell<usize>);

impl WeakCell {
    pub const NONE: usize = 0;
    /// More than one has been seen.
    pub const MANY: usize = 1;
    const SPECIAL_LIMIT: usize = 16;

    #[inline(always)]
    pub fn bits(&self) -> usize {
        self.0.get()
    }

    pub fn get(&self) -> Option<Gc<JsCell>> {
        // SAFETY: cleared when a collection finds the cell dead.
        (self.bits() >= Self::SPECIAL_LIMIT).then(|| unsafe { Gc::from_address(self.bits()) })
    }

    pub fn set(&self, owner: &CodeBlock, cell: &JsCell) {
        self.0.set(cell.address());
        owner.write_barrier_for_cell();
    }

    /// While the owner is being reconciled, before `clear_if_dead`.
    pub(crate) fn set_unbarriered(&self, cell: &JsCell) {
        self.0.set(cell.address());
    }

    pub fn set_special(&self, value: usize) {
        assert!(value < Self::SPECIAL_LIMIT);
        self.0.set(value);
    }

    #[inline(always)]
    pub fn record(&self, owner: &CodeBlock, cell: &JsCell) {
        match self.bits() {
            Self::NONE => self.set(owner, cell),
            seen if seen != cell.address() => self.0.set(Self::MANY),
            _ => {}
        }
    }

    /// Whether it died.
    pub fn clear_if_dead(&self, survivors: &Survivors) -> bool {
        let is_dead = self.get().is_some_and(|cell| !survivors.contains(cell));
        if is_dead {
            self.0.set(Self::NONE);
        }
        is_dead
    }
}

#[repr(C, align(8))]
pub struct Flags(pub Cell<u32>);

#[repr(transparent)]
pub struct Counter(pub Cell<u64>);

#[repr(C)]
pub struct CasesAndValue {
    pub value: ValueProfile,
    pub cases: AccessCases,
}

#[repr(C)]
pub struct GetById {
    pub value: ValueProfile,
    pub cache: LoadCache,
}

#[repr(C)]
pub struct GetLength {
    pub value: ValueProfile,
    pub cache: LoadCache,
    pub array: ArrayProfile,
}

#[repr(C)]
pub struct GetByVal {
    pub value: ValueProfile,
    pub array: ArrayProfile,
    pub cases: AccessCases,
}

#[repr(C)]
pub struct ArrayAccess {
    pub array: ArrayProfile,
    pub cases: AccessCases,
}

/// The inline case is for the name in `name`.
#[repr(C)]
pub struct GetPrivateName {
    pub value: ValueProfile,
    pub cache: LoadCache,
    pub name: WeakCell,
}

#[repr(C)]
pub struct PutPrivateName {
    pub cache: StoreCache,
    pub name: WeakCell,
}

/// Without a shape `pointer` is the address of a `GlobalBinding`, with one it is that of the global object.
#[repr(C)]
pub struct GlobalAccess {
    pub shape: Cell<ShapeId>,
    pub byte_offset: Cell<i32>,
    pub pointer: Cell<usize>,
}

#[repr(C)]
pub struct GetGlobal {
    pub value: ValueProfile,
    pub access: GlobalAccess,
}

#[repr(C)]
pub struct GetImport {
    pub value: ValueProfile,
    /// The address of the slot, in an environment that the module keeps alive.
    pub slot: Cell<usize>,
    pub environment: Cell<usize>,
}

#[repr(C)]
pub struct Call {
    pub value: ValueProfile,
    pub profile: CallProfile,
}

#[repr(C)]
pub struct SuperConstruct {
    pub call: Call,
    pub new_target: WeakCell,
}

#[repr(C)]
pub struct ToThis {
    pub value: ValueProfile,
}

#[repr(C)]
pub struct NewObject {
    pub shape: Cell<ShapeId>,
    pub inline_capacity: Cell<u32>,
    /// Null if the object needs more than a cell.
    pub allocator: Cell<*const Allocator>,
}

/// What its owner makes of it.
#[repr(transparent)]
pub struct Word(pub Cell<u64>);

#[repr(C)]
pub struct NewArrayWithSpecies {
    pub value: ValueProfile,
    pub allocation: ArrayAllocationProfile,
    pub array: ArrayProfile,
}

#[repr(C)]
pub struct ForIn {
    pub modes: Flags,
    pub array: ArrayProfile,
    pub cases: AccessCases,
}

#[repr(C)]
pub struct ForInGet {
    pub value: ValueProfile,
    pub for_in: ForIn,
}

#[repr(C)]
pub struct IteratorNext {
    pub value: ValueProfile,
    pub modes: Flags,
    pub array: ArrayProfile,
}

macro_rules! entries {
    ($($type:ty { $($part:ident)* })*) => {$(
        // SAFETY: made of parts that are.
        unsafe impl Entry for $type {
            #[inline]
            #[allow(unused_variables)]
            fn visit(&self, visitor: &mut Visitor) {
                $(self.$part.visit(visitor);)*
            }

            #[inline]
            #[allow(unused_variables)]
            fn visit_transitions(&self, visitor: &mut Visitor) {
                $(self.$part.visit_transitions(visitor);)*
            }

            #[inline]
            #[allow(unused_variables)]
            fn reconcile(&self, owner: &CodeBlock, survivors: &Survivors) {
                $(self.$part.reconcile(owner, survivors);)*
            }
        }
    )*};
}

entries! {
    ValueProfile {}
    ArithmeticProfile {}
    Flags {}
    Counter {}
    Word {}
    CasesAndValue { cases }
    GetById { cache }
    GetLength { cache array }
    GetByVal { array cases }
    ArrayAccess { array cases }
    GetGlobal { access }
    GetImport {}
    Call { profile }
    SuperConstruct { call new_target }
    ToThis {}
    NewArrayWithSpecies { allocation array }
    ForIn { array cases }
    ForInGet { for_in }
    IteratorNext { array }
}

// SAFETY: integers.
unsafe impl Entry for WeakCell {
    fn reconcile(&self, _: &CodeBlock, survivors: &Survivors) {
        self.clear_if_dead(survivors);
    }
}

// SAFETY: integers.
unsafe impl Entry for ArrayProfile {
    fn reconcile(&self, _: &CodeBlock, _: &Survivors) {
        self.update();
    }
}

// SAFETY: integers.
unsafe impl Entry for GlobalAccess {
    fn reconcile(&self, _: &CodeBlock, survivors: &Survivors) {
        if !self.shape.get().is_none() && !survivors.contains_shape(self.shape.get()) {
            self.shape.set(ShapeId::NONE);
            self.pointer.set(0);
        }
    }
}

// SAFETY: made of parts that are.
unsafe impl Entry for GetPrivateName {
    fn visit(&self, visitor: &mut Visitor) {
        self.cache.visit(visitor);
    }

    fn reconcile(&self, owner: &CodeBlock, survivors: &Survivors) {
        if self.name.clear_if_dead(survivors) {
            self.cache.reset();
        }
        self.cache.reconcile(owner, survivors);
    }
}

// SAFETY: as above.
unsafe impl Entry for PutPrivateName {
    fn visit(&self, visitor: &mut Visitor) {
        self.cache.visit(visitor);
    }

    fn visit_transitions(&self, visitor: &mut Visitor) {
        self.cache.visit_transitions(visitor);
    }

    fn reconcile(&self, owner: &CodeBlock, survivors: &Survivors) {
        if self.name.clear_if_dead(survivors) {
            self.cache.reset();
        }
        self.cache.reconcile(owner, survivors);
    }
}

// SAFETY: integers and a pointer.
unsafe impl Entry for NewObject {
    fn visit(&self, visitor: &mut Visitor) {
        if !self.shape.get().is_none() {
            visitor.append_shape(self.shape.get());
        }
    }
}

// SAFETY: an integer.
unsafe impl Entry for ArrayAllocationProfile {
    fn reconcile(&self, _: &CodeBlock, _: &Survivors) {
        // SAFETY: a code block that has run since the last collection is reconciled.
        unsafe { self.update() };
    }
}

/// Kept alive.
#[repr(transparent)]
pub struct StrongCell(pub crate::Field<Option<Gc<JsCell>>>);

// SAFETY: zero is `None`.
unsafe impl Entry for StrongCell {
    fn visit(&self, visitor: &mut Visitor) {
        crate::Trace::trace(&self.0, visitor);
    }
}

macro_rules! entry_types {
    ($($kind:ident => $type:ty,)*) => {
        $(const _: () = assert!(size_of::<$type>() == MetadataKind::$kind.words() as usize * 8);)*

        macro_rules! for_each_entry {
            ($code_block:expr, |$entry:ident| $body:expr) => {
                for (kind, range) in $code_block.body().metadata().ranges() {
                    match kind {
                        $(MetadataKind::$kind => {
                            // SAFETY: the words of that kind.
                            for $entry in unsafe { $code_block.entries::<$type>(range) } {
                                $body;
                            }
                        })*
                    }
                }
            };
        }
    };
}

entry_types! {
    Value => ValueProfile,
    UnaryArithmetic => ArithmeticProfile,
    BinaryArithmetic => ArithmeticProfile,
    Branch => Flags,
    LazyCache => AccessCases,
    LazyCacheAndValue => CasesAndValue,
    GetById => GetById,
    GetByIdDirect => GetById,
    GetLength => GetLength,
    PutById => StoreCache,
    GetByVal => GetByVal,
    ArrayAccess => ArrayAccess,
    GetPrivateName => GetPrivateName,
    PutPrivateName => PutPrivateName,
    PrivateBrand => PutPrivateName,
    GetGlobal => GetGlobal,
    PutGlobal => GlobalAccess,
    GetImport => GetImport,
    ResolveDynamic => Word,
    GetDynamic => ValueProfile,
    PutDynamic => Word,
    Call => Call,
    SuperConstruct => SuperConstruct,
    ToThis => ToThis,
    CachedCallee => WeakCell,
    NewObject => NewObject,
    NewArray => ArrayAllocationProfile,
    NewArrayWithSpecies => NewArrayWithSpecies,
    SharedRegExp => StrongCell,
    ForIn => ForIn,
    ForInGet => ForInGet,
    IterationModes => Flags,
    IteratorNext => IteratorNext,
    Catch => Counter,
    Coverage => Word,
}

#[derive(Default)]
pub(crate) struct SideTables {
    pub(crate) cases: RefCell<CaseArena>,
    /// The types seen by the profiles of the arguments, and then by those of the instructions.
    predictions: OnceCell<Box<[Cell<SpeculatedType>]>>,
    pub(crate) has_transitions: Cell<bool>,
    #[expect(clippy::vec_box, reason = "handed out by address")]
    call_targets: RefCell<Vec<Box<[CallTargetCount; CallProfile::MAX_TARGETS]>>>,
}

impl CodeBlock {
    /// # Safety
    /// The words are entries of that type.
    unsafe fn entries<T: Entry>(&self, words: core::ops::Range<u32>) -> &[T] {
        let count = words.len() * size_of::<u64>() / size_of::<T>();
        // SAFETY: the caller's.
        unsafe {
            let first = self.metadata_ptr().add(words.start as usize);
            core::slice::from_raw_parts(first.cast(), count)
        }
    }

    /// # Safety
    /// The operand of an instruction of this code whose entry is of that type.
    #[inline(always)]
    pub unsafe fn metadata<T: Entry>(&self, word: u32) -> &T {
        // SAFETY: the caller's.
        unsafe { &*self.metadata_ptr().add(word as usize).cast() }
    }

    pub(crate) fn allocate_call_targets(&self) -> *const CallTargetCount {
        let targets = Box::new(core::array::from_fn(|_| CallTargetCount {
            target: WeakCell(Cell::new(WeakCell::NONE)),
            count: Cell::new(0),
        }));
        let first = targets.as_ptr();
        self.caches.call_targets.borrow_mut().push(targets);
        first
    }

    fn value_profiles(&self) -> impl Iterator<Item = &ValueProfile> {
        let ranges = self.body().metadata().ranges();
        let words = ranges
            .filter(|(kind, _)| kind.has_value_profile())
            .flat_map(|(kind, range)| range.step_by(kind.words() as usize));
        // SAFETY: what such an entry starts with.
        let of_instructions = words.map(|word| unsafe { self.metadata::<ValueProfile>(word) });
        self.argument_profiles().iter().chain(of_instructions)
    }

    pub fn update_predictions(&self) {
        let predictions = self
            .caches
            .predictions
            .get_or_init(|| self.value_profiles().map(|_| Cell::default()).collect());
        for (prediction, profile) in predictions.iter().zip(self.value_profiles()) {
            prediction.set(prediction.get() | profile.take_type());
        }
    }

    fn prediction_at(&self, index: usize) -> &Cell<SpeculatedType> {
        if self.caches.predictions.get().is_none() {
            self.update_predictions();
        }
        &self.caches.predictions.get().expect("just made")[index]
    }

    /// Argument 0 is `this`. As of the last `update_predictions`.
    pub fn argument_prediction(&self, index: u32) -> &Cell<SpeculatedType> {
        assert!(index < self.parameters());
        self.prediction_at(index as usize)
    }

    /// Of the entry that starts at `word`. As of the last `update_predictions`.
    pub fn prediction(&self, word: u32) -> &Cell<SpeculatedType> {
        let mut index = self.parameters();
        for (kind, range) in self.body().metadata().ranges() {
            if range.contains(&word) {
                assert!(kind.has_value_profile());
                index += (word - range.start) / kind.words();
                break;
            }
            if kind.has_value_profile() {
                index += range.len() as u32 / kind.words();
            }
        }
        self.prediction_at(index as usize)
    }

    fn is_warm(&self) -> bool {
        self.tier() != Tier::Interpreter
            || self.execution_counter().get() > -options().threshold_for_baseline / 2
    }
}

pub(super) fn visit(code_block: &CodeBlock, visitor: &mut Visitor) {
    for_each_entry!(code_block, |entry| entry.visit(visitor));
    if code_block.caches.has_transitions.get() {
        visitor.constrain_by_current_cell();
    }
}

pub(super) fn visit_transitions(code_block: &CodeBlock, visitor: &mut Visitor) {
    for_each_entry!(code_block, |entry| entry.visit_transitions(visitor));
}

pub(super) fn reconcile_weak_references(code_block: &CodeBlock, survivors: &Survivors) {
    if code_block.is_warm() {
        code_block.update_predictions();
    } else {
        for profile in code_block.value_profiles() {
            profile.clear_if_dead(survivors);
        }
    }
    for_each_entry!(code_block, |entry| entry.reconcile(code_block, survivors));
}
