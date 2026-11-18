//! The atoms of a thread. The table holds no references: an atom leaves it when its last reference goes.

use core::cell::{Cell, RefCell};

use crate::static_names::{ATOM_SLOTS, ATOMS};
use crate::{AllocError, StringImpl, StringRef, Text, parse_array_index};

/// The address of an atom under the high 16 bits of its hash, which spare most probes a look at the string.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Slot(u64);

impl Slot {
    const EMPTY: Slot = Slot(0);
    const DELETED: Slot = Slot(1);
    const TAG_SHIFT: u32 = 48;

    #[inline(always)]
    fn tag(hash: u32) -> u64 {
        u64::from(hash >> 8)
    }

    #[inline(always)]
    fn new(atom: &StringImpl) -> Slot {
        let address = core::ptr::from_ref(atom).expose_provenance() as u64;
        debug_assert!(address >> Self::TAG_SHIFT == 0);
        Slot(Self::tag(atom.existing_hash()) << Self::TAG_SHIFT | address)
    }

    #[inline(always)]
    fn atom(&self) -> Option<&StringImpl> {
        let address = (self.0 & ((1 << Self::TAG_SHIFT) - 1)) as usize;
        // SAFETY: an atom is taken out of the table before it is freed.
        (address > 1).then(|| unsafe { &*core::ptr::with_exposed_provenance(address) })
    }
}

struct AtomTable {
    slots: Box<[Slot]>,
    live: usize,
    deleted: usize,
}

impl AtomTable {
    fn new() -> AtomTable {
        let slots = ATOM_SLOTS.iter().map(|&seed| match seed & 0xffff {
            0 => Slot::EMPTY,
            index => {
                let address = ATOMS[index as usize - 1]
                    .as_ptr()
                    .as_ptr()
                    .expose_provenance();
                Slot(u64::from(seed >> 16) << Slot::TAG_SHIFT | address as u64)
            }
        });
        AtomTable {
            slots: slots.collect(),
            live: ATOMS.len(),
            deleted: 0,
        }
    }

    /// The atom, or the slot to put it in.
    #[inline]
    fn find(&self, text: Text<'_>, hash: u32) -> Result<&StringImpl, usize> {
        let mask = self.slots.len() - 1;
        let tag = Slot::tag(hash);
        let mut index = hash as usize & mask;
        let mut reusable = None;
        loop {
            let slot = &self.slots[index];
            if slot.0 >> Slot::TAG_SHIFT == tag
                && let Some(atom) = slot.atom()
                && atom.text() == text
            {
                return Ok(atom);
            }
            if *slot == Slot::EMPTY {
                return Err(reusable.unwrap_or(index));
            }
            if *slot == Slot::DELETED && reusable.is_none() {
                reusable = Some(index);
            }
            index = (index + 1) & mask;
        }
    }

    fn insert(&mut self, index: usize, atom: &StringImpl) {
        if self.slots[index] == Slot::DELETED {
            self.deleted -= 1;
        }
        self.slots[index] = Slot::new(atom);
        self.live += 1;
        if (self.live + self.deleted) * 3 > self.slots.len() * 2 {
            self.rehash();
        }
    }

    fn remove(&mut self, atom: &StringImpl) {
        let mask = self.slots.len() - 1;
        let wanted = Slot::new(atom);
        let mut index = atom.existing_hash() as usize & mask;
        while self.slots[index] != wanted {
            assert!(
                self.slots[index] != Slot::EMPTY,
                "an atom was released on another thread than its own"
            );
            index = (index + 1) & mask;
        }
        self.live -= 1;
        if self.slots[(index + 1) & mask] == Slot::EMPTY {
            self.slots[index] = Slot::EMPTY;
        } else {
            self.slots[index] = Slot::DELETED;
            self.deleted += 1;
        }
        if self.live * 16 < self.slots.len() && self.slots.len() > ATOM_SLOTS.len() {
            self.rehash();
        }
    }

    #[cold]
    fn rehash(&mut self) {
        let capacity = (self.live * 3).next_power_of_two().max(ATOM_SLOTS.len());
        let mask = capacity - 1;
        let mut slots = vec![Slot::EMPTY; capacity].into_boxed_slice();
        for slot in &self.slots {
            if let Some(atom) = slot.atom() {
                let mut index = atom.existing_hash() as usize & mask;
                while slots[index] != Slot::EMPTY {
                    index = (index + 1) & mask;
                }
                slots[index] = *slot;
            }
        }
        self.slots = slots;
        self.deleted = 0;
    }
}

impl Drop for AtomTable {
    /// What outlives the thread is an ordinary string from now on.
    fn drop(&mut self) {
        HAS_ENDED.set(true);
        for atom in self.slots.iter().filter_map(Slot::atom) {
            if !atom.is_static() {
                atom.unmark_as_atom();
            }
        }
    }
}

thread_local! {
    static TABLE: RefCell<AtomTable> = RefCell::new(AtomTable::new());
    static HAS_ENDED: Cell<bool> = const { Cell::new(false) };
}

/// The atom with these characters. `create` makes the string if there is none; it has no hash or the right one.
#[inline]
pub(crate) fn add(
    text: Text<'_>,
    hash: u32,
    create: impl FnOnce() -> Result<StringRef, AllocError>,
) -> Result<StringRef, AllocError> {
    if text.is_empty() {
        return Ok(StringRef::empty());
    }
    TABLE.with_borrow_mut(|table| match table.find(text, hash) {
        Ok(atom) => Ok(StringRef::retain(atom)),
        Err(index) => {
            let atom = create()?;
            if atom.existing_hash() == 0 {
                atom.set_hash(hash);
            }
            atom.mark_as_atom(parse_array_index(text).is_some());
            table.insert(index, &atom);
            Ok(atom)
        }
    })
}

#[inline]
pub(crate) fn find(text: Text<'_>, hash: u32) -> Option<StringRef> {
    if text.is_empty() {
        return Some(StringRef::empty());
    }
    TABLE.with_borrow(|table| table.find(text, hash).ok().map(StringRef::retain))
}

pub(crate) fn length_of_longest() -> usize {
    TABLE.with_borrow(|table| {
        let atoms = table.slots.iter().filter_map(Slot::atom);
        atoms.map(StringImpl::len).max().unwrap_or(0)
    })
}

pub(crate) fn remove(atom: &StringImpl) {
    if !HAS_ENDED.get() {
        TABLE.with_borrow_mut(|table| table.remove(atom));
    }
}
