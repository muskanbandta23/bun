use core::sync::atomic::{Ordering, fence};

use crate::heap::{HeapAnalyzer, barrier};
use crate::{
    CellKind, CellType, Class, ElementKind, Field, Gc, ImmutableArray, JsCell,
    JsDestructibleObject, JsFinalObject, JsObject, JsString, PropertyOffset, Storage, TypeFlags,
    Value, Visitor,
};

#[inline(always)]
pub(crate) fn visit_cell(cell: Gc<JsCell>, visitor: &mut Visitor) {
    // SAFETY: each type means that class and no other, and the last is the method of the class of the cell.
    unsafe {
        match cell.cell_type() {
            CellType::String => cell.cast_unchecked::<JsString>().visit_fibers(visitor),
            CellType::FinalObject => cell.cast_unchecked::<JsFinalObject>().trace_fields(visitor),
            CellType::Array => cell.cast_unchecked::<JsObject>().trace_fields(visitor),
            _ => {
                let shape = cell.shape_id_concurrently().address();
                let class = Gc::<crate::Shape>::from_address(shape).class();
                (class.ops().visit_children)(&cell, visitor);
            }
        }
    }
}

pub(crate) unsafe extern "C" fn destroy_by_class_field(cell: *mut JsCell) {
    // SAFETY: the space has only `JsDestructibleObject`s.
    unsafe {
        let class = cell
            .byte_add(JsDestructibleObject::OFFSET_OF_CLASS)
            .cast::<&'static Class>()
            .read();
        (class.ops().destroy)(cell);
    }
}

pub(crate) unsafe extern "C" fn destroy_by_shape(cell: *mut JsCell) {
    // SAFETY: the space has only cells whose shape is destroyed after them.
    unsafe { ((*cell).class().ops().destroy)(cell) }
}

#[inline(always)]
pub(super) fn append_hidden(fields: &[Field<Option<Value>>], visitor: &mut Visitor) {
    for field in fields {
        // SAFETY: the object keeps it alive.
        unsafe { visitor.append_hidden_word(field.word_concurrently()) };
    }
}

impl JsObject {
    /// The highest offset in use, unless the object was caught in the middle of a change.
    #[inline(always)]
    pub(super) fn visit_shape_and_storage(&self, visitor: &mut Visitor) -> Option<PropertyOffset> {
        let id = self.shape_id_concurrently();
        visitor.append_shape(id);
        let is_fenced = barrier::is_fenced();
        if is_fenced {
            if id.is_nuked() {
                visitor.did_race();
                return None;
            }
            fence(Ordering::Acquire);
        }
        // SAFETY: not nuked, and kept alive by the cell.
        let shape = unsafe { id.decode() };
        let max_offset = shape.max_offset();
        if is_fenced {
            fence(Ordering::Acquire);
        }
        let storage = self.storage_concurrently();
        if storage.is_none() {
            return Some(max_offset);
        }
        let mode = shape.instance_indexing_mode();
        let capacity = max_offset.out_of_line_capacity();
        let flags = self.inline_flags_concurrently();
        if is_fenced {
            fence(Ordering::Acquire);
            if self.shape_id_concurrently() != id
                || shape.max_offset() != max_offset
                || self.storage_concurrently() != storage
            {
                visitor.did_race();
                return None;
            }
        }
        // SAFETY: the shape and the storage went together at one time, and memory is not freed during marking.
        unsafe {
            if mode.is_copy_on_write() {
                let array = ImmutableArray::from_storage(storage);
                visitor.append(array);
                return Some(max_offset);
            }
            let mut base = storage.base(capacity);
            if flags.contains(TypeFlags::HAS_PRE_CAPACITY) {
                base -= storage.pre_capacity(capacity) * size_of::<u64>();
            }
            visitor.mark_auxiliary(base);
            append_hidden(storage.slots(max_offset.out_of_line_size()), visitor);
            match mode.kind() {
                ElementKind::Contiguous => {
                    let length = storage.public_length_concurrently();
                    append_hidden(storage.values(length), visitor);
                }
                ElementKind::Sparse => {
                    visitor.append_word(storage.value_at(0).word_concurrently());
                }
                _ => {}
            }
        }
        Some(max_offset)
    }

    pub fn analyze_properties(&self, analyzer: &mut HeapAnalyzer) {
        let this = Gc::from(self.as_cell());
        for (name, offset, _) in self.shape().properties().iter() {
            if let Some(cell) = self.get_direct_at(offset).as_cell() {
                analyzer.property_edge(this, cell, name);
            }
        }
        if self.element_kind() == ElementKind::Contiguous {
            for index in 0..self.public_length() {
                if let Some(cell) = self.try_get_dense(index).and_then(Value::as_cell) {
                    analyzer.index_edge(this, cell, index);
                }
            }
        }
    }

    pub(crate) fn storage_size(&self) -> usize {
        if self.indexing_mode().is_copy_on_write() {
            return 0;
        }
        let has_elements = self.indexing_mode().has_elements();
        // SAFETY: it has elements.
        let words = has_elements.then(|| unsafe { self.element_words() });
        Storage::size(self.shape().out_of_line_capacity(), words)
    }
}
