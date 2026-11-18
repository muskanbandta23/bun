use core::mem::offset_of;
use core::ops::Deref;

use bun_alloc::AllocError;
use bun_engine_text::{
    Atom, CodeUnit, MAX_LENGTH, ReverseStringWriter, StringImpl, StringRef, Text,
};
use smallvec::SmallVec;

use crate::heap::barrier;
use crate::{Cx, Gc, GcFree, JsCell, JsResult, JsString, Visitor, Vm};

/// A concatenation of two or three strings, or a part of one, whose characters are copied when they are needed.
#[repr(C)]
pub struct JsRopeString {
    base: JsString,
    /// The length, and above it the low 32 bits of the second fiber.
    length_and_fiber1: u64,
    /// The high 16 bits of the second fiber, and above them the third one or the offset of a substring.
    fiber1_and_fiber2: u64,
}

impl !GcFree for JsRopeString {}

impl Deref for JsRopeString {
    type Target = JsString;

    #[inline(always)]
    fn deref(&self) -> &JsString {
        &self.base
    }
}

/// Below this, copying is cheaper than making a rope and resolving it.
const MAX_EAGER_CONCATENATION: u32 = 12;
const MAX_ATOM_ON_STACK: usize = 256;

impl JsRopeString {
    pub const IS_SUBSTRING: usize = 2;
    pub const IS_LATIN1: usize = StringImpl::IS_LATIN1 as usize;
    pub const FIBER_MASK: usize = !7;
    pub const OFFSET_OF_LENGTH: usize = offset_of!(JsRopeString, length_and_fiber1);
    pub const OFFSET_OF_FIBER2: usize = offset_of!(JsRopeString, fiber1_and_fiber2);

    #[inline(always)]
    fn allocate(vm: &Vm, fiber0: usize, fiber1: usize, fiber2: usize, length: u32) -> Gc<JsString> {
        let allocator = &vm.heap.rope_strings.allocator;
        let address = allocator.try_allocate_sized(&vm.heap, size_of::<JsRopeString>());
        if address == 0 {
            bun_alloc::out_of_memory();
        }
        let words = [
            JsCell::header_for(&vm.shapes.string()),
            (fiber0 | JsString::IS_ROPE) as u64,
            u64::from(length) | (fiber1 as u64) << 32,
            (fiber1 as u64) >> 32 | (fiber2 as u64) << 16,
        ];
        // SAFETY: a new cell of four words.
        unsafe {
            core::ptr::with_exposed_provenance_mut::<[u64; 4]>(address).write(words);
            barrier::publish_fence();
            Gc::from_address(address)
        }
    }

    #[inline(always)]
    pub(super) fn rope_length(&self) -> u32 {
        self.length_and_fiber1 as u32
    }

    #[inline(always)]
    fn fiber1(&self) -> usize {
        (self.length_and_fiber1 >> 32 | (self.fiber1_and_fiber2 & 0xffff) << 32) as usize
    }

    #[inline(always)]
    fn fiber2(&self) -> usize {
        (self.fiber1_and_fiber2 >> 16) as usize
    }

    #[inline(always)]
    pub(super) fn as_substring(&self) -> Option<(Gc<JsString>, u32)> {
        // SAFETY: the rope keeps its base alive.
        (self.fiber() & Self::IS_SUBSTRING != 0).then(|| {
            (
                unsafe { Gc::from_address(self.fiber1()) },
                self.fiber2() as u32,
            )
        })
    }

    #[inline(always)]
    pub(super) fn visit_rope(&self, fiber0: usize, visitor: &mut Visitor) {
        // SAFETY: zero, or a string that the rope keeps alive.
        unsafe {
            visitor.append_word(self.fiber1() as u64);
            if fiber0 & Self::IS_SUBSTRING == 0 {
                visitor.append_word((fiber0 & Self::FIBER_MASK) as u64);
                visitor.append_word(self.fiber2() as u64);
            }
        }
    }

    /// `base` is no rope, and the range is inside it.
    #[inline]
    pub fn substring_of_resolved(
        vm: &Vm,
        base: Gc<JsString>,
        start: u32,
        length: u32,
    ) -> Gc<JsString> {
        let resolved = base.try_resolved().expect("no rope");
        let text = resolved
            .text()
            .slice(start as usize..start as usize + length as usize);
        if length <= 2 {
            if let Some(small) = JsString::small(vm, text) {
                return small;
            }
            if let Text::Latin1(&[first, second]) = text {
                return vm.string_caches.two_characters(vm, first, second);
            }
        }
        if length as usize == resolved.len() {
            return base;
        }
        let latin1 = if text.is_latin1() { Self::IS_LATIN1 } else { 0 };
        let flags = Self::IS_SUBSTRING | latin1;
        Self::allocate(vm, flags, base.address(), start as usize, length)
    }

    /// From the last to the first.
    #[inline(always)]
    fn for_each_leaf_reversed(&self, mut visit: impl FnMut(Text<'_>)) {
        let mut pending = SmallVec::<[usize; 32]>::new();
        let mut current = self.address();
        loop {
            // SAFETY: reachable from `self`, and `visit` does not collect.
            let string = unsafe { Gc::<JsString>::from_address(current) };
            if let Some(view) = string.try_view() {
                visit(view.text());
                match pending.pop() {
                    Some(next) => current = next,
                    None => return,
                }
                continue;
            }
            // SAFETY: what has no view is a rope.
            let rope = unsafe { string.cast_unchecked::<JsRopeString>() };
            pending.push(rope.fiber() & Self::FIBER_MASK);
            current = rope.fiber1();
            if rope.fiber2() != 0 {
                pending.push(current);
                current = rope.fiber2();
            }
        }
    }

    fn concatenate<T: CodeUnit>(&self) -> Result<StringRef, AllocError> {
        let mut writer = ReverseStringWriter::<T>::try_new(self.rope_length() as usize)?;
        self.for_each_leaf_reversed(|text| writer.prepend(text));
        Ok(writer.finish())
    }

    fn characters(&self) -> Result<StringRef, AllocError> {
        if let Some((base, start)) = self.as_substring() {
            let range = start as usize..(start + self.rope_length()) as usize;
            return Ok(StringRef::substring(
                base.try_impl().expect("no rope"),
                range,
            ));
        }
        if self.is_latin1() {
            self.concatenate::<u8>()
        } else {
            self.concatenate::<u16>()
        }
    }

    #[inline(never)]
    pub(super) fn flatten(&self, vm: &Vm) -> Result<(), AllocError> {
        let string = self.characters()?;
        let cost = string.take_cost();
        self.become_resolved(string);
        vm.heap.report_extra_memory_allocated(self, cost);
        Ok(())
    }

    fn atom_via_stack<T: CodeUnit>(&self) -> Atom {
        let mut buffer = [T::default(); MAX_ATOM_ON_STACK];
        let mut start = MAX_ATOM_ON_STACK;
        self.for_each_leaf_reversed(|text| {
            start -= text.len();
            match T::units(text) {
                Some(units) => buffer[start..][..units.len()].copy_from_slice(units),
                None => {
                    for (target, unit) in buffer[start..].iter_mut().zip(text.code_units()) {
                        *target = T::from_u16_truncating(unit);
                    }
                }
            }
        });
        Atom::new(T::text(&buffer[start..]))
    }

    pub(super) fn flatten_to_atom(&self, vm: &Vm) -> Result<(), AllocError> {
        let atom = if self.rope_length() as usize > MAX_ATOM_ON_STACK {
            Atom::from_string(&*self.characters()?)
        } else if self.is_latin1() {
            self.atom_via_stack::<u8>()
        } else {
            self.atom_via_stack::<u16>()
        };
        let cost = atom.take_cost();
        self.become_resolved(atom.into_string());
        vm.heap.report_extra_memory_allocated(self, cost);
        Ok(())
    }
}

impl JsString {
    /// The pieces that the string consists of, from the last to the first, without putting them together.
    pub fn for_each_piece_reversed(&self, _: &crate::NoGc<'_>, mut visit: impl FnMut(Text<'_>)) {
        match (self.try_view(), self.as_rope()) {
            (Some(view), _) => visit(view.text()),
            (None, Some(rope)) => rope.for_each_leaf_reversed(visit),
            (None, None) => unreachable!(),
        }
    }

    /// Leaves a rope as it is, and allocates nothing in the heap. Empty if there is no memory.
    pub fn to_string_without_resolving(&self) -> StringRef {
        match self.as_rope() {
            Some(rope) => rope.characters().unwrap_or_default(),
            None => self.try_impl().map_or_default(StringRef::retain),
        }
    }

    #[inline]
    pub fn try_concat(vm: &Vm, a: Gc<JsString>, b: Gc<JsString>) -> Result<Gc<Self>, AllocError> {
        let (a_length, b_length) = (a.len(), b.len());
        if a_length == 0 {
            return Ok(b);
        }
        if b_length == 0 {
            return Ok(a);
        }
        let length = a_length + b_length;
        if length as usize > MAX_LENGTH {
            return Err(AllocError);
        }
        if length <= MAX_EAGER_CONCATENATION
            && let (Some(a), Some(b)) = (a.try_impl(), b.try_impl())
        {
            let string = StringRef::try_concat(&[a.text(), b.text()])?;
            return Ok(JsString::allocate(vm, string));
        }
        let latin1 = usize::from(a.is_latin1() && b.is_latin1()) * JsRopeString::IS_LATIN1;
        let rope = JsRopeString::allocate(vm, a.address() | latin1, b.address(), 0, length);
        a.keep_alive();
        Ok(rope)
    }

    #[inline]
    pub fn concat(cx: &Cx, a: Gc<JsString>, b: Gc<JsString>) -> JsResult<Gc<JsString>> {
        Self::try_concat(cx, a, b).map_err(|_| cx.throw_out_of_memory())
    }

    pub fn concat3(
        cx: &Cx,
        a: Gc<JsString>,
        b: Gc<JsString>,
        c: Gc<JsString>,
    ) -> JsResult<Gc<JsString>> {
        if a.is_empty() {
            return Self::concat(cx, b, c);
        }
        if b.is_empty() {
            return Self::concat(cx, a, c);
        }
        if c.is_empty() {
            return Self::concat(cx, a, b);
        }
        let length = u64::from(a.len()) + u64::from(b.len()) + u64::from(c.len());
        if length > MAX_LENGTH as u64 {
            return Err(cx.throw_out_of_memory());
        }
        let is_latin1 = a.is_latin1() && b.is_latin1() && c.is_latin1();
        let first = a.address() | (usize::from(is_latin1) * JsRopeString::IS_LATIN1);
        let rope = JsRopeString::allocate(cx, first, b.address(), c.address(), length as u32);
        for fiber in [a, b, c] {
            fiber.keep_alive();
        }
        Ok(rope)
    }
}

/// Concatenates any number of strings.
#[derive(Default)]
pub struct RopeBuilder {
    first: Option<Gc<JsString>>,
    second: Option<Gc<JsString>>,
}

impl RopeBuilder {
    pub fn push(&mut self, cx: &Cx, string: Gc<JsString>) -> JsResult<()> {
        match (self.first, self.second) {
            _ if string.is_empty() => {}
            (None, _) => self.first = Some(string),
            (Some(_), None) => self.second = Some(string),
            (Some(first), Some(second)) => {
                self.first = Some(JsString::concat3(cx, first, second, string)?);
                self.second = None;
            }
        }
        Ok(())
    }

    pub fn finish(self, cx: &Cx) -> JsResult<Gc<JsString>> {
        match (self.first, self.second) {
            (None, _) => Ok(cx.small_strings.empty()),
            (Some(only), None) => Ok(only),
            (Some(first), Some(second)) => JsString::concat(cx, first, second),
        }
    }
}
