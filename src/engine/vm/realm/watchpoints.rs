//! Parts of the standard library that fast paths count on being as they were defined.

use core::cell::RefCell;

use crate::{
    Cx, Gc, GlobalObject, InlineWatchpointSet, JsCell, PropertyCondition, Vm, Watchpoint,
    WatchpointState, watch_condition,
};

macro_rules! realm_watchpoints {
    ($($(#[$doc:meta])* $name:ident,)*) => {
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        #[repr(u8)]
        pub enum RealmWatchpoint {
            $($(#[$doc])* $name,)*
        }

        impl RealmWatchpoint {
            pub const COUNT: usize = [$(stringify!($name),)*].len();
        }
    };
}

realm_watchpoints! {
    /// Arrays are not made with `SLOW_PUT`.
    NotHavingABadTime,
    NoObjectMasqueradesAsUndefined,
    /// No global variable has been made read-only.
    GlobalVariablesAreWritable,
    /// `RegExp.prototype.compile` has not given any object another pattern.
    NoRegExpIsRecompiled,
    /// `Array.prototype` and `Object.prototype` have no elements, and their prototypes are the original ones.
    ArrayPrototypeChainIsSane,
    ObjectPrototypeChainIsSane,
    StringPrototypeChainIsSane,
    ArrayIteratorProtocol,
    MapIteratorProtocol,
    SetIteratorProtocol,
    StringIteratorProtocol,
    TypedArrayIteratorProtocol,
    GeneratorProtocol,
    AsyncGeneratorProtocol,
    ArraySpecies,
    ArrayJoin,
    ArrayToString,
    ArrayIsConcatSpreadable,
    ArrayBufferSpecies,
    SharedArrayBufferSpecies,
    TypedArraySpecies,
    PromiseThen,
    PromiseResolve,
    PromiseSpecies,
    RegExpPrimordialProperties,
    RegExpSpecies,
    RegExpPrototypeTest,
    StringSymbolMatch,
    StringSymbolMatchAll,
    StringSymbolReplace,
    StringSymbolSearch,
    StringSymbolSplit,
    StringToString,
    StringValueOf,
    NumberToString,
    /// `Symbol.toPrimitive`, `valueOf`, `toString`, `toJSON` and `toISOString` of `Date.prototype`.
    DateConversions,
    MapSet,
    SetAdd,
    SetPrimordialProperties,
    /// `Object.prototype` has none of the fields of a property descriptor.
    PropertyDescriptorFields,
}

pub(super) struct RealmWatchpoints {
    /// Clear until the conditions have been looked at.
    sets: [InlineWatchpointSet; RealmWatchpoint::COUNT],
    watchers: RefCell<Vec<Watchpoint>>,
}

impl RealmWatchpoints {
    pub(super) const OFFSET_OF_SETS: usize = core::mem::offset_of!(RealmWatchpoints, sets);

    pub(super) fn new() -> RealmWatchpoints {
        let watchpoints = RealmWatchpoints {
            sets: [const { InlineWatchpointSet::new(WatchpointState::Clear) };
                RealmWatchpoint::COUNT],
            watchers: RefCell::default(),
        };
        for fired_directly in [
            RealmWatchpoint::NotHavingABadTime,
            RealmWatchpoint::NoObjectMasqueradesAsUndefined,
            RealmWatchpoint::GlobalVariablesAreWritable,
            RealmWatchpoint::NoRegExpIsRecompiled,
        ] {
            watchpoints.sets[fired_directly as usize].start_watching();
        }
        watchpoints
    }
}

impl GlobalObject {
    /// As it is, which may be before its conditions have been looked at.
    #[inline(always)]
    pub fn watchpoints_unchecked(&self, which: RealmWatchpoint) -> &InlineWatchpointSet {
        &self.realm_watchpoints().sets[which as usize]
    }

    #[inline]
    pub fn watchpoints(&self, vm: &Vm, which: RealmWatchpoint) -> &InlineWatchpointSet {
        let set = self.watchpoints_unchecked(which);
        if set.state() == WatchpointState::Clear {
            self.install_watchpoint(vm, which);
        }
        set
    }

    #[inline]
    pub fn holds(&self, vm: &Vm, which: RealmWatchpoint) -> bool {
        self.watchpoints(vm, which).is_still_valid()
    }

    #[cold]
    #[inline(never)]
    fn install_watchpoint(&self, vm: &Vm, which: RealmWatchpoint) {
        // SAFETY: as in `initialize_slot`.
        let cx: &Cx = unsafe { Cx::from_global(self.into()) };
        let set = self.watchpoints_unchecked(which);
        let invalidate = move |vm: &Vm, owner: Gc<JsCell>| {
            let global = owner.cast::<GlobalObject>();
            global.watchpoints_unchecked(which).invalidate(vm);
        };
        let conditions = (vm.hooks().realm_watchpoint_conditions)(cx, which);
        let watchers = conditions.and_then(|conditions| {
            let watch = |condition: &PropertyCondition| {
                // SAFETY: the realm owns the watchpoint, and keeps the objects of its library alive.
                unsafe { watch_condition(self, condition, invalidate) }
            };
            conditions.iter().map(watch).collect::<Option<Vec<_>>>()
        });
        match watchers {
            Some(watchers) => {
                set.start_watching();
                let mut all = self.realm_watchpoints().watchers.borrow_mut();
                all.extend(watchers);
            }
            None => set.invalidate(vm),
        }
    }

    #[inline(always)]
    fn is_watched(&self, which: RealmWatchpoint) -> bool {
        self.watchpoints_unchecked(which).state() == WatchpointState::Watched
    }

    #[inline(always)]
    pub fn is_having_a_bad_time(&self) -> bool {
        !self.is_watched(RealmWatchpoint::NotHavingABadTime)
    }

    #[inline(always)]
    pub fn array_prototype_chain_is_sane(&self) -> bool {
        self.is_watched(RealmWatchpoint::ArrayPrototypeChainIsSane)
    }

    #[inline(always)]
    pub fn array_iterator_protocol_is_pristine(&self) -> bool {
        self.is_watched(RealmWatchpoint::ArrayIteratorProtocol)
    }
}

impl Cx {
    #[inline]
    pub fn holds(&self, which: RealmWatchpoint) -> bool {
        self.global().holds(self, which)
    }
}
