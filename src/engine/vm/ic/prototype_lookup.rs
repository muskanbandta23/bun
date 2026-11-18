use core::cell::{Cell, RefCell};
use core::mem::offset_of;

use bun_engine_macros::cell;
use bun_engine_text::{Identifier, PropertyName};
use smallvec::SmallVec;

use crate::heap::{PlainKey, Survivors, WeakValueMap};
use crate::{
    ConditionSet, Gc, InlineWatchpointSet, JsCell, JsObject, PropertyOffset, TypeFlags, Value,
    Visitor, Vm, Watchpoint, WatchpointState, vm::Root,
};

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Query {
    Get,
    /// That nothing on the chain has a say in a store.
    SetMiss,
    /// The name is not used.
    Instanceof {
        prototype: usize,
    },
}

/// What a prototype chain says about a name, for as long as that stays true. All caches that ask the same share it.
#[cell(flags = TypeFlags::SHAPE_IS_IMMORTAL, visit_children, reconcile_weak_references)]
pub struct PrototypeLookup {
    base: JsCell,
    /// The bits of what the slot of the holder has if that is known to stay, of undefined if there is no holder, and
    /// of the answer for `instanceof`. Otherwise, and once it is invalid, zero.
    constant: Cell<u64>,
    /// Zero once it is invalid.
    holder: Cell<usize>,
    byte_offset: i32,
    is_valid: Cell<bool>,
    dependents: InlineWatchpointSet,
    name: Identifier,
    guards: RefCell<(ConditionSet, SmallVec<[Watchpoint; 2]>)>,
    /// What else the answer depends on being alive.
    operand: usize,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    first: usize,
    name: usize,
    query: Query,
}

// SAFETY: integers.
unsafe impl PlainKey for Key {}

pub(crate) struct PrototypeLookups {
    map: WeakValueMap<Key, PrototypeLookup>,
    of_empty_chain: Root<PrototypeLookup>,
}

impl Default for PrototypeLookups {
    fn default() -> PrototypeLookups {
        PrototypeLookups {
            map: WeakValueMap::default(),
            of_empty_chain: Root::new(),
        }
    }
}

impl crate::services::Service for PrototypeLookups {
    fn visit(&self, visitor: &mut Visitor) {
        self.of_empty_chain.visit(visitor);
    }

    fn reconcile_weak_references(&self, _: &Vm, survivors: &Survivors) {
        self.map.prune(survivors);
    }
}

impl PrototypeLookup {
    pub const OFFSET_OF_CONSTANT: usize = offset_of!(PrototypeLookup, constant);
    pub const OFFSET_OF_HOLDER: usize = offset_of!(PrototypeLookup, holder);
    pub const OFFSET_OF_BYTE_OFFSET: usize = offset_of!(PrototypeLookup, byte_offset);
    pub const OFFSET_OF_IS_VALID: usize = offset_of!(PrototypeLookup, is_valid);

    fn create(
        vm: &Vm,
        name: PropertyName<'_>,
        conditions: ConditionSet,
        constant: Option<Value>,
        holder: Option<(Gc<JsObject>, PropertyOffset)>,
        operand: usize,
    ) -> Option<Gc<PrototypeLookup>> {
        let lookup = vm.allocate::<PrototypeLookup>(PrototypeLookupInit {
            base: vm.shape_for::<PrototypeLookup>(),
            constant: Cell::new(Value::bits_of(constant)),
            holder: Cell::new(holder.map_or(0, |it| it.0.address())),
            byte_offset: holder.map_or(0, |it| it.1.byte_offset() as i32),
            is_valid: Cell::new(true),
            dependents: InlineWatchpointSet::new(WatchpointState::Watched),
            name: name.to_identifier(),
            guards: RefCell::default(),
            operand,
        });
        let invalidate = |vm: &Vm, owner: Gc<JsCell>| owner.cast::<Self>().invalidate(vm);
        // SAFETY: owned by the lookup, which drops them in `reconcile_weak_references`.
        let watchpoints = unsafe { conditions.watch(&lookup, invalidate) }?;
        lookup.guards.replace((conditions, watchpoints));
        Some(lookup)
    }

    fn cached(
        vm: &Vm,
        first: Gc<JsObject>,
        name: PropertyName<'_>,
        query: Query,
        create: impl FnOnce() -> Option<Gc<PrototypeLookup>>,
    ) -> Option<Gc<PrototypeLookup>> {
        let map = &vm.services.prototype_lookups.map;
        let key = Key {
            first: first.address(),
            name: name.address(),
            query,
        };
        if let Some(lookup) = map.get(vm, &key).filter(|it| it.is_valid()) {
            return Some(lookup);
        }
        let _defer = vm.heap.defer_collection();
        let lookup = create()?;
        map.insert(key, lookup);
        Some(lookup)
    }

    /// Always valid, and undefined.
    pub fn of_empty_chain(vm: &Vm) -> Gc<PrototypeLookup> {
        let root = &vm.services.prototype_lookups.of_empty_chain;
        root.get().unwrap_or_else(|| {
            let (name, conditions) = (PropertyName::EMPTY, ConditionSet::default());
            let lookup = Self::create(vm, name, conditions, Some(Value::UNDEFINED), None, 0);
            root.set(lookup);
            lookup.expect("nothing to watch")
        })
    }

    pub fn for_miss(
        vm: &Vm,
        first: Option<Gc<JsObject>>,
        name: PropertyName<'_>,
    ) -> Option<Gc<PrototypeLookup>> {
        let Some(first) = first else {
            return Some(Self::of_empty_chain(vm));
        };
        Self::cached(vm, first, name, Query::Get, || {
            let conditions = ConditionSet::for_miss(vm, Some(first), name)?;
            Self::create(vm, name, conditions, Some(Value::UNDEFINED), None, 0)
        })
        .filter(|lookup| lookup.holder().is_none())
    }

    pub fn for_hit(
        vm: &Vm,
        first: Gc<JsObject>,
        holder: Gc<JsObject>,
        name: PropertyName<'_>,
    ) -> Option<Gc<PrototypeLookup>> {
        Self::cached(vm, first, name, Query::Get, || {
            let presence = ConditionSet::for_hit(vm, Some(first), holder, name)?;
            let Some((offset, _)) = holder.shape().get(name) else {
                let place = Some((holder, PropertyOffset::INVALID));
                return Self::create(vm, name, presence, None, place, 0);
            };
            let (place, value) = (Some((holder, offset)), holder.get_direct_at(offset));
            ConditionSet::for_equivalent_hit(vm, Some(first), holder, name)
                .and_then(|it| Self::create(vm, name, it, Some(value), place, 0))
                .or_else(|| Self::create(vm, name, presence, None, place, 0))
        })
        .filter(|lookup| lookup.holder() == Some(holder))
    }

    pub fn for_setter_miss(
        vm: &Vm,
        first: Option<Gc<JsObject>>,
        name: PropertyName<'_>,
    ) -> Option<Gc<PrototypeLookup>> {
        let Some(first) = first else {
            return Some(Self::of_empty_chain(vm));
        };
        Self::cached(vm, first, name, Query::SetMiss, || {
            let conditions = ConditionSet::for_setter_miss(vm, Some(first), name)?;
            Self::create(vm, name, conditions, Some(Value::UNDEFINED), None, 0)
        })
    }

    pub fn for_instanceof(
        vm: &Vm,
        first: Gc<JsObject>,
        prototype: Gc<JsObject>,
        answer: bool,
    ) -> Option<Gc<PrototypeLookup>> {
        let (name, operand) = (PropertyName::EMPTY, prototype.address());
        let query = Query::Instanceof { prototype: operand };
        Self::cached(vm, first, name, query, || {
            let conditions = ConditionSet::for_instanceof(vm, first, prototype)?;
            Self::create(vm, name, conditions, Some(answer.into()), None, operand)
        })
    }

    #[inline(always)]
    pub fn is_valid(&self) -> bool {
        self.is_valid.get()
    }

    #[inline(always)]
    pub fn constant(&self) -> Option<Value> {
        // SAFETY: the holder has it, or it is no cell.
        unsafe { Value::from_bits_or_empty(self.constant.get()) }
    }

    #[inline(always)]
    pub fn holder(&self) -> Option<Gc<JsObject>> {
        let address = self.holder.get();
        // SAFETY: cleared when it dies.
        (address != 0).then(|| unsafe { Gc::from_address(address) })
    }

    pub fn byte_offset(&self) -> i32 {
        self.byte_offset
    }

    /// What the slot of the holder has now. The property is in its shape.
    #[inline(always)]
    pub(super) fn stored_value(&self) -> Option<Value> {
        let holder = self.holder()?;
        // SAFETY: a condition says that the holder has the slot.
        unsafe { super::slot_at(&holder, self.byte_offset) }.get()
    }

    pub fn name(&self) -> PropertyName<'_> {
        self.name.name()
    }

    /// Fires when it becomes invalid.
    pub fn dependents(&self) -> &InlineWatchpointSet {
        &self.dependents
    }

    fn invalidate(&self, vm: &Vm) {
        self.is_valid.set(false);
        self.constant.set(0);
        self.holder.set(0);
        self.guards.take();
        self.dependents.invalidate(vm);
    }

    fn visit_children(&self, visitor: &mut Visitor) {
        visitor.reconcile_current_cell_later();
    }

    fn reconcile_weak_references(&self, survivors: &Survivors) {
        // SAFETY: only the address is looked at.
        let operand = || unsafe { Gc::<JsCell>::from_address(self.operand) };
        if self.is_valid()
            && !(self.guards.borrow().0.is_live(survivors)
                && (self.operand == 0 || survivors.contains(operand())))
        {
            self.invalidate(survivors.vm());
        }
    }
}
