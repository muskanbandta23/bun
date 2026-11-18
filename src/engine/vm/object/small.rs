//! Objects and arrays with few members, made without a call. Whoever makes them runs `publish_fence` before storing a
//! reference to them in an older cell.

use crate::heap::{ATOM_SIZE, EXACT_CUTOFF};
use crate::{
    ElementKind, Gc, GlobalObject, JsArray, JsCell, JsFinalObject, JsObject, Shape, ShapeFlags,
    ShapeId, ShapeSpec, Storage, Value, Vm,
};

const WORDS_PER_ATOM: usize = ATOM_SIZE / size_of::<u64>();

/// # Safety
/// There is room for them, and they are not empty.
#[inline(always)]
unsafe fn copy_words(values: &[Value], target: *mut u64, word: impl Fn(Value) -> u64) {
    let mut index = values.len();
    while {
        index -= 1;
        // SAFETY: the caller's. There is no instruction, but the compiler can neither unroll this nor make it a call.
        unsafe {
            target.add(index).write(word(*values.get_unchecked(index)));
            core::arch::asm!("/* {0} */", inout(reg) index, options(pure, nomem, nostack, preserves_flags));
        }
        index != 0
    } {}
}

/// A shape of ordinary objects whose properties fill a small cell, up to one slot. It is not kept alive.
#[derive(Clone, Copy)]
pub(crate) struct SnugShape(ShapeId);

impl SnugShape {
    const HEADER: u32 = ShapeSpec::of::<JsFinalObject>().instance_header();

    pub(crate) fn fits(shape: &Shape) -> bool {
        let unfit = ShapeFlags::DICTIONARY | ShapeFlags::IS_CLAIMED | ShapeFlags::HAS_ANY_ACCESSOR;
        let (count, capacity) = (shape.slot_count(), shape.inline_capacity());
        shape.instance_header() == Self::HEADER
            && !shape.has(unfit)
            && count != 0
            && count.next_multiple_of(WORDS_PER_ATOM) == capacity
            && JsFinalObject::allocation_size(capacity) <= EXACT_CUTOFF
    }

    /// # Safety
    /// It fits.
    #[inline(always)]
    pub(crate) unsafe fn new_unchecked(shape: ShapeId) -> SnugShape {
        SnugShape(shape)
    }

    /// # Safety
    /// The shape is alive, and has as many slots as there are values.
    #[inline(always)]
    pub(crate) unsafe fn try_create(self, vm: &Vm, values: &[Value]) -> Option<Gc<JsObject>> {
        let atoms = 1 + values.len().div_ceil(WORDS_PER_ATOM);
        // SAFETY: it fits.
        unsafe { core::hint::assert_unchecked(atoms <= EXACT_CUTOFF / ATOM_SIZE) };
        let address = vm.heap.cells.try_bump_atoms(atoms)?;
        let words = core::ptr::with_exposed_provenance_mut::<u64>(address);
        // SAFETY: a new cell of that size, each word of which is written.
        unsafe {
            words.write(u64::from(self.0.bits()) | u64::from(Self::HEADER) << 32);
            words.add(1).write(0);
            words.add(atoms * WORDS_PER_ATOM - 1).write(0);
            copy_words(values, words.add(2), Value::to_bits);
            Some(Gc::from_address(address))
        }
    }
}

impl JsArray {
    const MAX_SMALL_LENGTH: usize = EXACT_CUTOFF / size_of::<u64>() - 1;

    /// # Safety
    /// `kind` is dense and can represent the values. No collection runs.
    #[inline(always)]
    pub(crate) unsafe fn try_create_small(
        vm: &Vm,
        global: &GlobalObject,
        kind: ElementKind,
        values: &[Value],
    ) -> Option<Gc<JsArray>> {
        const { assert!(Storage::MIN_VECTOR_LENGTH == 3 && size_of::<JsArray>() == ATOM_SIZE) };
        if values.len().wrapping_sub(1) >= Self::MAX_SMALL_LENGTH {
            return None;
        }
        let atoms = (values.len() / WORDS_PER_ATOM + 1).max(2);
        let storage = Storage::from_base(vm.heap.auxiliary.try_bump_atoms(atoms)?, 0);
        let address = vm.heap.cells.try_bump_atoms(1)?;
        let (elements, room) = (storage.elements_ptr(), atoms * WORDS_PER_ATOM - 1);
        let words = core::ptr::with_exposed_provenance_mut::<u64>(address);
        // SAFETY: new and of these sizes. At most two elements are left for holes.
        unsafe {
            storage.set_lengths(values.len() as u32, room as u32);
            elements
                .add(room - 2)
                .cast::<[u64; 2]>()
                .write([kind.hole(); 2]);
            if kind == ElementKind::Double {
                copy_words(values, elements, |it| {
                    it.as_number().unwrap_unchecked().to_bits()
                });
            } else {
                copy_words(values, elements, Value::to_bits);
            }
            words.write(JsCell::header_for(&global.array_shape(kind)));
            words.add(1).write(storage.address() as u64);
            Some(Gc::from_address(address))
        }
    }
}
