use core::cell::{Cell, RefCell};

use bun_engine_number::NumberText;
use bun_engine_text::{Atom, PropertyName, STATIC_ATOM_COUNT, StringImpl, StringRef, Text};

use crate::heap::Scope;
use crate::{FastHashSet, Gc, JsString, Visitor, Vm};

/// The strings of the static atoms, the first 256 of which are those of one Latin-1 character. They never die.
#[repr(C)]
pub struct SmallStrings {
    empty: Cell<usize>,
    /// Zero until it is needed.
    of_static_atoms: [Cell<usize>; STATIC_ATOM_COUNT],
}

impl SmallStrings {
    pub const OFFSET_OF_EMPTY: usize = core::mem::offset_of!(SmallStrings, empty);
    pub const OFFSET_OF_SINGLE_CHARACTERS: usize =
        core::mem::offset_of!(SmallStrings, of_static_atoms);

    pub(crate) const fn new() -> SmallStrings {
        SmallStrings {
            empty: Cell::new(0),
            of_static_atoms: [const { Cell::new(0) }; STATIC_ATOM_COUNT],
        }
    }

    pub(crate) fn initialize(&self, vm: &Vm) {
        self.empty
            .set(JsString::allocate(vm, StringRef::empty()).address());
    }

    #[inline(always)]
    pub fn empty(&self) -> Gc<JsString> {
        // SAFETY: a root, made with the `Vm`.
        unsafe { Gc::from_address(self.empty.get()) }
    }

    #[inline(always)]
    fn get_or_create(
        &self,
        vm: &Vm,
        index: usize,
        string: impl FnOnce() -> StringRef,
    ) -> Gc<JsString> {
        match self.of_static_atoms[index].get() {
            0 => self.create(vm, index, string()),
            // SAFETY: a root.
            address => unsafe { Gc::from_address(address) },
        }
    }

    #[cold]
    #[inline(never)]
    fn create(&self, vm: &Vm, index: usize, string: StringRef) -> Gc<JsString> {
        let string = JsString::allocate(vm, string);
        self.of_static_atoms[index].set(string.address());
        string
    }

    #[inline(always)]
    pub fn single_character(&self, vm: &Vm, character: u8) -> Gc<JsString> {
        self.get_or_create(vm, usize::from(character), || {
            StringRef::single_character(character)
        })
    }

    #[inline(always)]
    pub fn existing_single_character(&self, character: u8) -> Option<Gc<JsString>> {
        let address = self.of_static_atoms[usize::from(character)].get();
        // SAFETY: a root.
        (address != 0).then(|| unsafe { Gc::from_address(address) })
    }

    /// Panics unless the index is below `STATIC_ATOM_COUNT`.
    pub fn of_static_atom(&self, vm: &Vm, index: usize) -> Gc<JsString> {
        self.get_or_create(vm, index, || StringRef::static_atom(index))
    }

    /// For generated code, which does not look whether one exists.
    pub fn create_all_single_characters(&self, vm: &Vm) {
        for character in 0..=u8::MAX {
            self.single_character(vm, character);
        }
    }

    pub(crate) fn visit(&self, visitor: &mut Visitor) {
        for string in core::iter::once(&self.empty).chain(&self.of_static_atoms) {
            // SAFETY: zero or a root.
            unsafe { visitor.append_hidden_word(string.get() as u64) };
        }
    }
}

const NAME_CACHE_SIZE: usize = 512;
const NUMBER_CACHE_SIZE: usize = 256;
const PAIR_CACHE_SIZE: usize = 256;

/// The strings are forgotten at each collection, so nothing has to keep them alive.
pub(crate) struct StringCaches {
    names: [Cell<usize>; NAME_CACHE_SIZE],
    /// The bits of the number, and the string.
    numbers: [Cell<(u64, usize)>; NUMBER_CACHE_SIZE],
    pairs: [Cell<(u16, usize)>; PAIR_CACHE_SIZE],
    /// Characters that a string has swapped for an atom, which a view of it may still be reading.
    retired: RefCell<Vec<(usize, StringRef)>>,
}

impl StringCaches {
    pub(crate) const fn new() -> StringCaches {
        StringCaches {
            names: [const { Cell::new(0) }; NAME_CACHE_SIZE],
            numbers: [const { Cell::new((0, 0)) }; NUMBER_CACHE_SIZE],
            pairs: [const { Cell::new((0, 0)) }; PAIR_CACHE_SIZE],
            retired: RefCell::new(Vec::new()),
        }
    }

    pub(super) fn retire(&self, owner: usize, string: StringRef) {
        self.retired.borrow_mut().push((owner, string));
    }

    pub(crate) fn did_collect(&self, _: Scope, cells_on_stack: &[usize]) {
        self.names.iter().for_each(|entry| entry.set(0));
        self.numbers.iter().for_each(|entry| entry.set((0, 0)));
        self.pairs.iter().for_each(|entry| entry.set((0, 0)));
        let mut retired = self.retired.take();
        if !retired.is_empty() {
            let on_stack: FastHashSet<usize> = cells_on_stack.iter().copied().collect();
            retired.retain(|entry| on_stack.contains(&entry.0));
            self.retired.replace(retired);
        }
    }

    pub(super) fn two_characters(&self, vm: &Vm, first: u8, second: u8) -> Gc<JsString> {
        let key = u16::from_le_bytes([first, second]);
        let entry = &self.pairs[(usize::from(first) * 31 + usize::from(second)) % PAIR_CACHE_SIZE];
        if let (cached, address @ 1..) = entry.get()
            && cached == key
        {
            // SAFETY: made since the last collection.
            return unsafe { Gc::from_address(address) };
        }
        let atom = Atom::new(Text::Latin1(&[first, second]));
        let string = JsString::allocate(vm, atom.into_string());
        entry.set((key, string.address()));
        string
    }
}

impl JsString {
    /// Of a symbol, the description.
    #[inline(always)]
    pub fn from_name(vm: &Vm, name: PropertyName<'_>) -> Gc<JsString> {
        match name.static_atom_index() {
            Some(index) => vm
                .small_strings
                .get_or_create(vm, index, || StringRef::retain(name.string())),
            None => Self::from_impl_cached(vm, name.string()),
        }
    }

    /// For a string that is likely to be asked for again.
    pub fn from_impl_cached(vm: &Vm, string: &StringImpl) -> Gc<JsString> {
        if string.is_symbol() {
            return Self::new(vm, StringRef::substring(string, 0..string.len()));
        }
        let entry = &vm.string_caches.names[string.address() / 16 % NAME_CACHE_SIZE];
        if entry.get() != 0 {
            // SAFETY: looked up since the last collection.
            let cached = unsafe { Gc::<JsString>::from_address(entry.get()) };
            if cached.fiber() == string.address() {
                return cached;
            }
        }
        let result = Self::new(vm, StringRef::retain(string));
        entry.set(result.address());
        result
    }

    #[inline]
    pub fn from_int32(vm: &Vm, number: i32) -> Gc<JsString> {
        if let 0..=9 = number {
            return vm.small_strings.single_character(vm, b'0' + number as u8);
        }
        let key = crate::Value::int32(number).to_bits();
        Self::from_number_cached(vm, key, number as usize, || NumberText::from(number))
    }

    /// Number::toString in radix 10.
    #[inline]
    pub fn from_number(vm: &Vm, number: f64) -> Gc<JsString> {
        let integer = number as i32;
        if f64::from(integer) == number {
            return Self::from_int32(vm, integer);
        }
        let key = crate::Value::double(number).to_bits();
        let hash = key.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 40;
        Self::from_number_cached(vm, key, hash as usize, || NumberText::from(number))
    }

    #[inline(always)]
    fn from_number_cached(
        vm: &Vm,
        key: u64,
        hash: usize,
        text: impl FnOnce() -> NumberText,
    ) -> Gc<JsString> {
        let entry = &vm.string_caches.numbers[hash % NUMBER_CACHE_SIZE];
        if entry.get().0 == key {
            // SAFETY: made since the last collection.
            return unsafe { Gc::from_address(entry.get().1) };
        }
        let string = Self::allocate(vm, StringRef::from_text(text().text()));
        entry.set((key, string.address()));
        string
    }
}
