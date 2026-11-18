use bun_engine_bytecode::op;
use bun_engine_vm::code::metadata::{ArrayAccess, GetByVal};
use bun_engine_vm::{ArrayModes, ElementKind, IndexingMode, JsObject, Value};

use super::barrier_and_next;
use crate::machine::{EMPTY, cell, handler, next, slow};

const INT32: u8 = ElementKind::Int32 as u8;
const DOUBLE: u8 = ElementKind::Double as u8;
const CONTIGUOUS: u8 = ElementKind::Contiguous as u8;

handler! {
    fn get_by_val(m, op: op::GetByVal) {
        let (base, key) = (m.get(op.base), m.get(op.key));
        if !m.is_cell(base) || !m.is_int32(key) {
            slow!(m, op::GetByVal)
        }
        let (object, index) = (cell::<JsObject>(base), key as u32);
        let kind = object.indexing_mode().bits() & IndexingMode::KIND_MASK;
        let value = if kind == CONTIGUOUS || kind == INT32 {
            let storage = object.storage();
            if index >= storage.public_length() {
                slow!(m, op::GetByVal)
            }
            Value::bits_of(storage.value_at(index).get())
        } else if kind == DOUBLE {
            let storage = object.storage();
            if index >= storage.public_length() {
                slow!(m, op::GetByVal)
            }
            let number = storage.double_at(index).get();
            if number.is_nan() { EMPTY } else { m.box_double(number) }
        } else {
            EMPTY
        };
        if value == EMPTY {
            slow!(m, op::GetByVal)
        }
        let entry = m.entry::<GetByVal>(op.metadata);
        entry.array.record_shape(&object);
        entry.value.record_bits(value);
        m.set(op.dst, value);
        next!(m, op)
    }
}

macro_rules! stores {
    ($($name:ident, $Op:ident;)*) => { handler! {$(
        fn $name(m, op: op::$Op) {
            let (base, key, value) = (m.get(op.base), m.get(op.key), m.get(op.value));
            if !m.is_cell(base) || !m.is_int32(key) {
                slow!(m, op::$Op)
            }
            let (object, index) = (cell::<JsObject>(base), key as u32);
            let mode = object.indexing_mode().bits() & IndexingMode::KIND_AND_WRITABILITY_MASK;
            let word = if mode == CONTIGUOUS || (mode == INT32 && m.is_int32(value)) {
                value
            } else if mode == DOUBLE && let Some(number) = m.to_double(value) && !number.is_nan() {
                number.to_bits()
            } else {
                slow!(m, op::$Op)
            };
            let (storage, profile) = (object.storage(), &m.entry::<ArrayAccess>(op.metadata).array);
            if index >= storage.public_length() {
                if index >= storage.vector_length() {
                    slow!(m, op::$Op)
                }
                profile.record(ArrayModes::STORED_TO_HOLE);
                storage.set_public_length(index + 1);
            }
            profile.record_shape(&object);
            storage.value_at(index).set_word_without_barrier(word);
            barrier_and_next!(m, op: op::$Op, object, value)
        }
    )*} };
}

stores! {
    put_by_val, PutByVal;
    put_by_val_direct, PutByValDirect;
}
