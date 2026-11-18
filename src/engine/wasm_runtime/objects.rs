//! Structs and arrays.

use core::ptr;

use bun_engine_wasm_compiler::abi::{HostLayout, Trap};
use bun_engine_wasm_format::{AbstractHeapType, CanonicalValType, Rtt, StorageType, TypeIndex};

use crate::engine::Engine;
use crate::global::default_value;
use crate::instance::Instance;
use crate::store::Store;
use crate::value::{Bits, HostCell, Reference};
use crate::words;

const MAX_ARRAY_BYTES: u64 = 1 << 30;

/// # Safety
/// `place` is valid for a write of that size.
#[inline]
pub(crate) unsafe fn write(place: *mut u8, log2_size: u8, value: Bits) {
    // SAFETY: the caller's.
    unsafe { ptr::copy_nonoverlapping(value.to_le_bytes().as_ptr(), place, 1 << log2_size) };
}

#[inline]
fn new_object(instance: &Instance, ty: TypeIndex, rtt: &Rtt, size: usize) -> Option<HostCell> {
    let (host, vm) = instance.store().host();
    let object = host.new_object(vm, instance.type_data(ty), size)?;
    let place = object.field::<usize>(instance.engine().layout().object_rtt);
    // SAFETY: every object has the word.
    unsafe { place.write(rtt.address()) };
    Some(object)
}

/// The fields are zero or null.
#[inline]
pub(crate) fn new_struct(instance: &Instance, ty: TypeIndex) -> Result<HostCell, Trap> {
    let rtt = instance.module().types().rtt(ty);
    let payload = instance.engine().layout().struct_fields;
    let size = payload as usize + rtt.struct_size() as usize;
    let object = new_object(instance, ty, rtt, size).ok_or(Trap::StructNewFailure)?;
    for &offset in rtt.reference_offsets() {
        let place = object.field::<Reference>(payload + offset as i32);
        // SAFETY: within the size, and aligned.
        unsafe { place.write(Reference::NULL) };
    }
    instance.engine().publish_fence();
    Ok(object)
}

/// # Safety
/// `object` is a struct or an array, and lives for `'a`.
#[inline]
unsafe fn rtt_of<'a>(layout: &HostLayout, object: HostCell) -> &'a Rtt {
    // SAFETY: the object keeps its type alive.
    unsafe { &*ptr::with_exposed_provenance(object.field::<usize>(layout.object_rtt).read()) }
}

#[derive(Clone, Copy)]
pub(crate) struct Array<'a> {
    store: &'a Store,
    cell: HostCell,
    elements: *mut u8,
    length: u32,
    element: StorageType,
}

impl<'a> Array<'a> {
    /// # Safety
    /// `value` has the type of the elements, unless these are all written before the array is used.
    #[inline]
    pub(crate) unsafe fn new_filled(
        instance: &'a Instance,
        ty: TypeIndex,
        length: u32,
        value: Bits,
    ) -> Option<Self> {
        let rtt = instance.module().types().rtt(ty);
        let element = rtt.element().expect("the type of an array").storage;
        let bytes = u64::from(length) << element.log2_size();
        if bytes > MAX_ARRAY_BYTES {
            return None;
        }
        let layout = instance.engine().layout();
        let size = layout.array_elements as usize + bytes as usize;
        let cell = new_object(instance, ty, rtt, size)?;
        let array = Self {
            store: instance.store(),
            cell,
            elements: cell.field(layout.array_elements),
            length,
            element,
        };
        // SAFETY: within the size.
        unsafe {
            cell.field::<u32>(layout.array_length).write(length);
            if value != 0 {
                array.fill_without_barrier(0, value, length);
            }
        }
        instance.engine().publish_fence();
        Some(array)
    }

    /// With zeros or nulls.
    #[inline]
    pub(crate) fn new_default(instance: &'a Instance, ty: TypeIndex, length: u32) -> Option<Self> {
        let element = instance.module().types().array_type(ty).element.storage;
        // SAFETY: validation allows it where null is a value, and others overwrite it.
        unsafe { Self::new_filled(instance, ty, length, default_value(element.unpacked())) }
    }

    /// `None` for null.
    ///
    /// # Safety
    /// `reference` is null or an array, which lives for `'a`.
    #[inline]
    pub(crate) unsafe fn of(store: &'a Store, reference: Reference) -> Option<Self> {
        let (cell, layout) = (reference.cell()?, store.engine().layout());
        // SAFETY: the caller's.
        let (rtt, length) = unsafe {
            let length = cell.field::<u32>(layout.array_length).read();
            (rtt_of(layout, cell), length)
        };
        Some(Self {
            store,
            cell,
            elements: cell.field(layout.array_elements),
            length,
            element: rtt.element().expect("the type of an array").storage,
        })
    }

    #[inline]
    pub(crate) fn cell(&self) -> HostCell {
        self.cell
    }

    #[inline]
    pub(crate) fn has_range(&self, start: u32, count: u32) -> bool {
        u64::from(start) + u64::from(count) <= self.length.into()
    }

    /// In bytes, of so many elements.
    #[inline]
    pub(crate) fn size_of(&self, count: u32) -> u64 {
        u64::from(count) << self.element.log2_size()
    }

    #[inline]
    fn address_of(&self, index: u32) -> *mut u8 {
        self.elements.wrapping_add(self.size_of(index) as usize)
    }

    /// # Safety
    /// As for `fill`.
    #[inline]
    unsafe fn fill_without_barrier(&self, start: u32, value: Bits, count: u32) {
        /// # Safety
        /// `first` is valid for the writes, and nothing else accesses the memory meanwhile.
        unsafe fn repeat<const N: usize>(first: *mut u8, value: Bits, count: usize) {
            let bytes = *value.to_le_bytes().first_chunk::<N>().unwrap();
            // SAFETY: the caller's.
            unsafe { core::slice::from_raw_parts_mut(first.cast(), count) }.fill(bytes);
        }
        let (first, count) = (self.address_of(start), count as usize);
        // SAFETY: the caller's. The collector reads references only.
        unsafe {
            match self.element.log2_size() {
                0 => repeat::<1>(first, value, count),
                1 => repeat::<2>(first, value, count),
                2 => repeat::<4>(first, value, count),
                3 => self
                    .store
                    .engine()
                    .fill_words(first.cast(), [value as u64], count),
                _ => repeat::<16>(first, value, count),
            }
        }
    }

    /// # Safety
    /// The array has the range, and `value` has the type of the elements.
    #[inline]
    pub(crate) unsafe fn fill(&self, start: u32, value: Bits, count: u32) {
        // SAFETY: the caller's.
        unsafe { self.fill_without_barrier(start, value, count) };
        if self.element.is_ref() && Reference(value as u64).cell().is_some() {
            self.store.write_barrier(self.cell);
        }
    }

    /// # Safety
    /// Both have the range, and the type of the elements of `from` is a subtype of that of the elements of `self`.
    #[inline]
    pub(crate) unsafe fn copy_from(&self, from: Self, dst: u32, src: u32, count: u32) {
        let (dst, src) = (self.address_of(dst), from.address_of(src));
        if !self.element.is_ref() {
            // SAFETY: the caller's.
            return unsafe { ptr::copy(src, dst, self.size_of(count) as usize) };
        }
        let engine = self.store.engine();
        // SAFETY: the caller's.
        unsafe { engine.move_words(dst.cast(), src.cast(), count as usize) };
        self.store.write_barrier(self.cell);
    }

    /// # Safety
    /// The elements are numbers, and there are enough from `start` on.
    #[inline]
    pub(crate) unsafe fn write_bytes(&self, start: u32, bytes: &[u8]) {
        // SAFETY: the caller's.
        unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), self.address_of(start), bytes.len()) };
    }
}

impl Engine {
    /// On any thread.
    ///
    /// # Safety
    /// `object` is a struct or an array.
    pub unsafe fn trace_object(&self, object: HostCell, mut visit: impl FnMut(HostCell)) {
        let layout = self.layout();
        let mut visit = |place: *mut u64| {
            // SAFETY: a field or an element of the object.
            let value = Reference(unsafe { words::load(place) });
            value.cell().into_iter().for_each(&mut visit);
        };
        // SAFETY: the caller's.
        let rtt = unsafe { rtt_of(layout, object) };
        match rtt.element() {
            None => {
                for &offset in rtt.reference_offsets() {
                    visit(object.field(layout.struct_fields + offset as i32));
                }
            }
            Some(element) if element.storage.is_ref() => {
                // SAFETY: every array has the word.
                let length = unsafe { object.field::<u32>(layout.array_length).read() };
                let first = object.field::<u64>(layout.array_elements);
                (0..length as usize).for_each(|index| visit(first.wrapping_add(index)));
            }
            Some(_) => {}
        }
    }

    /// For a type below `any`. `value` is internalized.
    ///
    /// # Safety
    /// `value` is a value of the host.
    pub unsafe fn has_type(&self, value: Reference, ty: &CanonicalValType) -> bool {
        let expected = ty.erased().as_ref().expect("a reference type");
        if value.is_null() {
            return expected.is_nullable();
        }
        let layout = self.layout();
        let object = value.cell().and_then(|cell| {
            // SAFETY: every cell has the byte.
            let kind = unsafe { cell.field::<u8>(layout.cell_kind).read() };
            let is_object = kind == layout.struct_kind || kind == layout.array_kind;
            // SAFETY: the kind tells.
            is_object.then(|| unsafe { rtt_of(layout, cell) })
        });
        if let Some(defined) = ty.defined() {
            return object.is_some_and(|rtt| rtt.is_subtype_of(defined.rtt()));
        }
        let actual = match object {
            Some(rtt) => rtt.kind().abstract_type(),
            None if value.is_i31() => AbstractHeapType::I31,
            None => AbstractHeapType::Any,
        };
        actual.is_subtype_of(expected.heap_type().abstract_type())
    }
}
