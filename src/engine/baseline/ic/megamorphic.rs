//! Lookups in the tables of the `Vm`.

use bun_engine_asm::prelude::*;
use bun_engine_text::{OFFSET_OF_SYMBOL_HASH, StringImpl};
use bun_engine_vm::Vm;
use bun_engine_vm::caches::{LoadEntry, LookupTables, StoreEntry};

use super::abi::*;
use super::handlers::return_from_store;
use crate::emit::{Emit, FALSE, UNDEFINED, at, shape_id};
use crate::registers::*;

const ENTRY_SHIFT: u8 = size_of::<LoadEntry>().ilog2() as u8;

/// Leaves the address of the entry for `SHAPE` and `NAME` in `CASE`.
fn find(m: &mut MacroAssembler, primary: usize, secondary: usize, miss: Label) {
    let (symbol, hashed) = (m.new_label(), m.new_label());
    let (second, found) = (m.new_label(), m.new_label());
    m.load32(RESULT, at(NAME, StringImpl::OFFSET_OF_FLAGS));
    m.branch_test32(TestCond::NonZero, RESULT, StringImpl::IS_SYMBOL, symbol);
    m.shr32(RESULT, RESULT, StringImpl::FLAG_COUNT as u8);
    m.bind(hashed);
    m.shr32(CASE, SHAPE, 4);
    m.xor_shifted(
        Width::W32,
        CASE,
        CASE,
        SHAPE,
        bun_engine_asm::Shift::RightLogical,
        15,
    );
    m.add32(RESULT, RESULT, CASE);
    let check = |m: &mut MacroAssembler, table: usize, count: usize, otherwise: Label| {
        m.and32(RESULT, RESULT, count - 1);
        m.shl32(RESULT, RESULT, ENTRY_SHIFT);
        m.load64(CASE, at(VM, Vm::OFFSET_OF_LOOKUP_TABLES));
        m.compute_address(
            CASE,
            Address::indexed(CASE, RESULT, Scale::One, table as i32),
        );
        let key = at(CASE, LoadEntry::OFFSET_OF_SHAPE_AND_EPOCH);
        m.branch32(Cond::Ne, SHAPE, key, otherwise);
        m.branch64(
            Cond::Ne,
            NAME,
            at(CASE, LoadEntry::OFFSET_OF_NAME),
            otherwise,
        );
        m.load32(RESULT, at(VM, Vm::OFFSET_OF_PROTOTYPE_CHAIN_EPOCH));
        m.branch32(Cond::Ne, RESULT, key.plus(4), otherwise);
    };
    check(m, primary, LoadEntry::PRIMARY_COUNT, second);
    m.jump(found);
    m.bind(symbol);
    m.load32(RESULT, at(NAME, OFFSET_OF_SYMBOL_HASH));
    m.jump(hashed);
    m.bind(second);
    m.add32(RESULT, SHAPE, NAME);
    m.xor_shifted(
        Width::W32,
        RESULT,
        RESULT,
        RESULT,
        bun_engine_asm::Shift::RightLogical,
        9,
    );
    m.shr32(RESULT, RESULT, 3);
    check(m, secondary, LoadEntry::SECONDARY_COUNT, miss);
    m.bind(found);
}

fn find_load(m: &mut MacroAssembler, miss: Label) {
    let (first, second) = (
        LookupTables::OFFSET_OF_LOADS,
        LookupTables::OFFSET_OF_SECONDARY_LOADS,
    );
    find(m, first, second, miss);
}

pub(super) fn load(m: &mut MacroAssembler, miss: Label) {
    m.branch64(Cond::Ne, VALUE, BASE, miss);
    find_load(m, miss);
    m.branch8(
        Cond::Ne,
        at(CASE, LoadEntry::OFFSET_OF_ACCESSOR_KIND),
        0,
        miss,
    );
    m.load32_signed64(SHAPE, at(CASE, LoadEntry::OFFSET_OF_BYTE_OFFSET));
    m.load64(CASE, at(CASE, LoadEntry::OFFSET_OF_HOLDER));
    m.select_test64(TestCond::Zero, CASE, CASE, -1, BASE, CASE);
    m.slot_base(RESULT, CASE, SHAPE);
    m.load64(RESULT, Address::indexed(RESULT, SHAPE, Scale::One, 0));
    let done = m.new_label();
    m.branch_if_not_empty(RESULT, done);
    m.move32(RESULT, UNDEFINED);
    m.bind(done);
    m.ret();
}

pub(super) fn has(m: &mut MacroAssembler, miss: Label) {
    find_load(m, miss);
    m.load8(RESULT, at(CASE, LoadEntry::OFFSET_OF_IS_PRESENT));
    m.or32(RESULT, RESULT, FALSE);
    m.ret();
}

pub(super) fn store(m: &mut MacroAssembler, miss: Label, write_barrier: Label) {
    let (first, second) = (
        LookupTables::OFFSET_OF_STORES,
        LookupTables::OFFSET_OF_SECONDARY_STORES,
    );
    find(m, first, second, miss);
    m.branch8(
        Cond::Ne,
        at(CASE, StoreEntry::OFFSET_OF_NEEDS_MORE_SLOTS),
        0,
        miss,
    );
    m.load32_signed64(SHAPE, at(CASE, StoreEntry::OFFSET_OF_BYTE_OFFSET));
    m.slot_base(RESULT, BASE, SHAPE);
    m.store64(Address::indexed(RESULT, SHAPE, Scale::One, 0), VALUE);
    m.load32(RESULT, at(CASE, StoreEntry::OFFSET_OF_NEW_SHAPE));
    m.store32(shape_id(BASE), RESULT);
    return_from_store(m, write_barrier);
}

const _: () = {
    assert!(size_of::<StoreEntry>() == size_of::<LoadEntry>());
    assert!(StoreEntry::OFFSET_OF_NAME == LoadEntry::OFFSET_OF_NAME);
    assert!(StoreEntry::OFFSET_OF_SHAPE_AND_EPOCH == LoadEntry::OFFSET_OF_SHAPE_AND_EPOCH);
    assert!(StoreEntry::PRIMARY_COUNT == LoadEntry::PRIMARY_COUNT);
    assert!(StoreEntry::SECONDARY_COUNT == LoadEntry::SECONDARY_COUNT);
};
