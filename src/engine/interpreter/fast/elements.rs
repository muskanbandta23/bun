use bun_engine_bytecode::op;
use bun_engine_vm::code::metadata::{ArrayAccess, GetByVal};
use bun_engine_vm::{ArrayModes, ElementKind, IndexingMode, JsObject, Value};

use super::barrier_and_next;
use super::properties::{get_by_name, put_by_name, put_by_name_direct};
use crate::machine::{EMPTY, boolean, cell, go, handler, next, slow};

const INT32: u8 = ElementKind::Int32 as u8;
const DOUBLE: u8 = ElementKind::Double as u8;
const CONTIGUOUS: u8 = ElementKind::Contiguous as u8;

handler! {
    fn get_by_val(m, op: op::GetByVal) {
        let (base, key) = (m.get(op.base), m.get(op.key));
        if !m.is_int32(key) {
            go!(m, get_by_name::<W>)
        }
        if !m.is_cell(base) {
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

handler! {
    fn in_by_val(m, op: op::InByVal) {
        let (base, key) = (m.get(op.base), m.get(op.key));
        if !m.is_cell(base) || !m.is_int32(key) || cell::<JsObject>(base).try_get_dense(key as u32).is_none() {
            slow!(m, op::InByVal)
        }
        m.entry::<ArrayAccess>(op.metadata).array.record_shape(&cell(base));
        m.set(op.dst, boolean(true));
        next!(m, op)
    }
}

macro_rules! stores {
    ($($name:ident, $by_name:ident, $Op:ident;)*) => { handler! {$(
        fn $name(m, op: op::$Op) {
            let (base, key, value) = (m.get(op.base), m.get(op.key), m.get(op.value));
            if !m.is_int32(key) {
                go!(m, $by_name::<W>)
            }
            if !m.is_cell(base) {
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
    put_by_val, put_by_name, PutByVal;
    put_by_val_direct, put_by_name_direct, PutByValDirect;
}
