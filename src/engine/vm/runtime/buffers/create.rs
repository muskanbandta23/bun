//! Typed arrays that have no buffer yet.

use bun_alloc::AllocError;

use super::contents::{MAX_ARRAY_BUFFER_SIZE, try_allocate};
use super::element::{Element, ElementType};
use super::slice::BufferSlice;
use super::view::ViewMode;
use crate::{
    CellKind, Cx, Gc, JsArrayBufferView, JsResult, JsTypedArray, NoGc, Shape, TypedArray, Vm,
    with_element_type,
};

/// Writes the elements of a new array from the first on.
pub struct Filler<'a, T> {
    rest: BufferSlice<'a, T>,
}

impl<'a, T: Element> Filler<'a, T> {
    /// The next `count` elements, all of which the caller writes without reading any. Panics if fewer are left.
    #[inline(always)]
    pub(super) fn reserve(&mut self, count: usize) -> BufferSlice<'a, T> {
        let next = self.rest.slice(0..count);
        self.rest = self.rest.slice(count..self.rest.len());
        next
    }

    /// `write` gets all that are left, reads none, and returns how many it has written.
    #[inline(always)]
    pub(super) fn write_some(&mut self, write: impl FnOnce(BufferSlice<'a, T>) -> usize) -> usize {
        let count = write(self.rest);
        self.reserve(count);
        count
    }

    /// Panics if they do not fit.
    #[inline]
    pub fn copy(&mut self, source: BufferSlice<'_, T>) {
        self.reserve(source.len()).copy_from(source);
    }

    /// Panics if they do not fit.
    #[inline]
    pub fn copy_reversed(&mut self, source: BufferSlice<'_, T>) {
        self.reserve(source.len()).copy_reversed_from(source);
    }

    /// Panics if they do not fit.
    #[inline]
    pub fn copy_slice(&mut self, source: &[T]) {
        self.reserve(source.len()).copy_from_slice(source);
    }

    #[inline]
    pub fn fill_rest(&mut self, value: T) {
        self.reserve(self.rest.len()).fill(value);
    }
}

impl<T: Element> TypedArray<T> {
    #[inline(always)]
    fn try_allocate(
        vm: &Vm,
        shape: Gc<Shape>,
        length: usize,
        is_zeroed: bool,
    ) -> Result<Gc<TypedArray<T>>, AllocError> {
        if length > JsArrayBufferView::MAX_SMALL_LENGTH {
            if length > MAX_ARRAY_BUFFER_SIZE / size_of::<T>() {
                return Err(AllocError);
            }
            let vector = try_allocate(length * size_of::<T>(), is_zeroed)?;
            // SAFETY: new, and of that length.
            return Ok(unsafe { Self::adopt(vm, shape, vector, length) });
        }
        JsArrayBufferView::assert_shape(&shape, Self::CLASS);
        let size = (length * size_of::<T>()).next_multiple_of(size_of::<u64>());
        let vector = JsArrayBufferView::allocate_small_vector(vm, size)?;
        // SAFETY: new and of that size, the class is checked, and it is that of the type.
        unsafe {
            if is_zeroed {
                vector.write_bytes(0, size);
            }
            let mode = ViewMode::empty();
            Ok(JsArrayBufferView::allocate(vm, shape, vector, length, 0, mode, 0).cast_unchecked())
        }
    }

    /// Panics if there are too many elements.
    ///
    /// # Safety
    /// `vector` is an allocation of the default allocator that has so many elements and that the caller gives up.
    #[inline(never)]
    pub unsafe fn adopt(vm: &Vm, shape: Gc<Shape>, vector: *mut u8, length: usize) -> Gc<Self> {
        assert!(length <= MAX_ARRAY_BUFFER_SIZE / size_of::<T>() && !vector.is_null());
        JsArrayBufferView::assert_shape(&shape, Self::CLASS);
        // SAFETY: the caller's, the class is checked, and it is that of the type.
        unsafe {
            let view =
                JsArrayBufferView::allocate(vm, shape, vector, length, 0, ViewMode::OVERSIZE, 0);
            vm.services.buffers.add_oversize_view(&view);
            vm.heap
                .report_extra_memory_allocated(&view, length * size_of::<T>());
            view.cast_unchecked()
        }
    }

    /// Zeros. Panics unless the shape is for arrays of the type whose length is fixed, as all that follow do.
    #[inline]
    pub fn create(cx: &Cx, shape: Gc<Shape>, length: usize) -> JsResult<Gc<TypedArray<T>>> {
        cx.or_out_of_memory(Self::try_allocate(cx, shape, length, true))
    }

    /// # Safety
    /// Each element is written before it is read.
    #[inline]
    pub unsafe fn create_uninitialized(
        cx: &Cx,
        shape: Gc<Shape>,
        length: usize,
    ) -> JsResult<Gc<TypedArray<T>>> {
        cx.or_out_of_memory(Self::try_allocate(cx, shape, length, false))
    }

    /// The elements that `fill` leaves are zero.
    #[inline]
    pub fn create_filled(
        cx: &mut Cx,
        shape: Gc<Shape>,
        length: usize,
        fill: impl FnOnce(&NoGc<'_>, &mut Filler<'_, T>),
    ) -> JsResult<Gc<TypedArray<T>>> {
        // SAFETY: the filler only writes, and has written all of them in the end.
        let array = unsafe { Self::create_uninitialized(cx, shape, length) }?;
        let no_gc = cx.no_gc();
        let mut filler = Filler {
            rest: array.elements(&no_gc),
        };
        fill(&no_gc, &mut filler);
        if !filler.rest.is_empty() {
            filler.rest.fill_with_zeros();
        }
        Ok(array)
    }

    pub fn from_slice(cx: &Cx, shape: Gc<Shape>, elements: &[T]) -> JsResult<Gc<TypedArray<T>>> {
        // SAFETY: all are written, and nothing else has the array yet.
        unsafe {
            let array = Self::create_uninitialized(cx, shape, elements.len())?;
            array.elements_unchecked().copy_from_slice(elements);
            Ok(array)
        }
    }

    pub fn from_vec(cx: &Cx, shape: Gc<Shape>, elements: Vec<T>) -> JsResult<Gc<TypedArray<T>>> {
        let length = elements.len();
        if length <= JsArrayBufferView::MAX_SMALL_LENGTH
            || length > MAX_ARRAY_BUFFER_SIZE / size_of::<T>()
        {
            return Self::from_slice(cx, shape, &elements);
        }
        // SAFETY: the global allocator is the default one.
        Ok(unsafe { Self::adopt(cx, shape, elements.leak().as_mut_ptr().cast(), length) })
    }
}

impl JsTypedArray {
    fn element_type_of(shape: &Shape) -> ElementType {
        ElementType::of_cell_type(shape.instance_type()).expect("the shape of a typed array")
    }

    /// Zeros, of the type that the shape says.
    pub fn create(cx: &Cx, shape: Gc<Shape>, length: usize) -> JsResult<Gc<JsTypedArray>> {
        with_element_type!(Self::element_type_of(&shape), T => {
            TypedArray::<T>::create(cx, shape, length).map(Gc::upcast)
        })
    }

    /// # Safety
    /// Each element is written before it is read.
    pub unsafe fn create_uninitialized(
        cx: &Cx,
        shape: Gc<Shape>,
        length: usize,
    ) -> JsResult<Gc<JsTypedArray>> {
        with_element_type!(Self::element_type_of(&shape), T => {
            // SAFETY: the caller's.
            unsafe { TypedArray::<T>::create_uninitialized(cx, shape, length) }.map(Gc::upcast)
        })
    }
}
