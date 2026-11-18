//! The strings that have been made since the last collection, because values repeat.

use bun_alloc::AllocError;
use bun_engine_text::{Atom, CodeUnit, StringRef, Text, equal_units, with_text};

use crate::{Gc, JsString, Vm};

pub(super) const SHORT: usize = 16;
const LONGEST: usize = 256;
const COUNT: usize = 256;

#[derive(Clone, Copy)]
#[repr(C, align(32))]
struct Short {
    /// Latin-1 without control characters, and zeros after it.
    characters: u128,
    string: usize,
}

pub(super) struct Strings {
    short: [Short; COUNT],
    /// The address, under some more bits of the hash.
    long: [u64; COUNT],
}

const VACANT: Short = Short {
    characters: 1,
    string: 0,
};

impl Default for Strings {
    fn default() -> Strings {
        Strings {
            short: [VACANT; COUNT],
            long: [0; COUNT],
        }
    }
}

#[inline(always)]
fn mix(a: u64, b: u64) -> u64 {
    let product = u128::from(a ^ 0x2d35_8dcc_aa6c_78a5) * u128::from(b ^ 0x8bb8_4b93_962e_acc9);
    product as u64 ^ (product >> 64) as u64
}

static KEPT: [u128; SHORT + 1] = {
    let mut masks = [u128::MAX; SHORT + 1];
    let mut length = 0;
    while length < SHORT {
        masks[length] = (1 << (8 * length)) - 1;
        length += 1;
    }
    masks
};

impl Strings {
    pub(super) fn clear(&mut self) {
        self.short.fill(VACANT);
        self.long.fill(0);
    }

    /// Of the first `length` characters, none of which is a control character.
    #[inline(always)]
    pub(super) fn short(
        &mut self,
        vm: &Vm,
        characters: [u8; SHORT],
        length: usize,
    ) -> Result<Gc<JsString>, AllocError> {
        let characters = u128::from_le_bytes(characters) & KEPT[length];
        let hash = mix(characters as u64, (characters >> 64) as u64);
        let entry = &mut self.short[hash as usize % COUNT];
        if entry.characters == characters {
            // SAFETY: made since the last collection.
            return Ok(unsafe { Gc::from_address(entry.string) });
        }
        Self::add_short(entry, vm, characters, length)
    }

    #[inline(never)]
    fn add_short(
        entry: &mut Short,
        vm: &Vm,
        characters: u128,
        length: usize,
    ) -> Result<Gc<JsString>, AllocError> {
        /// What fills the two smallest blocks that have room for the header.
        const SMALL: usize = 12;
        const LARGE: usize = 28;
        let mut bytes = [0; LARGE];
        bytes[..SHORT].copy_from_slice(&characters.to_le_bytes());
        // What a script compares it with, or uses it for, is likely to be a name.
        let string = match (Atom::find(Text::Latin1(&bytes[..length])), bytes.first_chunk()) {
            (Some(atom), _) => atom.into_string(),
            (None, Some(few)) if length <= SMALL => StringRef::try_from_prefix::<_, SMALL>(few, length)?,
            _ => StringRef::try_from_prefix(&bytes, length)?,
        };
        let string = JsString::new(vm, string);
        *entry = Short {
            characters,
            string: string.address(),
        };
        Ok(string)
    }

    /// Latin-1 if the characters allow it.
    pub(super) fn any(&mut self, vm: &Vm, text: Text<'_>) -> Result<Gc<JsString>, AllocError> {
        with_text!(text, |units| self.of_units(vm, units))
    }

    #[inline(always)]
    fn of_units<T: CodeUnit>(&mut self, vm: &Vm, units: &[T]) -> Result<Gc<JsString>, AllocError> {
        const ADDRESS_BITS: u32 = 48;
        let create = || Ok(JsString::new(vm, StringRef::try_narrowed(T::text(units))?));
        if !(2..=LONGEST).contains(&units.len()) {
            return create();
        }
        let hash = sample(bun_pod::slice_as_bytes(units));
        let entry = &mut self.long[hash as usize % COUNT];
        let address = *entry ^ hash >> ADDRESS_BITS << ADDRESS_BITS;
        if address >> ADDRESS_BITS == 0 && address != 0 {
            // SAFETY: made since the last collection.
            let string = unsafe { Gc::<JsString>::from_address(address as usize) };
            let is_equal = |it| with_text!(it, |cached| equal_units(cached, units));
            if string.try_impl().is_some_and(|it| is_equal(it.text())) {
                return Ok(string);
            }
        }
        let string = create()?;
        *entry = hash >> ADDRESS_BITS << ADDRESS_BITS | string.address() as u64;
        Ok(string)
    }
}

/// A hash of the length and of some of the bytes, which are two or more.
#[inline(always)]
fn sample(bytes: &[u8]) -> u64 {
    let length = bytes.len();
    let (Some(head), Some(tail)) = (bytes.first_chunk(), bytes.last_chunk()) else {
        let few = [0, 1, length / 2 - 1, length / 2, length - 2, length - 1];
        let few = few.iter().fold(0, |word, &at| word << 8 | u64::from(bytes[at]));
        return mix(few, length as u64);
    };
    let middle = bytes[length / 2 - 4..].first_chunk().unwrap_or(head);
    let [head, middle, tail] = [head, middle, tail].map(|it| u64::from_le_bytes(*it));
    mix(head ^ middle.rotate_left(20), tail ^ (length as u64) << 7)
}
