//! Access to memory that other views, other threads and the embedder may have too, and that need not be aligned.

use core::intrinsics::{
    unaligned_volatile_load, unaligned_volatile_store, volatile_copy_memory, volatile_set_memory,
};
use core::marker::PhantomData;
use core::ops::Range;
use core::ptr::NonNull;

use bun_alloc::AllocError;

use super::element::{Element, NumberElement};

/// Elements that stay accessible for `'a` but can change at any time. Nothing ever refers to one: each is copied in or
/// out, and no address depends on a value that is read twice.
pub struct BufferSlice<'a, T> {
    start: *mut T,
    length: usize,
    memory: PhantomData<&'a [core::cell::Cell<T>]>,
}

impl<T> Clone for BufferSlice<'_, T> {
    #[inline(always)]
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for BufferSlice<'_, T> {}

impl<'a, T: Element> From<&'a mut [T]> for BufferSlice<'a, T> {
    #[inline(always)]
    fn from(elements: &'a mut [T]) -> BufferSlice<'a, T> {
        // SAFETY: borrowed exclusively.
        unsafe { BufferSlice::new(elements.as_mut_ptr().cast(), elements.len()) }
    }
}

impl<'a, T: Element> BufferSlice<'a, T> {
    /// # Safety
    /// `length` elements can be read and written for `'a`, and no reference to them exists meanwhile.
    #[inline(always)]
    pub unsafe fn new(start: *mut u8, length: usize) -> BufferSlice<'a, T> {
        BufferSlice {
            start: start.cast(),
            length,
            memory: PhantomData,
        }
    }

    #[inline(always)]
    pub fn empty() -> BufferSlice<'a, T> {
        // SAFETY: there is nothing to access.
        unsafe { BufferSlice::new(NonNull::dangling().as_ptr(), 0) }
    }

    #[inline(always)]
    pub fn len(self) -> usize {
        self.length
    }

    #[inline(always)]
    pub fn is_empty(self) -> bool {
        self.length == 0
    }

    #[inline(always)]
    pub fn byte_len(self) -> usize {
        self.length * size_of::<T>()
    }

    #[inline(always)]
    pub fn as_ptr(self) -> *mut T {
        self.start
    }

    #[inline(always)]
    pub fn is_aligned(self) -> bool {
        self.start.is_aligned()
    }

    #[inline(always)]
    pub fn get(self, index: usize) -> Option<T> {
        // SAFETY: inside.
        (index < self.length).then(|| unsafe { unaligned_volatile_load(self.start.add(index)) })
    }

    /// Panics past the end.
    #[inline(always)]
    pub fn at(self, index: usize) -> T {
        self.get(index).expect("inside the slice")
    }

    /// False, having done nothing, past the end.
    #[inline(always)]
    pub fn try_set(self, index: usize, value: T) -> bool {
        if index >= self.length {
            return false;
        }
        // SAFETY: inside.
        unsafe { unaligned_volatile_store(self.start.add(index), value) };
        true
    }

    /// Panics past the end.
    #[inline(always)]
    pub fn set(self, index: usize, value: T) {
        assert!(self.try_set(index, value));
    }

    /// `None` unless the range is inside.
    #[inline(always)]
    pub fn try_slice(self, start: usize, count: usize) -> Option<BufferSlice<'a, T>> {
        let rest = self.length.checked_sub(start)?;
        // SAFETY: inside.
        (count <= rest).then(|| unsafe { BufferSlice::new(self.start.add(start).cast(), count) })
    }

    /// Panics unless the range is inside.
    #[inline(always)]
    pub fn slice(self, range: Range<usize>) -> BufferSlice<'a, T> {
        let count = range.end.wrapping_sub(range.start);
        self.try_slice(range.start, count)
            .expect("inside the slice")
    }

    /// As much of the range as is inside.
    #[inline(always)]
    pub fn clamped(self, range: Range<usize>) -> BufferSlice<'a, T> {
        let end = range.end.min(self.length);
        self.slice(range.start.min(end)..end)
    }

    /// The same bytes as elements of another type, but for a remainder that does not make one.
    #[inline(always)]
    pub fn cast<U: Element>(self) -> BufferSlice<'a, U> {
        // SAFETY: no more bytes, any of which are a value.
        unsafe { BufferSlice::new(self.start.cast(), self.byte_len() / size_of::<U>()) }
    }

    #[inline(always)]
    pub fn bits(self) -> BufferSlice<'a, T::Bits> {
        self.cast()
    }

    #[inline(always)]
    pub fn bytes(self) -> BufferSlice<'a, u8> {
        self.cast()
    }

    #[inline(always)]
    pub fn iter(self) -> impl DoubleEndedIterator<Item = T> + ExactSizeIterator + Clone + 'a {
        // SAFETY: inside.
        (0..self.length).map(move |index| unsafe { unaligned_volatile_load(self.start.add(index)) })
    }

    #[inline(always)]
    fn addresses(self) -> Range<usize> {
        self.start.addr()..self.start.addr() + self.byte_len()
    }

    /// Whether a byte is in both.
    #[inline(always)]
    pub fn overlaps<U: Element>(self, other: BufferSlice<'_, U>) -> bool {
        let (this, other) = (self.addresses(), other.addresses());
        this.start.max(other.start) < this.end.min(other.end)
    }

    /// As `memmove`. Panics if the lengths differ.
    #[inline(always)]
    pub fn copy_from(self, source: BufferSlice<'_, T>) {
        assert!(self.length == source.length);
        // SAFETY: both have that many bytes.
        unsafe {
            volatile_copy_memory::<u8>(self.start.cast(), source.start.cast(), self.byte_len())
        }
    }

    /// Byte by byte from the first on, so that it may come to read what it has written. Panics if the lengths differ.
    #[inline]
    pub fn copy_ascending_from(self, source: BufferSlice<'_, T>) {
        assert!(self.length == source.length);
        let distance = self.start.addr().wrapping_sub(source.start.addr());
        if distance == 0 || distance >= self.byte_len() {
            return self.copy_from(source);
        }
        // What is in front of the target repeats.
        let (target, mut done) = (self.bytes(), distance);
        target
            .slice(0..done)
            .copy_from(source.bytes().slice(0..done));
        while done < target.len() {
            let next = target.clamped(done..done * 2);
            next.copy_from(target.slice(0..next.len()));
            done *= 2;
        }
    }

    /// Panics if the lengths differ.
    #[inline(always)]
    pub fn copy_from_slice(self, source: &[T]) {
        assert!(self.length == source.len());
        // SAFETY: both have that many bytes.
        unsafe {
            volatile_copy_memory::<u8>(self.start.cast(), source.as_ptr().cast(), self.byte_len());
        }
    }

    /// Panics if the lengths differ.
    #[inline(always)]
    pub fn copy_to_slice(self, target: &mut [T]) {
        BufferSlice::from(target).copy_from(self);
    }

    pub fn try_to_vec(self) -> Result<Vec<T>, AllocError> {
        let mut copy = Vec::<T>::new();
        copy.try_reserve_exact(self.length)
            .map_err(|_| AllocError)?;
        // SAFETY: there is room for them, and they are all written.
        unsafe {
            BufferSlice::new(copy.as_mut_ptr().cast(), self.length).copy_from(self);
            copy.set_len(self.length);
        }
        Ok(copy)
    }

    #[inline]
    pub fn fill(self, value: T) {
        if size_of::<T>() == 1 {
            // SAFETY: a value of one byte, and inside.
            return unsafe {
                volatile_set_memory(self.start, core::mem::transmute_copy(&value), self.length);
            };
        }
        for index in 0..self.length {
            // SAFETY: inside.
            unsafe { self.start.add(index).write_unaligned(value) };
        }
    }

    #[inline]
    pub fn fill_with_zeros(self) {
        self.bytes().fill(0);
    }

    #[inline]
    pub fn reverse(self) {
        let half = self.length / 2;
        // SAFETY: the two halves, which are inside and apart.
        unsafe { swap_mirrored(self.start, self.start.add(self.length - half), half) }
    }

    /// Stops at the end of the shorter of the two. Returns how many it has stored.
    #[inline(always)]
    pub fn fill_from(self, values: impl IntoIterator<Item = T>) -> usize {
        let mut count = 0;
        for (index, value) in (0..self.length).zip(values) {
            // SAFETY: inside.
            unsafe { self.start.add(index).write_unaligned(value) };
            count += 1;
        }
        count
    }

    /// From the first element on. `map` does nothing but compute. Panics if the lengths differ.
    #[inline(always)]
    pub(super) fn map_ascending_from<S: Element>(
        self,
        source: BufferSlice<'_, S>,
        map: impl Fn(S) -> T,
    ) {
        assert!(self.length == source.length);
        for index in 0..self.length {
            // SAFETY: inside both.
            unsafe {
                let value = map(source.start.add(index).read_unaligned());
                self.start.add(index).write_unaligned(value);
            }
        }
    }

    /// The last element of `source` comes first. They do not overlap. Panics if the lengths differ.
    #[inline]
    pub(super) fn copy_reversed_from(self, source: BufferSlice<'_, T>) {
        assert!(self.length == source.length);
        for index in 0..self.length {
            // SAFETY: inside both.
            unsafe {
                let value = source.start.add(self.length - 1 - index).read_unaligned();
                self.start.add(index).write_unaligned(value);
            }
        }
    }
}

impl<T: NumberElement> BufferSlice<'_, T> {
    /// From the first element on, so that it may come to read what it has written. Panics if the lengths differ.
    #[inline(never)]
    pub fn convert_ascending_from<S: NumberElement>(self, source: BufferSlice<'_, S>) {
        self.map_ascending_from(source, S::convert);
    }
}

/// # Safety
/// Each has `count` elements, and they do not overlap.
#[inline(always)]
unsafe fn swap_mirrored<T: Element>(front: *mut T, back: *mut T, count: usize) {
    for index in 0..count {
        // SAFETY: the caller's.
        unsafe {
            let (a, b) = (front.add(index), back.add(count - 1 - index));
            let value = a.read_unaligned();
            a.write_unaligned(b.read_unaligned());
            b.write_unaligned(value);
        }
    }
}
