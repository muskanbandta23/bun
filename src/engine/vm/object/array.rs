use bun_alloc::AllocError;
use bun_engine_macros::{cell, object_ops};
use bun_engine_text::{PropertyName, names};

use super::elements::{MIN_SPARSE_INDEX, is_dense_enough, store_unchecked};
use crate::heap::barrier;
use crate::{
    Attributes, CellKind, Cx, DeletePropertySlot, DontEnumMode, ElementKind, Elements, Gc,
    GlobalObject, ImmutableArray, IndexingMode, JsObject, JsObjectInit, JsResult,
    PropertyDescriptor, PropertyNameList, PropertySlot, PutPropertySlot, Shape, ShapeFlags,
    ShapeSpec, Storage, TransitionKind, TypeFlags, Value, Vm, errors,
};

#[cell(name = "Array", cell_type = Array..=DerivedArray, ops, export = "engine_class_Array")]
pub struct JsArray {
    base: JsObject,
}

/// `new Array(n)` is sparse from here on.
const MIN_SPARSE_CONSTRUCTION_LENGTH: u32 = 1 << 27;
/// Up to here, moving the elements is cheaper than what it takes to avoid that.
const MAX_MOVED_BY_SHIFT: u32 = 32;

fn to_array_length(cx: &mut Cx, value: Value) -> JsResult<u32> {
    let length = value.to_uint32(cx)?;
    if value.to_number(cx)? != f64::from(length) {
        return Err(cx.throw_range_error(errors::INVALID_ARRAY_LENGTH));
    }
    Ok(length)
}

#[object_ops]
impl JsArray {
    fn get_own_property_slot(
        &self,
        cx: &mut Cx,
        name: PropertyName<'_>,
        slot: &mut PropertySlot,
    ) -> JsResult<bool> {
        if name == names::length {
            let mut attributes = Attributes::DONT_DELETE | Attributes::DONT_ENUM;
            attributes.set(Attributes::READ_ONLY, !self.is_length_writable());
            slot.set_value(self, attributes, self.length().into());
            return Ok(true);
        }
        self.ordinary_get_own_property_slot(cx, name, slot)
    }

    fn put(
        &self,
        cx: &mut Cx,
        name: PropertyName<'_>,
        value: Value,
        slot: &mut PutPropertySlot,
    ) -> JsResult<bool> {
        if name != names::length {
            return self.ordinary_put(cx, name, value, slot);
        }
        if !self.is_length_writable() {
            return cx.reject(slot.is_strict(), "Array length is not writable");
        }
        if !slot.is_receiver(self) {
            return JsObject::define_property_on_receiver(cx, name, value, slot);
        }
        let length = to_array_length(cx, value)?;
        self.set_length(cx, length, slot.is_strict())
    }

    fn delete_property(
        &self,
        cx: &mut Cx,
        name: PropertyName<'_>,
        slot: &mut DeletePropertySlot,
    ) -> JsResult<bool> {
        if name == names::length {
            return Ok(false);
        }
        self.ordinary_delete_property(cx, name, slot)
    }

    fn own_special_property_names(
        &self,
        _: &mut Cx,
        list: &mut PropertyNameList,
        mode: DontEnumMode,
    ) -> JsResult<()> {
        if mode == DontEnumMode::Include {
            list.add(names::length);
        }
        Ok(())
    }

    fn define_own_property(
        &self,
        cx: &mut Cx,
        name: PropertyName<'_>,
        descriptor: &PropertyDescriptor,
        should_throw: bool,
    ) -> JsResult<bool> {
        if name != names::length {
            return self.ordinary_define_own_property(cx, name, descriptor, should_throw);
        }
        let mut descriptor = *descriptor;
        let mut length = self.length();
        if let Some(value) = descriptor.value() {
            length = to_array_length(cx, value)?;
            descriptor.set_value(length.into());
        }
        let mut attributes = Attributes::DONT_DELETE | Attributes::DONT_ENUM;
        attributes.set(Attributes::READ_ONLY, !self.is_length_writable());
        let current = PropertyDescriptor::data(self.length().into(), attributes);
        if let Some(message) = descriptor.conflict_with(cx, &current)? {
            return cx.reject(should_throw, message);
        }
        let result = if descriptor.value().is_some() && length != self.length() {
            self.set_length(cx, length, should_throw)
        } else {
            Ok(true)
        };
        if descriptor.has_writable() && !descriptor.writable() && self.is_length_writable() {
            self.non_property_transition(cx, TransitionKind::MakeLengthReadOnly);
        }
        result
    }
}

impl JsArray {
    pub(crate) fn create_shape(
        vm: &Vm,
        global: Gc<GlobalObject>,
        prototype: Value,
        mode: IndexingMode,
    ) -> Gc<Shape> {
        let spec = ShapeSpec::of::<JsArray>().with_indexing_mode(mode);
        Shape::create(vm, Some(global), prototype, spec)
    }

    /// # Safety
    /// As for `JsObjectInit::with_storage`.
    #[inline(always)]
    pub unsafe fn create_with_storage(vm: &Vm, shape: Gc<Shape>, storage: Storage) -> Gc<JsArray> {
        // SAFETY: the caller's.
        let base = unsafe { JsObjectInit::with_storage(shape, storage) };
        vm.allocate::<JsArray>(JsArrayInit { base })
    }

    #[inline(always)]
    fn writable_dense_kind(shape: &Shape) -> ElementKind {
        let mode = shape.instance_indexing_mode();
        assert!(mode.kind().is_dense() && !mode.is_copy_on_write() && !shape.is_dictionary());
        mode.kind()
    }

    /// Holes up to `length`, and room for at least `vector_length` elements.
    #[inline]
    pub fn try_create(
        vm: &Vm,
        shape: Gc<Shape>,
        length: u32,
        vector_length: u32,
    ) -> Result<Gc<JsArray>, AllocError> {
        let kind = Self::writable_dense_kind(&shape);
        if length >= MIN_SPARSE_CONSTRUCTION_LENGTH {
            let array = Self::try_create(vm, shape, 0, 0)?;
            array.convert_to_sparse(vm)?;
            // SAFETY: not dense.
            unsafe { array.storage().set_public_length(length) };
            return Ok(array);
        }
        let capacity = shape.out_of_line_capacity();
        let wanted = length.max(vector_length.min(Storage::MAX_VECTOR_LENGTH));
        let vector_length = Storage::optimal_vector_length(capacity, wanted);
        let storage = Storage::try_create(vm, capacity, kind, length, vector_length)?;
        // SAFETY: made for the shape, and without any cell.
        unsafe {
            for slot in storage.slots(shape.out_of_line_size()) {
                slot.set_without_barrier(Some(Value::UNDEFINED));
            }
            Ok(Self::create_with_storage(vm, shape, storage))
        }
    }

    /// The kind of the elements becomes as general as the values need.
    #[inline]
    pub fn try_from_values_with_shape(
        vm: &Vm,
        shape: Gc<Shape>,
        values: &[Value],
    ) -> Result<Gc<JsArray>, AllocError> {
        if values.len() > Storage::MAX_VECTOR_LENGTH as usize {
            return Err(AllocError);
        }
        let array = Self::try_create(vm, shape, 0, values.len() as u32)?;
        array.append_values(vm, values)?;
        Ok(array)
    }

    pub fn from_immutable(cx: &Cx, elements: Gc<ImmutableArray>) -> JsResult<Gc<JsArray>> {
        let global = cx.global();
        if global.is_having_a_bad_time() {
            let length = elements.len() as usize;
            let array = cx.new_array_with_capacity(ElementKind::Contiguous, length)?;
            for (index, value) in (0..).zip(elements.iter()) {
                array
                    .store_element(cx, index, value)
                    .map_err(|_| cx.throw_out_of_memory())?;
            }
            return Ok(array);
        }
        let mode = IndexingMode::array(elements.kind()).copy_on_write();
        let shape = global.original_array_shape(mode);
        // SAFETY: what such a shape describes. `elements` is alive until the array has it.
        let array = unsafe { Self::create_with_storage(cx, shape, elements.storage()) };
        elements.keep_alive();
        Ok(array)
    }

    #[inline(always)]
    pub fn length(&self) -> u32 {
        self.public_length()
    }

    #[inline(always)]
    pub fn is_length_writable(&self) -> bool {
        !self.indexing_mode().is_slow_put() || !self.shape().has(ShapeFlags::LENGTH_IS_READ_ONLY)
    }

    pub fn set_length(&self, cx: &mut Cx, length: u32, should_throw: bool) -> JsResult<bool> {
        let old = self.length();
        if !self.is_length_writable() {
            return cx.reject(should_throw, errors::READ_ONLY_PROPERTY_WRITE);
        }
        if length == old {
            return Ok(true);
        }
        let kind = self.element_kind();
        if !kind.is_dense() {
            return self.set_sparse_length(cx, length, should_throw);
        }
        if length > old {
            if length > self.vector_length()
                && (length > Storage::MAX_VECTOR_LENGTH
                    || (length >= MIN_SPARSE_INDEX
                        && !is_dense_enough(length, self.count_dense_elements())))
            {
                return self.set_sparse_length(cx, length, should_throw);
            }
            self.ensure_public_length(cx, length)
                .map_err(|_| cx.throw_out_of_memory())?;
            return Ok(true);
        }
        let mut kept = length;
        if self.shape().has(ShapeFlags::ELEMENTS_ARE_SEALED) {
            let last = (length..old).rfind(|&index| self.try_get_dense(index).is_some());
            kept = last.map_or(length, |last| last + 1);
        }
        self.truncate(cx, kept);
        if kept != length {
            return cx.reject(should_throw, errors::UNABLE_TO_DELETE_PROPERTY);
        }
        Ok(true)
    }

    /// The elements are dense, and `length` is at most the current one.
    fn truncate(&self, vm: &Vm, length: u32) {
        const WORTH_RECLAIMING: u32 = 64;
        self.ensure_writable_elements(vm);
        let (kind, capacity) = (self.element_kind(), self.shape().out_of_line_capacity());
        // SAFETY: all ranges are inside the vectors.
        unsafe {
            let old = self.storage();
            if old.public_length() - length > length.max(WORTH_RECLAIMING) {
                let vector_length = Storage::optimal_vector_length(capacity, length);
                if let Ok(new) = Storage::try_allocate(vm, 0, capacity, Some(vector_length)) {
                    let count = capacity + 1 + length as usize;
                    old.copy_words_to(new, -1 - capacity as isize, count);
                    new.set_lengths(length, vector_length);
                    new.clear_elements(kind, length, vector_length);
                    return self.set_storage(new);
                }
            }
            old.clear_elements(kind, length, old.public_length());
            old.set_public_length(length);
        }
    }

    fn set_sparse_length(&self, cx: &mut Cx, length: u32, should_throw: bool) -> JsResult<bool> {
        let sparse = self
            .convert_to_sparse(cx)
            .map_err(|_| cx.throw_out_of_memory())?;
        let mut kept = length;
        if length < self.length() {
            let entries = sparse.sorted_entries();
            for &(index, entry) in entries.iter().rev().take_while(|entry| entry.0 >= length) {
                if !entry.attributes.is_configurable() {
                    kept = index + 1;
                    break;
                }
                sparse.remove(index);
            }
        }
        // SAFETY: not dense.
        unsafe { self.storage().set_public_length(kept) };
        if kept != length {
            return cx.reject(should_throw, errors::UNABLE_TO_DELETE_PROPERTY);
        }
        Ok(true)
    }

    /// `Array.prototype.push` with one argument.
    #[inline(always)]
    pub fn push(&self, cx: &mut Cx, value: Value) -> JsResult<()> {
        if self.try_set_dense(self.length(), value) {
            return Ok(());
        }
        self.push_slow(cx, value)
    }

    #[inline(never)]
    fn push_slow(&self, cx: &mut Cx, value: Value) -> JsResult<()> {
        let length = self.length();
        self.put_by_index(cx, length, value, true)?;
        if length == u32::MAX {
            return Err(cx.throw_range_error(errors::LENGTH_EXCEEDS_MAXIMUM));
        }
        if self.indexing_mode().is_slow_put() || !self.element_kind().is_dense() {
            self.set_length(cx, length + 1, true)?;
        }
        Ok(())
    }

    /// CreateDataPropertyOrThrow at the end, for an array that the engine is filling.
    #[inline(always)]
    pub fn append(&self, cx: &mut Cx, value: Value) -> JsResult<()> {
        self.create_data_index(cx, self.length().into(), value, true)
            .map(|_| ())
    }

    /// Defines them at the end. Panics unless the elements are dense and the array can grow.
    #[inline]
    pub fn append_values(&self, vm: &Vm, values: &[Value]) -> Result<(), AllocError> {
        assert!(
            !self
                .shape()
                .has(ShapeFlags::DID_PREVENT_EXTENSIONS | ShapeFlags::LENGTH_IS_READ_ONLY)
        );
        let mut kind = self.element_kind();
        if kind != ElementKind::Contiguous {
            kind = values
                .iter()
                .map(|&value| ElementKind::for_value(value))
                .fold(kind, ElementKind::max);
        }
        self.ensure_element_kind(vm, kind);
        let start = self.length();
        let end = u32::try_from(start as usize + values.len()).map_err(|_| AllocError)?;
        self.ensure_public_length(vm, end)?;
        let storage = self.storage();
        for (index, &value) in (start..end).zip(values) {
            // SAFETY: inside, the kind can represent the value, and the barrier follows.
            unsafe { store_unchecked(storage, kind, index, value) };
        }
        self.write_barrier_for_cell();
        Ok(())
    }

    /// `Array.prototype.pop`
    #[inline]
    pub fn pop(&self, cx: &mut Cx) -> JsResult<Value> {
        if self.has_plain_writable_elements() {
            let Some(last) = self.length().checked_sub(1) else {
                return Ok(Value::UNDEFINED);
            };
            if let Some(value) = self.try_get_dense(last) {
                // SAFETY: there is an element at that index.
                unsafe {
                    let storage = self.storage();
                    storage.clear_elements(self.element_kind(), last, last + 1);
                    storage.set_public_length(last);
                }
                return Ok(value);
            }
        }
        let Some(last) = self.length().checked_sub(1) else {
            self.set_length(cx, 0, true)?;
            return Ok(Value::UNDEFINED);
        };
        let value = self.get_index(cx, last.into())?;
        self.delete_index_or_throw(cx, last.into())?;
        self.set_length(cx, last, true)?;
        Ok(value)
    }

    /// Whether elements can be moved around as words: nothing would see a hole being read or written.
    #[inline]
    pub fn can_move_elements(&self, vm: &Vm) -> bool {
        let mode = self.indexing_mode();
        if !mode.kind().is_dense() || mode.is_slow_put() {
            return false;
        }
        self.ensure_writable_elements(vm);
        // SAFETY: only reads.
        !self.holes_must_forward_to_prototype()
            || unsafe { self.elements_unchecked() }.is_some_and(|it| !it.has_holes())
    }

    /// Removes `count` elements from `start` on. False, having done nothing, if it cannot be done in place.
    pub fn try_remove_range(&self, vm: &Vm, start: u32, count: u32) -> bool {
        if !self.can_move_elements(vm) {
            return false;
        }
        let length = self.length();
        assert!(start <= length && count <= length - start);
        let (kind, tail) = (self.element_kind(), length - start - count);
        let storage = self.storage();
        // SAFETY: all ranges are inside the vector, and the words that the header moves to have been vacated.
        unsafe {
            if count != 0 && tail > MAX_MOVED_BY_SHIFT && start < tail && !barrier::is_fenced() {
                let capacity = self.shape().out_of_line_capacity();
                let pre_capacity = self.pre_capacity(capacity) + count as usize;
                let vector_length = storage.vector_length() - count;
                storage.move_elements(0, count, start);
                let moved = storage.shift_header(capacity, count as isize);
                moved.set_lengths(length - count, vector_length);
                moved.set_pre_capacity(capacity, pre_capacity);
                self.nuke_shape_and_set_storage(moved, true);
            } else {
                storage.move_elements(start + count, start, tail);
                storage.clear_elements(kind, length - count, length);
                storage.set_public_length(length - count);
            }
        }
        self.write_barrier_for_cell();
        true
    }

    fn pre_capacity(&self, capacity: usize) -> usize {
        if self.inline_flags().contains(TypeFlags::HAS_PRE_CAPACITY) {
            // SAFETY: the flag says that there is a count.
            unsafe { self.storage().pre_capacity(capacity) }
        } else {
            0
        }
    }

    /// Makes `count` holes at `start`. False, having done nothing, if it cannot be done in place.
    pub fn try_insert_holes(&self, vm: &Vm, start: u32, count: u32) -> Result<bool, AllocError> {
        if !self.can_move_elements(vm) {
            return Ok(false);
        }
        let length = self.length();
        assert!(start <= length);
        let Some(new_length) = length
            .checked_add(count)
            .filter(|&it| it <= Storage::MAX_VECTOR_LENGTH)
        else {
            return Ok(false);
        };
        let (kind, tail) = (self.element_kind(), length - start);
        if tail <= MAX_MOVED_BY_SHIFT || start >= tail || barrier::is_fenced() {
            self.ensure_public_length(vm, new_length)?;
            // SAFETY: inside the new length.
            unsafe {
                self.storage().move_elements(start, start + count, tail);
                self.storage().clear_elements(kind, start, start + count);
            }
            self.write_barrier_for_cell();
            return Ok(true);
        }
        let capacity = self.shape().out_of_line_capacity();
        // SAFETY: the header moves into unused words of the same allocation.
        unsafe {
            if self.pre_capacity(capacity) < count as usize
                || self.vector_length() + count > Storage::MAX_VECTOR_LENGTH
            {
                let pre_capacity = (count + length / 2) as usize;
                let new = Storage::try_allocate(vm, pre_capacity, capacity, Some(length))?;
                let words = capacity + 1 + length as usize;
                self.storage()
                    .copy_words_to(new, -1 - capacity as isize, words);
                new.set_lengths(length, length);
                new.set_pre_capacity(capacity, pre_capacity);
                self.set_shape_and_storage(&self.shape(), new, true);
            }
            let left = self.pre_capacity(capacity) - count as usize;
            let vector_length = self.vector_length() + count;
            let moved = self.storage().shift_header(capacity, -(count as isize));
            moved.set_lengths(new_length, vector_length);
            moved.move_elements(count, 0, start);
            moved.clear_elements(kind, start, start + count);
            if left != 0 {
                moved.set_pre_capacity(capacity, left);
            }
            self.nuke_shape_and_set_storage(moved, left != 0);
        }
        self.write_barrier_for_cell();
        Ok(true)
    }

    /// A new array of the current realm with these elements of `source`, unless reading them might run code.
    pub fn try_fast_slice(
        cx: &Cx,
        source: &JsObject,
        start: u32,
        count: u32,
    ) -> JsResult<Option<Gc<JsArray>>> {
        let kind = source.element_kind();
        if !kind.is_dense()
            || u64::from(start) + u64::from(count) > u64::from(source.public_length())
            || source.holes_must_forward_to_prototype()
        {
            return Ok(None);
        }
        let shape = cx.global().array_shape(kind);
        let vector_length = Storage::optimal_vector_length(0, count);
        let storage = Storage::try_allocate(cx, 0, 0, Some(vector_length))
            .map_err(|_| cx.throw_out_of_memory())?;
        // SAFETY: inside both. `source` keeps the elements alive until the new array has them.
        let array = unsafe {
            let from = source.storage().elements_ptr().add(start as usize);
            core::ptr::copy_nonoverlapping(from, storage.elements_ptr(), count as usize);
            storage.set_lengths(count, vector_length);
            storage.clear_elements(kind, count, vector_length);
            Self::create_with_storage(cx, shape, storage)
        };
        source.as_object().keep_alive();
        Ok(Some(array))
    }

    /// Copies all elements of `source`, holes included, to `start`, which is at least the length. False, having done
    /// nothing, if that is not what defining them one by one would do.
    pub fn try_append_elements(&self, cx: &Cx, start: u32, source: &JsObject) -> JsResult<bool> {
        let (from, count) = (source.element_kind(), source.public_length());
        let mode = self.indexing_mode();
        let end = u64::from(start) + u64::from(count);
        if !from.is_dense()
            || !mode.kind().is_dense()
            || mode.is_slow_put()
            || start < self.length()
            || start - self.length() > MIN_SPARSE_INDEX
            || end > u64::from(Storage::MAX_VECTOR_LENGTH)
            || source.holes_must_forward_to_prototype()
        {
            return Ok(false);
        }
        self.ensure_element_kind(cx, from);
        self.ensure_public_length(cx, end as u32)
            .map_err(|_| cx.throw_out_of_memory())?;
        let (to, from) = (self.element_kind(), source.element_kind());
        // SAFETY: inside both, and `to` is at least as general as `from`.
        unsafe {
            let target = self.storage();
            if (to == ElementKind::Double) == (from == ElementKind::Double) {
                let first = target.elements_ptr().add(start as usize);
                barrier::move_words(source.storage().elements_ptr(), first, count as usize);
            } else {
                for index in 0..count {
                    if let Some(value) = source.try_get_dense(index) {
                        store_unchecked(target, to, start + index, value);
                    }
                }
            }
        }
        self.write_barrier_for_cell();
        Ok(true)
    }

    /// Whether iterating over it comes down to reading its elements.
    #[inline]
    pub fn is_iteration_pristine(&self) -> bool {
        let shape = self.shape();
        let Some(global) = shape.global() else {
            return false;
        };
        if !global.array_iterator_protocol_is_pristine() {
            return false;
        }
        global.is_original_array_shape(&shape)
            || (shape.prototype() == global.array_prototype().into()
                && !shape.may_intercept_indexed_access()
                && shape.get(bun_engine_text::symbols::iterator).is_none())
    }

    /// The elements as values, if there are no holes and they are stored that way.
    #[inline]
    pub fn packed_values<'a>(&'a self, no_gc: &'a crate::NoGc<'_>) -> Option<&'a [Value]> {
        match self.elements(no_gc)? {
            elements @ (Elements::Int32(values) | Elements::Contiguous(values))
                if !elements.has_holes() =>
            {
                // SAFETY: none is empty.
                Some(unsafe { core::slice::from_raw_parts(values.as_ptr().cast(), values.len()) })
            }
            _ => None,
        }
    }
}

const _: () = assert!(JsArray::FLAGS.contains(TypeFlags::OVERRIDES_PUT));
