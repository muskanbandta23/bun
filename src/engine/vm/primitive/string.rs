use core::cell::UnsafeCell;
use core::cmp::Ordering as Comparison;
use core::mem::offset_of;
use core::ops::Deref;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicUsize, Ordering};

use bun_alloc::AllocError;
use bun_engine_text::{Atom, Identifier, PropertyName, StringImpl, StringRef, Text, names};

use crate::heap::{keep_alive, visit_children_of};
use crate::{
    Attributes, CellKind, CellType, CellTypeRange, Class, ClassOps, Cx, Extends, Gc, GcFree,
    JsCell, JsResult, JsRopeString, PropertyKey, PropertySlot, Shape, TypeFlags, Visitor, Vm,
    ascii_literal,
};

/// The per-cell bit says that the characters are an atom.
#[repr(C)]
pub struct JsString {
    base: JsCell,
    /// A counted reference to a `StringImpl` or, with `IS_ROPE`, the first fiber of a `JsRopeString`.
    fiber: UnsafeCell<usize>,
}

#[unsafe(export_name = "engine_class_String")]
static STRING_CLASS: Class = Class::new::<JsString>(
    ascii_literal!("string"),
    None,
    CellTypeRange::new(CellType::String, CellType::String),
);

impl !GcFree for JsString {}

impl Deref for JsString {
    type Target = JsCell;

    #[inline(always)]
    fn deref(&self) -> &JsCell {
        &self.base
    }
}

// SAFETY: starts with a `JsCell`.
unsafe impl Extends<JsCell> for JsString {}

unsafe extern "C" fn estimated_size(cell: &JsCell, _: &Vm) -> usize {
    let string = Gc::from(cell).cast::<JsString>();
    cell.cell_size() + string.try_impl().map_or(0, StringImpl::shared_cost)
}

// SAFETY: visits the fibers of a rope.
unsafe impl CellKind for JsString {
    type Base = JsCell;
    type Init = (Gc<Shape>, StringRef);

    const CLASS: &'static Class = &STRING_CLASS;
    const CELL_TYPE: CellType = CellType::String;
    const FLAGS: TypeFlags = TypeFlags::SHAPE_IS_IMMORTAL
        .union(TypeFlags::OVERRIDES_GET_OWN_PROPERTY_SLOT)
        .union(TypeFlags::INTERCEPTS_INDEXED_ACCESS);
    const HOLDS_CLASS: bool = false;
    const OPS: ClassOps = {
        let mut ops = JsCell::OPS;
        ops.visit_children = visit_children_of::<JsString>;
        ops.destroy = crate::heap::destroy::<JsString>;
        ops.estimated_size = estimated_size;
        ops
    };

    #[inline(always)]
    fn is(cell: &JsCell) -> bool {
        cell.is_string()
    }

    #[inline(always)]
    fn shape_of(init: &Self::Init) -> Gc<Shape> {
        init.0
    }

    #[inline(always)]
    unsafe fn initialize(this: *mut JsString, (shape, string): Self::Init) {
        let atom_bit = u64::from(string.is_atom()) << (JsCell::OFFSET_OF_FLAGS * 8 + 7);
        let fiber = string.into_raw().as_ptr().expose_provenance();
        // SAFETY: the caller's.
        unsafe {
            this.cast::<u64>()
                .write(JsCell::header_for(&shape) | atom_bit);
            (&raw mut (*this).fiber).write(UnsafeCell::new(fiber));
        }
    }

    fn trace_fields(&self, visitor: &mut Visitor) {
        self.visit_fibers(visitor);
    }
}

impl Drop for JsString {
    #[inline(always)]
    fn drop(&mut self) {
        let fiber = *self.fiber.get_mut();
        if fiber & Self::IS_ROPE == 0 {
            // SAFETY: the reference that the cell holds.
            drop(unsafe { StringRef::from_raw(impl_at(fiber)) });
        }
    }
}

#[inline(always)]
fn impl_at(fiber: usize) -> NonNull<StringImpl> {
    // SAFETY: a string that is no rope has characters.
    unsafe { NonNull::new_unchecked(core::ptr::with_exposed_provenance_mut(fiber)) }
}

/// The characters of a string, which stay where they are while this is alive.
pub struct StringView {
    owner: Gc<JsString>,
    offset: u32,
    text: Text<'static>,
}

impl StringView {
    #[inline(always)]
    pub fn text(&self) -> Text<'_> {
        self.text
    }

    /// The string that has the characters, which is no rope, and where they start in it.
    #[inline(always)]
    pub fn owner(&self) -> (Gc<JsString>, u32) {
        (self.owner, self.offset)
    }

    /// Panics if the range is not inside the view.
    #[inline]
    pub fn substring(&self, vm: &Vm, range: core::ops::Range<usize>) -> Gc<JsString> {
        assert!(range.start <= range.end && range.end <= self.text.len());
        let start = self.offset + range.start as u32;
        JsRopeString::substring_of_resolved(vm, self.owner, start, range.len() as u32)
    }
}

impl Drop for StringView {
    #[inline(always)]
    fn drop(&mut self) {
        keep_alive(self.owner.address());
    }
}

/// The same, of a string that has its own `StringImpl`.
pub struct ResolvedString {
    owner: Gc<JsString>,
    string: NonNull<StringImpl>,
}

impl Deref for ResolvedString {
    type Target = StringImpl;

    #[inline(always)]
    fn deref(&self) -> &StringImpl {
        // SAFETY: the owner holds it or, having swapped it for an atom, the `Vm` does while the owner is on the stack.
        unsafe { self.string.as_ref() }
    }
}

impl Drop for ResolvedString {
    #[inline(always)]
    fn drop(&mut self) {
        keep_alive(self.owner.address());
    }
}

impl JsString {
    pub const OFFSET_OF_FIBER: usize = offset_of!(JsString, fiber);
    pub const IS_ROPE: usize = 1;

    #[inline(always)]
    pub(super) fn allocate(vm: &Vm, string: StringRef) -> Gc<JsString> {
        vm.allocate::<JsString>((vm.shapes.string(), string))
    }

    #[inline]
    pub fn new(vm: &Vm, string: StringRef) -> Gc<JsString> {
        if string.len() <= 1
            && let Some(small) = Self::small(vm, string.text())
        {
            return small;
        }
        let cost = string.take_cost();
        let result = Self::allocate(vm, string);
        vm.heap.report_extra_memory_allocated(&result, cost);
        result
    }

    #[inline(always)]
    pub(super) fn small(vm: &Vm, text: Text<'_>) -> Option<Gc<JsString>> {
        match text.get(0) {
            None => Some(vm.small_strings.empty()),
            Some(unit @ ..=0xff) if text.len() == 1 => {
                Some(vm.small_strings.single_character(vm, unit as u8))
            }
            Some(_) => None,
        }
    }

    #[inline]
    pub fn try_from_text(vm: &Vm, text: Text<'_>) -> Result<Gc<JsString>, AllocError> {
        if text.len() <= 1
            && let Some(small) = Self::small(vm, text)
        {
            return Ok(small);
        }
        Ok(Self::new(vm, StringRef::try_from_text(text)?))
    }

    /// For text whose size no script picks.
    pub fn from_text(vm: &Vm, text: Text<'_>) -> Gc<JsString> {
        Self::try_from_text(vm, text).unwrap_or_else(|_| bun_alloc::out_of_memory())
    }

    pub fn from_literal(vm: &Vm, ascii: &'static str) -> Gc<JsString> {
        Self::new(vm, StringRef::from_literal(ascii))
    }

    #[inline]
    pub fn from_unit(vm: &Vm, unit: u16) -> Gc<JsString> {
        match u8::try_from(unit) {
            Ok(character) => vm.small_strings.single_character(vm, character),
            Err(_) => Self::allocate(vm, StringRef::from_text(Text::Utf16(&[unit]))),
        }
    }

    pub fn from_code_point(vm: &Vm, code_point: u32) -> Gc<JsString> {
        match u16::try_from(code_point) {
            Ok(unit) => Self::from_unit(vm, unit),
            Err(_) => {
                let offset = code_point - 0x10000;
                let units = [
                    0xd800 | (offset >> 10) as u16,
                    0xdc00 | (offset & 0x3ff) as u16,
                ];
                Self::allocate(vm, StringRef::from_text(Text::Utf16(&units)))
            }
        }
    }

    #[inline(always)]
    pub(crate) fn fiber(&self) -> usize {
        // SAFETY: only this thread writes it.
        unsafe { *self.fiber.get() }
    }

    #[inline(always)]
    fn fiber_atomic(&self) -> &AtomicUsize {
        // SAFETY: aligned. The plain loads are on the only thread that stores.
        unsafe { AtomicUsize::from_ptr(self.fiber.get()) }
    }

    pub(super) fn become_resolved(&self, string: StringRef) {
        let is_atom = string.is_atom();
        let fiber = string.into_raw().as_ptr().expose_provenance();
        self.fiber_atomic().store(fiber, Ordering::Release);
        if is_atom {
            self.set_per_cell_bit(true);
        }
    }

    #[inline(always)]
    pub fn is_rope(&self) -> bool {
        self.fiber() & Self::IS_ROPE != 0
    }

    /// A rope that stands for a part of another string.
    pub fn is_unresolved_substring(&self) -> bool {
        self.is_rope() && self.fiber() & JsRopeString::IS_SUBSTRING != 0
    }

    #[inline(always)]
    pub(super) fn as_rope(&self) -> Option<Gc<JsRopeString>> {
        // SAFETY: only a `JsRopeString` has the bit.
        self.is_rope()
            .then(|| unsafe { Gc::from(self).cast_unchecked() })
    }

    /// Not to be held across an allocation.
    #[inline(always)]
    pub(crate) fn try_impl(&self) -> Option<&StringImpl> {
        let fiber = self.fiber();
        // SAFETY: the cell holds a reference.
        (fiber & Self::IS_ROPE == 0).then(|| unsafe { impl_at(fiber).as_ref() })
    }

    #[inline(always)]
    pub fn len(&self) -> u32 {
        match self.as_rope() {
            Some(rope) => rope.rope_length(),
            // SAFETY: as above.
            None => unsafe { impl_at(self.fiber()).as_ref() }.len() as u32,
        }
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline(always)]
    pub fn is_latin1(&self) -> bool {
        match self.try_impl() {
            Some(string) => string.is_latin1(),
            None => self.fiber() & JsRopeString::IS_LATIN1 != 0,
        }
    }

    #[inline(always)]
    pub fn is_atom(&self) -> bool {
        self.per_cell_bit()
    }

    #[inline(always)]
    pub fn try_resolved(&self) -> Option<ResolvedString> {
        let fiber = self.fiber();
        (fiber & Self::IS_ROPE == 0).then(|| ResolvedString {
            owner: self.into(),
            string: impl_at(fiber),
        })
    }

    #[inline(always)]
    pub fn try_resolve(&self, vm: &Vm) -> Result<ResolvedString, AllocError> {
        if let Some(rope) = self.as_rope() {
            rope.flatten(vm)?;
        }
        Ok(ResolvedString {
            owner: self.into(),
            string: impl_at(self.fiber()),
        })
    }

    #[inline(always)]
    pub fn resolve(&self, cx: &Cx) -> JsResult<ResolvedString> {
        self.try_resolve(cx).map_err(|_| cx.throw_out_of_memory())
    }

    pub fn to_string_ref(&self, cx: &Cx) -> JsResult<StringRef> {
        Ok(StringRef::retain(&*self.resolve(cx)?))
    }

    /// Unless the characters have to be put together first.
    #[inline(always)]
    pub fn try_view(&self) -> Option<StringView> {
        let (owner, offset, length) = match self.as_rope() {
            None => (self.into(), 0, None),
            Some(rope) => {
                let (base, offset) = rope.as_substring()?;
                (base, offset, Some(rope.rope_length()))
            }
        };
        // SAFETY: as in `ResolvedString::deref`. The base of a substring is no rope.
        let mut text = unsafe { impl_at(owner.fiber()).as_ref() }.text();
        if let Some(length) = length {
            text = text.slice(offset as usize..(offset + length) as usize);
        }
        Some(StringView {
            owner,
            offset,
            text,
        })
    }

    #[inline(always)]
    pub fn view(&self, cx: &Cx) -> JsResult<StringView> {
        if let Some(view) = self.try_view() {
            return Ok(view);
        }
        let resolved = self.resolve(cx)?;
        Ok(StringView {
            owner: resolved.owner,
            offset: 0,
            // SAFETY: as in `ResolvedString::deref`.
            text: unsafe { resolved.string.as_ref() }.text(),
        })
    }

    #[inline(always)]
    pub fn to_property_key(&self, cx: &Cx) -> JsResult<PropertyKey> {
        if !self.is_atom() {
            self.atomize(cx).map_err(|_| cx.throw_out_of_memory())?;
        }
        // SAFETY: an atom, which is never swapped again.
        Ok(unsafe { PropertyKey::from_atom_string(self.into(), impl_at(self.fiber())) })
    }

    pub fn to_identifier(&self, cx: &Cx) -> JsResult<Identifier> {
        Ok(self.to_property_key(cx)?.to_identifier())
    }

    /// `None` if no atom has these characters, so that no ordinary object has such a property.
    #[inline]
    pub fn to_existing_property_key(&self, cx: &Cx) -> JsResult<Option<PropertyKey>> {
        if !self.is_atom() {
            let Some(atom) = Atom::find_string(&*self.resolve(cx)?) else {
                return Ok(None);
            };
            self.swap_to_atom(cx, atom);
        }
        self.to_property_key(cx).map(Some)
    }

    #[inline(never)]
    fn atomize(&self, vm: &Vm) -> Result<(), AllocError> {
        if let Some(rope) = self.as_rope() {
            return rope.flatten_to_atom(vm);
        }
        // SAFETY: no rope, and nothing is allocated in the heap meanwhile.
        let atom = Atom::from_string(unsafe { impl_at(self.fiber()).as_ref() });
        self.swap_to_atom(vm, atom);
        Ok(())
    }

    fn swap_to_atom(&self, vm: &Vm, atom: Atom) {
        let old = self.fiber();
        if old == atom.address() {
            return self.set_per_cell_bit(true);
        }
        self.become_resolved(atom.into_string());
        // SAFETY: the reference that the cell held.
        let old = unsafe { StringRef::from_raw(impl_at(old)) };
        vm.string_caches.retire(self.address(), old);
    }

    /// `None` unless both are atoms.
    #[inline(always)]
    pub fn equals_if_atoms(&self, other: &JsString) -> Option<bool> {
        (self.is_atom() && other.is_atom()).then(|| self.fiber() == other.fiber())
    }

    #[inline(always)]
    pub fn equals(&self, cx: &Cx, other: &JsString) -> JsResult<bool> {
        if let (Some(a), Some(b)) = (self.try_impl(), other.try_impl()) {
            return Ok(a == b);
        }
        self.equals_slow(cx, other)
    }

    #[inline(never)]
    fn equals_slow(&self, cx: &Cx, other: &JsString) -> JsResult<bool> {
        Ok(self.len() == other.len() && self.view(cx)?.text() == other.view(cx)?.text())
    }

    pub fn compare(&self, cx: &Cx, other: &JsString) -> JsResult<Comparison> {
        Ok(self.view(cx)?.text().cmp(&other.view(cx)?.text()))
    }

    /// Panics past the end.
    #[inline]
    pub fn unit_at(&self, cx: &Cx, index: u32) -> JsResult<u16> {
        Ok(self.view(cx)?.text().at(index as usize))
    }

    /// Panics past the end.
    #[inline]
    pub fn string_at(&self, cx: &Cx, index: u32) -> JsResult<Gc<JsString>> {
        Ok(Self::from_unit(cx, self.unit_at(cx, index)?))
    }

    /// Panics if the range is not inside the string.
    #[inline]
    pub fn substring(&self, cx: &Cx, start: u32, end: u32) -> JsResult<Gc<JsString>> {
        if start == 0 && end == self.len() {
            return Ok(self.into());
        }
        if start == end {
            return Ok(cx.small_strings.empty());
        }
        Ok(self.view(cx)?.substring(cx, start as usize..end as usize))
    }

    pub fn to_number(&self, cx: &Cx) -> JsResult<f64> {
        Ok(bun_engine_number::string_to_number(self.view(cx)?.text()))
    }

    /// The properties that a String object has because of its value.
    pub fn get_string_property_slot(
        &self,
        cx: &Cx,
        name: PropertyName<'_>,
        slot: &mut PropertySlot,
    ) -> JsResult<bool> {
        if name == names::length {
            slot.set_value_without_base(Attributes::CONSTANT, self.len().into());
            return Ok(true);
        }
        match name.as_index() {
            Some(index) => self.get_string_index_slot(cx, index, slot),
            None => Ok(false),
        }
    }

    pub fn get_string_index_slot(
        &self,
        cx: &Cx,
        index: u32,
        slot: &mut PropertySlot,
    ) -> JsResult<bool> {
        if index >= self.len() {
            return Ok(false);
        }
        let attributes = Attributes::DONT_DELETE | Attributes::READ_ONLY;
        slot.set_value_without_base(attributes, self.string_at(cx, index)?.into());
        Ok(true)
    }

    #[inline(always)]
    pub(crate) fn visit_fibers(&self, visitor: &mut Visitor) {
        let fiber = self.fiber_atomic().load(Ordering::Acquire);
        if fiber & Self::IS_ROPE != 0 {
            // SAFETY: only a `JsRopeString` has the bit.
            let rope = unsafe { Gc::from(self).cast_unchecked::<JsRopeString>() };
            rope.visit_rope(fiber, visitor);
        } else if visitor.is_first_visit() {
            // SAFETY: what is swapped out is released only after marking.
            visitor.report_extra_memory(unsafe { impl_at(fiber).as_ref() }.shared_cost());
        }
    }
}
