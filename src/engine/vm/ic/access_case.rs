use core::cell::Cell;
use core::mem::offset_of;
use core::ptr::NonNull;

use bun_engine_text::{Identifier, PropertyName};

use super::{PrototypeLookup, is_live};
use crate::code::metadata::Entry;
use crate::heap::Survivors;
use crate::{CodeBlock, Gc, ShapeId, Visitor, Vm};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum AccessKind {
    /// The last of a list: takes the slow path, which may add a case.
    End,
    /// The last of a list: asks the cache of the `Vm`.
    Megamorphic,
    /// The last of a list: takes the slow path, which has given up on caching.
    Generic,
    LoadOwn,
    /// The constant of the lookup.
    LoadConstant,
    /// The slot of the holder of the lookup.
    LoadInherited,
    CallOwnGetter,
    CallInheritedGetter,
    /// `pointer` is the function. It gets the receiver.
    CallCustomAccessorGetter,
    /// It gets the holder of the lookup, or the object itself if the lookup has none.
    CallCustomValueGetter,
    /// For any shape.
    ArrayLength,
    StringLength,
    /// What the getter of the library returns, while the lookup is valid. The length of the buffer is fixed.
    TypedArrayLength,
    TypedArrayByteLength,
    TypedArrayByteOffset,
    /// `pointer` is the namespace, `second_pointer` the address of the slot.
    LoadModuleBinding,
    /// `pointer` is the address of the `GlobalBinding`, and `new_shape` that of the target of the proxy.
    LoadGlobalBinding,
    /// From the target of the proxy, which has `new_shape`.
    LoadOwnThroughGlobalProxy,
    Replace,
    Transition,
    TransitionAndGrow,
    CallOwnSetter,
    CallInheritedSetter,
    CallCustomAccessorSetter,
    CallCustomValueSetter,
    HasOwn,
    /// While the lookup is valid.
    HasInherited,
    HasNot,
    Delete,
    DeleteMissing,
    DeleteNonConfigurable,
    /// `pointer` is the prototype. The answer is the constant of the lookup.
    Instanceof,
}

impl AccessKind {
    pub fn is_last(self) -> bool {
        self as u8 <= AccessKind::Generic as u8
    }
}

/// Whether the name is a part of the instruction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Fixed,
    Variable,
}

/// One way in which an access has gone. Code that has called out does not look at it again.
#[repr(C)]
pub struct AccessCase {
    /// What baseline code jumps to.
    pub(super) code: Cell<usize>,
    pub(super) next: Cell<Option<NonNull<AccessCase>>>,
    pub(super) shape: Cell<ShapeId>,
    pub(super) byte_offset: i32,
    pub(super) new_shape: ShapeId,
    pub(super) kind: Cell<AccessKind>,
    /// Of the last: how often caching has failed.
    pub(super) failures: Cell<u8>,
    /// Of the last: whether the slow path ran after the site had cases.
    pub(super) did_miss: Cell<bool>,
    /// Kept alive. Zero if there is none.
    pub(super) lookup: usize,
    pub(super) pointer: usize,
    pub(super) second_pointer: usize,
    /// For `Key::Variable`.
    pub(super) key: Option<Identifier>,
}

impl AccessCase {
    pub const OFFSET_OF_CODE: usize = offset_of!(AccessCase, code);
    pub const OFFSET_OF_NEXT: usize = offset_of!(AccessCase, next);
    pub const OFFSET_OF_SHAPE: usize = offset_of!(AccessCase, shape);
    pub const OFFSET_OF_BYTE_OFFSET: usize = offset_of!(AccessCase, byte_offset);
    pub const OFFSET_OF_NEW_SHAPE: usize = offset_of!(AccessCase, new_shape);
    pub const OFFSET_OF_LOOKUP: usize = offset_of!(AccessCase, lookup);
    pub const OFFSET_OF_POINTER: usize = offset_of!(AccessCase, pointer);
    pub const OFFSET_OF_SECOND_POINTER: usize = offset_of!(AccessCase, second_pointer);
    pub const OFFSET_OF_KEY: usize = offset_of!(AccessCase, key);

    pub(super) fn new(kind: AccessKind, shape: ShapeId) -> AccessCase {
        AccessCase {
            code: Cell::new(0),
            next: Cell::new(None),
            shape: Cell::new(shape),
            byte_offset: 0,
            new_shape: ShapeId::NONE,
            kind: Cell::new(kind),
            failures: Cell::new(0),
            did_miss: Cell::new(false),
            lookup: 0,
            pointer: 0,
            second_pointer: 0,
            key: None,
        }
    }

    pub(super) fn with_lookup(mut self, lookup: Gc<PrototypeLookup>) -> AccessCase {
        self.lookup = lookup.address();
        self
    }

    pub(super) fn with_offset(mut self, offset: crate::PropertyOffset) -> AccessCase {
        self.byte_offset = offset.byte_offset() as i32;
        self
    }

    #[inline(always)]
    pub fn kind(&self) -> AccessKind {
        self.kind.get()
    }

    #[inline(always)]
    pub fn shape(&self) -> ShapeId {
        self.shape.get()
    }

    #[inline(always)]
    pub fn new_shape(&self) -> ShapeId {
        self.new_shape
    }

    #[inline(always)]
    pub fn byte_offset(&self) -> i32 {
        self.byte_offset
    }

    pub fn pointer(&self) -> usize {
        self.pointer
    }

    pub fn second_pointer(&self) -> usize {
        self.second_pointer
    }

    pub fn key(&self) -> Option<PropertyName<'_>> {
        self.key.as_ref().map(Identifier::name)
    }

    pub fn did_miss(&self) -> bool {
        self.did_miss.get()
    }

    #[inline(always)]
    pub fn lookup(&self) -> Option<Gc<PrototypeLookup>> {
        // SAFETY: visited with the code block.
        (self.lookup != 0).then(|| unsafe { Gc::from_address(self.lookup) })
    }

    /// # Safety
    /// The kind is one that has a lookup.
    #[inline(always)]
    pub unsafe fn lookup_unchecked(&self) -> Gc<PrototypeLookup> {
        // SAFETY: as above.
        unsafe { Gc::from_address(self.lookup) }
    }

    #[inline(always)]
    pub fn next(&self) -> Option<&AccessCase> {
        // SAFETY: in the arena of the same code block.
        self.next.get().map(|next| unsafe { next.as_ref() })
    }

    #[inline(always)]
    pub fn matches(&self, shape: ShapeId, key: usize) -> bool {
        self.shape() == shape && self.key.as_ref().map_or(0, |it| it.name().address()) == key
    }

    #[inline(always)]
    pub fn is_valid(&self) -> bool {
        self.lookup().is_none_or(|lookup| lookup.is_valid())
    }

    fn is_live(&self, survivors: &Survivors) -> bool {
        let points_to_cell = matches!(
            self.kind(),
            AccessKind::LoadModuleBinding | AccessKind::Instanceof
        );
        // SAFETY: only the address is looked at.
        let pointer = || unsafe { Gc::<crate::JsCell>::from_address(self.pointer) };
        is_live(self.shape(), survivors)
            && is_live(self.new_shape, survivors)
            && self.is_valid()
            && (!points_to_cell || survivors.contains(pointer()))
    }
}

#[derive(Default)]
pub(crate) struct CaseArena {
    #[expect(clippy::vec_box, reason = "handed out by address")]
    all: Vec<Box<AccessCase>>,
    free: Vec<NonNull<AccessCase>>,
}

impl CaseArena {
    fn allocate(&mut self, case: AccessCase) -> NonNull<AccessCase> {
        if let Some(mut free) = self.free.pop() {
            // SAFETY: in no list, and nothing looks at a case after it has called out.
            unsafe { *free.as_mut() = case };
            return free;
        }
        let mut boxed = Box::new(case);
        let address = NonNull::from(&mut *boxed);
        self.all.push(boxed);
        address
    }
}

/// A list that is empty or ends with a case for which `is_last` holds. While it is empty it counts misses.
#[repr(transparent)]
pub struct AccessCases(Cell<usize>);

impl AccessCases {
    pub const MAX_CASES: usize = 8;
    const MAX_FAILURES: u8 = 8;
    /// From here on the word is an address.
    pub const FIRST_ADDRESS: usize = 256;

    #[inline(always)]
    pub fn bits(&self) -> usize {
        self.0.get()
    }

    #[inline(always)]
    pub(super) fn set_bits(&self, bits: usize) {
        self.0.set(bits);
    }

    #[inline(always)]
    pub fn first(&self) -> Option<&AccessCase> {
        let bits = self.bits();
        // SAFETY: in the arena of the code block that has this metadata.
        (bits >= Self::FIRST_ADDRESS).then(|| unsafe { &*core::ptr::with_exposed_provenance(bits) })
    }

    #[inline(always)]
    pub fn iter(&self) -> impl Iterator<Item = &AccessCase> {
        core::iter::successors(self.first(), |case| case.next())
    }

    #[inline(always)]
    pub fn find(&self, shape: ShapeId, key: usize) -> Option<&AccessCase> {
        self.iter().find(|case| case.matches(shape, key))
    }

    pub fn last(&self) -> Option<&AccessCase> {
        self.iter().last()
    }

    pub fn has_run(&self) -> bool {
        self.bits() != 0
    }

    /// Code that runs once is not worth a cache.
    #[inline]
    pub(super) fn should_cache(&self) -> bool {
        match self.last() {
            Some(last) => {
                last.did_miss.set(true);
                last.kind() == AccessKind::End
            }
            None => self.0.replace(1) != 0,
        }
    }

    #[inline]
    pub(super) fn is_megamorphic(&self) -> bool {
        self.first()
            .is_some_and(|first| first.kind() == AccessKind::Megamorphic)
    }

    fn push(&self, vm: &Vm, owner: &CodeBlock, case: AccessCase, key: Key) {
        case.code.set((vm.hooks().access_handler)(vm, &case, key));
        let next = self.first().map(NonNull::from);
        case.next.set(next);
        let case = owner.caches.cases.borrow_mut().allocate(case);
        self.0.set(case.as_ptr().expose_provenance());
    }

    fn ensure_last(&self, vm: &Vm, owner: &CodeBlock, key: Key) -> &AccessCase {
        if self.first().is_none() {
            let end = AccessCase::new(AccessKind::End, ShapeId::NONE);
            self.push(vm, owner, end, key);
        }
        self.last().expect("just made")
    }

    fn retain(&self, owner: &CodeBlock, keep: impl Fn(&AccessCase) -> bool) {
        let mut previous: Option<&AccessCase> = None;
        for case in self.iter() {
            if case.kind().is_last() || keep(case) {
                previous = Some(case);
                continue;
            }
            match previous {
                Some(previous) => previous.next.set(case.next.get()),
                None => self.0.set(case.next.get().map_or(0, |it| it.addr().get())),
            }
            let mut arena = owner.caches.cases.borrow_mut();
            arena.free.push(NonNull::from(case));
        }
    }

    fn end_with(&self, vm: &Vm, owner: &CodeBlock, kind: AccessKind, key: Key) {
        let last = self.ensure_last(vm, owner, key);
        last.kind.set(kind);
        last.code.set((vm.hooks().access_handler)(vm, last, key));
    }

    /// `overflow` is what the site becomes if it has too many cases.
    pub(super) fn add(
        &self,
        vm: &Vm,
        owner: &CodeBlock,
        mut case: AccessCase,
        variable_key: Option<PropertyName<'_>>,
        overflow: AccessKind,
    ) {
        let key = if variable_key.is_some() {
            Key::Variable
        } else {
            Key::Fixed
        };
        let key_address = variable_key.map_or(0, PropertyName::address);
        self.ensure_last(vm, owner, key);
        self.retain(owner, |old| {
            old.is_valid()
                && !(old.matches(case.shape(), key_address) && old.pointer == case.pointer)
        });
        if self.iter().count() > Self::MAX_CASES {
            if overflow == AccessKind::Megamorphic {
                self.retain(owner, |_| false);
            }
            return self.end_with(vm, owner, overflow, key);
        }
        if !case.new_shape.is_none() {
            owner.caches.has_transitions.set(true);
        }
        case.key = variable_key.map(PropertyName::to_identifier);
        self.push(vm, owner, case, key);
        owner.write_barrier_for_cell();
    }

    pub(super) fn did_fail_to_cache(&self, vm: &Vm, owner: &CodeBlock, key: Key) {
        let last = self.ensure_last(vm, owner, key);
        last.did_miss.set(true);
        last.failures.update(|count| count + 1);
        if last.failures.get() >= Self::MAX_FAILURES {
            self.end_with(vm, owner, AccessKind::Generic, key);
        }
    }

    /// For when the code block gets baseline code.
    pub fn update_handlers(&self, vm: &Vm, key: Key) {
        for case in self.iter() {
            case.code.set((vm.hooks().access_handler)(vm, case, key));
        }
    }
}

pub(super) fn visit_transition(from: ShapeId, to: ShapeId, visitor: &mut Visitor) {
    if !to.is_none() && !from.is_none() && visitor.is_address_marked(from.address()) {
        visitor.append_shape(to);
    }
}

// SAFETY: an integer.
unsafe impl Entry for AccessCases {
    fn visit(&self, visitor: &mut Visitor) {
        for case in self.iter() {
            // SAFETY: zero, or a cell that has been kept alive so far.
            unsafe { visitor.append_hidden_word(case.lookup as u64) };
        }
    }

    fn visit_transitions(&self, visitor: &mut Visitor) {
        for case in self.iter() {
            visit_transition(case.shape(), case.new_shape, visitor);
        }
    }

    fn reconcile(&self, owner: &CodeBlock, survivors: &Survivors) {
        self.retain(owner, |case| case.is_live(survivors));
    }
}
