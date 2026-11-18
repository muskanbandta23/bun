//! Where a walk over the cases starts.

use core::mem::offset_of;

use bun_engine_asm::prelude::*;
use bun_engine_vm::code::metadata::{ArrayAccess, GetByVal};
use bun_engine_vm::ic::{
    AccessCase, AccessCases, AccessKind, BOOLEAN_SHAPE, LoadCache, NUMBER_SHAPE, PrototypeLookup,
    StoreCache,
};
use bun_engine_vm::{ArrayProfile, CellType, JsString, JsSymbol, TypeFlags};

use super::abi::*;
use super::handlers::return_from_store;
use super::{Elements, load_element, megamorphic, store_element};
use crate::emit::{Emit, at, shape_id};

macro_rules! entries {
    ($($(#[$doc:meta])* $name:ident,)*) => {
        pub struct Entries<T> {
            $($(#[$doc])* pub $name: T,)*
        }

        impl<T> Entries<T> {
            pub(crate) fn map<U>(self, mut map: impl FnMut(T) -> U) -> Entries<U> {
                Entries { $($name: map(self.$name),)* }
            }
        }
    };
}

entries! {
    /// From a `LoadCache`.
    get_by_id,
    /// From `AccessCases`.
    get_by_id_with_this,
    /// From a `StoreCache`.
    put_by_id,
    /// From a `metadata::GetByVal`.
    get_by_val,
    get_by_val_with_this,
    /// From a `metadata::ArrayAccess`.
    put_by_val,
    /// From `AccessCases`. The base is an object.
    in_by_id,
    /// From `AccessCases`. The base is an object.
    in_by_val,
    /// From `AccessCases`. The base is an object.
    instance_of,
    /// From `AccessCases`, for any access by a private name, which is the key. The base is a cell.
    by_private_name,
}

struct Emitter<'a> {
    m: &'a mut MacroAssembler,
    miss: Label,
    write_barrier: Label,
}

impl Entries<Label> {
    pub(crate) fn emit(m: &mut MacroAssembler, write_barrier: Label) -> Entries<Label> {
        let miss = m.here();
        m.move32(RESULT, 0);
        m.ret();
        let mut emitter = Emitter {
            m,
            miss,
            write_barrier,
        };
        let mega_load = emitter.routine(|e| megamorphic::load(e.m, e.miss));
        let mega_has = emitter.routine(|e| megamorphic::has(e.m, e.miss));
        let mega_store = emitter.routine(|e| megamorphic::store(e.m, e.miss, e.write_barrier));
        Entries {
            get_by_id: emitter.routine(|e| e.get_by_id(mega_load)),
            get_by_id_with_this: emitter.routine(|e| {
                e.load_shape_of_cell();
                e.walk(at(ENTRY, 0), miss, false);
            }),
            put_by_id: emitter.routine(|e| e.put_by_id(mega_store)),
            get_by_val: emitter.routine(|e| e.get_by_val(mega_load)),
            get_by_val_with_this: emitter.routine(|e| {
                e.load_shape_of_cell();
                e.load_name();
                e.walk(at(ENTRY, offset_of!(GetByVal, cases)), miss, false);
            }),
            put_by_val: emitter.routine(|e| e.put_by_val(mega_store)),
            in_by_id: emitter.routine(|e| {
                e.m.load32(SHAPE, shape_id(BASE));
                e.walk(at(ENTRY, 0), mega_has, true);
            }),
            in_by_val: emitter.routine(|e| {
                e.m.load32(SHAPE, shape_id(BASE));
                e.m.branch_if_not_cell(KEY, miss);
                e.load_name();
                e.walk(at(ENTRY, 0), mega_has, false);
            }),
            instance_of: emitter.routine(|e| {
                e.m.load32(SHAPE, shape_id(BASE));
                e.walk(at(ENTRY, 0), miss, false);
            }),
            by_private_name: emitter.routine(|e| {
                e.m.load32(SHAPE, shape_id(BASE));
                e.m.load64(NAME, at(KEY, JsSymbol::OFFSET_OF_NAME));
                e.walk(at(ENTRY, 0), miss, false);
            }),
        }
    }
}

impl Emitter<'_> {
    fn routine(&mut self, emit: impl FnOnce(&mut Self)) -> Label {
        self.m.align(16, 15);
        let start = self.m.here();
        emit(self);
        start
    }

    fn load_shape_of_cell(&mut self) {
        self.m.branch_if_not_cell(BASE, self.miss);
        self.m.load32(SHAPE, shape_id(BASE));
    }

    /// From the key, if that is an atom or a symbol.
    fn load_name(&mut self) {
        let (symbol, done) = (self.m.new_label(), self.m.new_label());
        self.m.branch_if_not_cell(KEY, self.miss);
        self.m.branch_if_not_type(KEY, CellType::String, symbol);
        (self.m).branch_unless_flag(KEY, TypeFlags::PER_CELL_BIT, self.miss);
        self.m.load64(NAME, at(KEY, JsString::OFFSET_OF_FIBER));
        self.m.jump(done);
        self.m.bind(symbol);
        self.m.branch_if_not_type(KEY, CellType::Symbol, self.miss);
        self.m.load64(NAME, at(KEY, JsSymbol::OFFSET_OF_NAME));
        self.m.bind(done);
    }

    fn walk(&mut self, cases: Address, megamorphic: Label, is_key_the_name: bool) {
        self.m.load64(CASE, cases);
        (self.m).branch64(Cond::Below, CASE, AccessCases::FIRST_ADDRESS, self.miss);
        if megamorphic != self.miss {
            if is_key_the_name {
                self.m.move64(NAME, KEY);
            }
            let kind = at(CASE, AccessCase::OFFSET_OF_KIND);
            (self.m).branch8(Cond::Eq, kind, AccessKind::Megamorphic as i32, megamorphic);
        }
        self.m.jump_indirect(at(CASE, AccessCase::OFFSET_OF_CODE));
    }

    fn get_by_id(&mut self, megamorphic: Label) {
        let cache = |field: usize| at(ENTRY, field);
        let [primitive, has_shape, other, constant, list] =
            core::array::from_fn(|_| self.m.new_label());
        let m = &mut *self.m;
        m.move64(VALUE, BASE);
        m.branch_if_not_cell(BASE, primitive);
        m.load32(SHAPE, shape_id(BASE));
        m.bind(has_shape);
        m.load32_signed64(CASE, cache(LoadCache::OFFSET_OF_BYTE_OFFSET));
        m.branch32(Cond::Ne, SHAPE, cache(LoadCache::OFFSET_OF_SHAPE), other);
        m.branch_if_zero32(CASE, constant);
        m.slot_base(RESULT, BASE, CASE);
        m.load64(RESULT, Address::indexed(RESULT, CASE, Scale::One, 0));
        m.ret();
        m.bind(constant);
        m.load64(RESULT, cache(LoadCache::OFFSET_OF_CASES_OR_LOOKUP));
        m.load64(RESULT, at(RESULT, PrototypeLookup::OFFSET_OF_CONSTANT));
        m.ret();
        m.bind(primitive);
        m.move32(SHAPE, NUMBER_SHAPE.bits());
        m.branch_if_number(BASE, has_shape);
        m.move32(SHAPE, BOOLEAN_SHAPE.bits());
        m.branch_if_boolean(BASE, RESULT, has_shape);
        m.jump(self.miss);
        m.bind(other);
        m.branch_if_not_zero32(CASE, list);
        m.branch_if_not_zero32(cache(LoadCache::OFFSET_OF_SHAPE), self.miss);
        m.bind(list);
        let cell = m.new_label();
        let unless_primitive = m.new_label();
        self.walk(
            cache(LoadCache::OFFSET_OF_CASES_OR_LOOKUP),
            unless_primitive,
            true,
        );
        self.m.bind(unless_primitive);
        self.m.branch_if_cell(BASE, cell);
        self.m.jump(self.miss);
        self.m.bind(cell);
        self.m.jump(megamorphic);
    }

    fn put_by_id(&mut self, megamorphic: Label) {
        let cache = |field: usize| at(ENTRY, field);
        self.load_shape_of_cell();
        let (other, store) = (self.m.new_label(), self.m.new_label());
        let m = &mut *self.m;
        m.load32(NAME, cache(StoreCache::OFFSET_OF_NEW_SHAPE));
        m.branch32(Cond::Ne, SHAPE, cache(StoreCache::OFFSET_OF_SHAPE), other);
        m.select_test32(TestCond::Zero, SHAPE, NAME, -1, SHAPE, NAME);
        m.branch_if_zero32(NAME, store);
        m.load64(RESULT, cache(StoreCache::OFFSET_OF_CASES_OR_LOOKUP));
        let is_valid = at(RESULT, PrototypeLookup::OFFSET_OF_IS_VALID);
        m.branch8(Cond::Eq, is_valid, 0, self.miss);
        m.bind(store);
        m.load32_signed64(CASE, cache(StoreCache::OFFSET_OF_BYTE_OFFSET));
        m.slot_base(RESULT, BASE, CASE);
        m.store64(Address::indexed(RESULT, CASE, Scale::One, 0), VALUE);
        m.store32(shape_id(BASE), SHAPE);
        return_from_store(m, self.write_barrier);
        m.bind(other);
        m.branch_if_not_zero32(NAME, self.miss);
        self.walk(
            cache(StoreCache::OFFSET_OF_CASES_OR_LOOKUP),
            megamorphic,
            true,
        );
    }

    /// Goes to the code for the type of the typed array.
    fn dispatch_on_typed_array(&mut self, mut emit: impl FnMut(&mut Self, Elements)) {
        (self.m).branch_unless_type_in(BASE, CellType::TYPED_ARRAYS, RESULT, self.miss);
        let (first, last) = (
            CellType::TYPED_ARRAYS.first.0,
            CellType::TYPED_ARRAYS.last.0,
        );
        let labels: Vec<Label> = (first..=last)
            .map(|it| {
                if Elements::TYPED.contains(&CellType(it)) {
                    self.m.new_label()
                } else {
                    self.miss
                }
            })
            .collect();
        self.m.jump_table(RESULT, &labels);
        for cell_type in Elements::TYPED {
            self.m.bind(labels[usize::from(cell_type.0 - first)]);
            emit(self, Elements::Typed(cell_type));
        }
    }

    fn get_by_val(&mut self, megamorphic: Label) {
        let by_name = self.m.new_label();
        self.load_shape_of_cell();
        self.m.branch_if_not_int32(KEY, by_name);
        self.m.sign_extend32_to_64(NAME, KEY);
        let last_shape = offset_of!(GetByVal, array) + ArrayProfile::OFFSET_OF_LAST_SHAPE;
        let found = |e: &mut Self| {
            e.m.store32(at(ENTRY, last_shape), SHAPE);
            e.m.ret();
        };
        for kind in [
            Elements::Values,
            Elements::Double,
            Elements::String,
            Elements::DirectArguments,
        ] {
            let other = self.m.new_label();
            load_element(self.m, kind, other, self.miss);
            found(self);
            self.m.bind(other);
        }
        self.dispatch_on_typed_array(|e, kind| {
            load_element(e.m, kind, e.miss, e.miss);
            found(e);
        });
        self.m.bind(by_name);
        self.load_name();
        self.m.move64(VALUE, BASE);
        self.walk(at(ENTRY, offset_of!(GetByVal, cases)), megamorphic, false);
    }

    fn put_by_val(&mut self, megamorphic: Label) {
        let by_name = self.m.new_label();
        self.m.branch_if_not_cell(BASE, self.miss);
        self.m.branch_if_not_int32(KEY, by_name);
        self.m.sign_extend32_to_64(NAME, KEY);
        let profile = |field: usize| at(ENTRY, offset_of!(ArrayAccess, array) + field);
        let modes = profile(ArrayProfile::OFFSET_OF_MODES);
        let stored = |e: &mut Self| {
            e.m.load32(RESULT, shape_id(BASE));
            e.m.store32(profile(ArrayProfile::OFFSET_OF_LAST_SHAPE), RESULT);
        };
        for kind in [Elements::Values, Elements::Double] {
            let other = self.m.new_label();
            store_element(self.m, kind, VALUE, modes, other, self.miss);
            stored(self);
            return_from_store(self.m, self.write_barrier);
            self.m.bind(other);
        }
        self.dispatch_on_typed_array(|e, kind| {
            store_element(e.m, kind, VALUE, modes, e.miss, e.miss);
            stored(e);
            e.m.ret();
        });
        self.m.bind(by_name);
        self.m.load32(SHAPE, shape_id(BASE));
        self.load_name();
        self.walk(
            at(ENTRY, offset_of!(ArrayAccess, cases)),
            megamorphic,
            false,
        );
    }
}
