use core::ops::Range;

use bun_alloc::AllocError;

use crate::{
    Cx, ElementKind, ElementWriter, Gc, HoleMode, ImmutableArray, IndexingMode, JsArray,
    JsArrayInit, JsObject, JsObjectInit, JsResult, NoGc, Shape, Storage, Value, Vm,
};

/// `new Array(n)` is sparse from here on.
const MIN_SPARSE_CONSTRUCTION_LENGTH: u32 = 1 << 27;

#[inline(always)]
fn writable_dense_kind(shape: &Shape) -> ElementKind {
    let mode = shape.instance_indexing_mode();
    assert!(mode.kind().is_dense() && !mode.is_copy_on_write() && !shape.is_dictionary());
    mode.kind()
}

/// # Safety
/// New, with the slots that the shape calls for.
#[inline(always)]
unsafe fn initialize_slots(storage: Storage, shape: &Shape) {
    // SAFETY: the caller's, and no cell.
    unsafe {
        for slot in storage.slots(shape.out_of_line_size()) {
            slot.set_without_barrier(Some(Value::UNDEFINED));
        }
    }
}

impl JsArray {
    /// # Safety
    /// As for `JsObjectInit::with_storage`.
    #[inline(always)]
    pub unsafe fn create_with_storage(vm: &Vm, shape: Gc<Shape>, storage: Storage) -> Gc<JsArray> {
        // SAFETY: the caller's.
        let base = unsafe { JsObjectInit::with_storage(shape, storage) };
        vm.allocate::<JsArray>(JsArrayInit { base })
    }

    /// Holes up to `length`, and room for at least `vector_length` elements.
    #[inline]
    pub fn try_create(
        vm: &Vm,
        shape: Gc<Shape>,
        length: u32,
        vector_length: u32,
    ) -> Result<Gc<JsArray>, AllocError> {
        let kind = writable_dense_kind(&shape);
        if length >= MIN_SPARSE_CONSTRUCTION_LENGTH {
            return Self::try_create_sparse(vm, shape, length);
        }
        let capacity = shape.out_of_line_capacity();
        let wanted = length.max(vector_length.min(Storage::MAX_VECTOR_LENGTH));
        let vector_length = Storage::optimal_vector_length(capacity, wanted);
        let storage = Storage::try_create(vm, capacity, kind, length, vector_length)?;
        // SAFETY: made for the shape, and without any cell.
        unsafe {
            initialize_slots(storage, &shape);
            Ok(Self::create_with_storage(vm, shape, storage))
        }
    }

    #[cold]
    fn try_create_sparse(vm: &Vm, shape: Gc<Shape>, length: u32) -> Result<Gc<Self>, AllocError> {
        let array = Self::try_create(vm, shape, 0, 0)?;
        array.convert_to_sparse(vm)?;
        // SAFETY: not dense.
        unsafe { array.storage().set_public_length(length) };
        Ok(array)
    }

    /// # Safety
    /// `fill` allocates nothing in the heap.
    #[inline(always)]
    pub(crate) unsafe fn try_build_with_shape(
        vm: &Vm,
        shape: Gc<Shape>,
        room: usize,
        fill: impl FnOnce(&mut ElementWriter<'_>),
    ) -> Result<Gc<JsArray>, AllocError> {
        let kind = writable_dense_kind(&shape);
        if room > Storage::MAX_VECTOR_LENGTH as usize {
            return Err(AllocError);
        }
        let (room, capacity) = (room as u32, shape.out_of_line_capacity());
        let vector_length = Storage::optimal_vector_length(capacity, room);
        let storage = Storage::try_allocate(vm, 0, capacity, Some(vector_length))?;
        // SAFETY: a collection finds an empty array or none, and nothing else finds it before it is complete.
        unsafe {
            if capacity != 0 {
                storage.clear_slots(0, capacity);
                initialize_slots(storage, &shape);
            }
            storage.set_lengths(0, vector_length);
            let array = Self::create_with_storage(vm, shape, storage);
            let mut writer = ElementWriter::new(storage.uninitialized_elements(0, room), kind);
            fill(&mut writer);
            let length = room - writer.remaining() as u32;
            storage.clear_elements(kind, length, vector_length);
            storage.set_public_length(length);
            Ok(array)
        }
    }

    /// An array of the current realm with what `fill` writes, which is at most `room` elements.
    #[inline(always)]
    pub fn build(
        cx: &mut Cx,
        kind: ElementKind,
        room: usize,
        fill: impl FnOnce(&mut ElementWriter<'_>, &NoGc<'_>),
    ) -> JsResult<Gc<JsArray>> {
        let (global, shape) = (cx.global(), cx.global().array_shape(kind));
        let no_gc = cx.no_gc();
        // SAFETY: outlives its cells. `fill` cannot get at it, and uses the token only after the allocations.
        let array = unsafe {
            let vm = &*global.vm_ptr();
            Self::try_build_with_shape(vm, shape, room, |writer| fill(writer, &no_gc))
        };
        cx.or_out_of_memory(array)
    }

    /// The kind of the elements becomes as general as the values need.
    #[inline]
    pub fn try_from_values_with_shape(
        vm: &Vm,
        shape: Gc<Shape>,
        values: &[Value],
    ) -> Result<Gc<JsArray>, AllocError> {
        let kind = shape.instance_indexing_mode().kind();
        if kind.generalized_for(values.iter().copied()) == kind {
            let fill = |writer: &mut ElementWriter<'_>| writer.extend_from_slice(values);
            // SAFETY: only copies.
            return unsafe { Self::try_build_with_shape(vm, shape, values.len(), fill) };
        }
        let length = u32::try_from(values.len()).map_err(|_| AllocError)?;
        let array = Self::try_create(vm, shape, 0, length)?;
        array.append_values(vm, values)?;
        Ok(array)
    }

    #[inline]
    pub fn from_immutable(cx: &Cx, elements: Gc<ImmutableArray>) -> JsResult<Gc<JsArray>> {
        let global = cx.global();
        if global.is_having_a_bad_time() {
            let (shape, length) = (global.array_shape(elements.kind()), elements.len() as usize);
            let fill =
                |writer: &mut ElementWriter<'_>| writer.extend(elements.elements(), HoleMode::Keep);
            // SAFETY: only copies.
            let array = unsafe { Self::try_build_with_shape(cx, shape, length, fill) };
            return cx.or_out_of_memory(array);
        }
        let mode = IndexingMode::array(elements.kind()).copy_on_write();
        let shape = global.original_array_shape(mode);
        // SAFETY: what such a shape describes. `elements` is alive until the array has it.
        let array = unsafe { Self::create_with_storage(cx, shape, elements.storage()) };
        elements.keep_alive();
        Ok(array)
    }

    /// An array of the current realm with these elements of `source`, unless getting them takes more than a look at
    /// its storage.
    #[inline]
    pub fn try_copy(
        cx: &mut Cx,
        source: &JsObject,
        range: Range<u64>,
        holes: HoleMode,
    ) -> JsResult<Option<Gc<JsArray>>> {
        if let Some(shared) = source.shared_elements()
            && range == (0..u64::from(shared.len()))
        {
            return Self::from_immutable(cx, shared).map(Some);
        }
        let no_gc = cx.no_gc();
        let Some(elements) = source.plain_elements(&no_gc, range.clone()) else {
            return Ok(None);
        };
        let (kind, length) = (elements.kind_with(holes), elements.len());
        let range = range.start as usize..range.end as usize;
        Self::build(cx, kind, length, |writer, no_gc| {
            writer.extend(source.dense_elements(no_gc).slice(range), holes);
        })
        .map(Some)
    }
}
