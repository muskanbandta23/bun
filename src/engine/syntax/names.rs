//! The names and strings of one parse, each as a small number.

use core::num::NonZeroU32;

use bun_engine_text::Text;

use crate::source::Source;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Name(NonZeroU32);

impl Name {
    const fn new(index: usize) -> Name {
        match NonZeroU32::new(index as u32) {
            Some(index) => Name(index),
            None => panic!("zero is no name"),
        }
    }

    /// Below [`Names::limit`].
    #[inline(always)]
    pub const fn index(self) -> usize {
        self.0.get() as usize
    }

    #[inline(always)]
    pub(crate) const fn from_word(word: u32) -> Option<Name> {
        match NonZeroU32::new(word) {
            Some(index) => Some(Name(index)),
            None => None,
        }
    }

    /// Whether only the engine can refer to it.
    #[inline(always)]
    pub const fn is_internal(self) -> bool {
        self.index() >= Name::THIS_VALUE.index() && self.index() < KNOWN.len()
    }
}

macro_rules! known_names {
    ($($name:ident = $text:literal,)*) => {
        #[allow(non_camel_case_types, clippy::upper_case_acronyms)]
        enum Position {
            None,
            $($name,)*
        }

        impl Name {
            $(pub const $name: Name = Name::new(Position::$name as usize);)*
        }

        const KNOWN: [&str; Position::None as usize + 1 + [$($text),*].len()] = ["", $($text),*];
    };
}

known_names! {
    // In the order of the keywords of `TokenKind`.
    NULL = "null",
    TRUE = "true",
    FALSE = "false",
    BREAK = "break",
    CASE = "case",
    DEFAULT = "default",
    FOR = "for",
    NEW = "new",
    VAR = "var",
    CONST = "const",
    CONTINUE = "continue",
    FUNCTION = "function",
    RETURN = "return",
    IF = "if",
    THIS = "this",
    DO = "do",
    WHILE = "while",
    SWITCH = "switch",
    WITH = "with",
    THROW = "throw",
    TRY = "try",
    CATCH = "catch",
    FINALLY = "finally",
    DEBUGGER = "debugger",
    ELSE = "else",
    IMPORT = "import",
    EXPORT = "export",
    CLASS = "class",
    EXTENDS = "extends",
    SUPER = "super",
    TYPEOF = "typeof",
    VOID = "void",
    DELETE = "delete",
    INSTANCEOF = "instanceof",
    IN = "in",
    ENUM = "enum",
    LET = "let",
    YIELD = "yield",
    AWAIT = "await",
    // Reserved in strict code.
    IMPLEMENTS = "implements",
    INTERFACE = "interface",
    PACKAGE = "package",
    PRIVATE = "private",
    PROTECTED = "protected",
    PUBLIC = "public",
    STATIC = "static",

    ARGUMENTS = "arguments",
    EVAL = "eval",
    AS = "as",
    ASYNC = "async",
    DEFER = "defer",
    FROM = "from",
    GET = "get",
    META = "meta",
    OF = "of",
    SET = "set",
    TARGET = "target",
    USING = "using",
    CONSTRUCTOR = "constructor",
    PRIVATE_CONSTRUCTOR = "#constructor",
    PROTOTYPE = "prototype",
    PROTO = "__proto__",
    APPLY = "apply",
    CALL = "call",
    CONSTRUCT = "construct",
    EXEC = "exec",
    HAS_OWN_PROPERTY = "hasOwnProperty",
    LENGTH = "length",
    NAME = "name",
    TEST = "test",
    UNDEFINED = "undefined",
    ARRAY = "Array",
    OBJECT = "Object",
    REFLECT = "Reflect",
    DONE = "done",
    NEXT = "next",
    VALUE = "value",
    EMPTY = "",

    // Bindings of the engine. No text of a script is interned as one of these.
    THIS_VALUE = "*this*",
    NEW_TARGET = "*new.target*",
    ACTIVE_FUNCTION = "*function*",
    HOME = "*home*",
    FIELD_INITIALIZER = "*fields*",
    CLASS_HOME = "*class home*",
    CLASS_STATIC_HOME = "*class static home*",
    CLASS_FIELD_INITIALIZER = "*class fields*",
    CLASS_BRAND = "*class brand*",
    DEFAULT_EXPORT = "*default*",
    IMPORT_META = "*import.meta*",
}

/// What `@` makes of a name in the source of a built-in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum NameKind {
    Ordinary,
    /// `@name`
    Private,
    /// `@@name`
    WellKnownSymbol,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Storage {
    Source,
    Latin1,
    Utf16,
}

#[derive(Clone, Copy)]
struct Entry {
    start: u32,
    length: u32,
    /// Past the end of the table if it has none.
    slot: u32,
    storage: Storage,
    kind: NameKind,
}

/// A short text is in `head` and `tail`. Of another, `head` is the hash and `tail` says whether it is wide.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
struct Key {
    head: u64,
    tail: u64,
    /// The length. Of a short text also the kind, and `SHORT`.
    shape: u32,
}

#[derive(Clone, Copy, Default)]
struct Slot {
    head: u64,
    tail: u64,
    shape: u32,
    name: u32,
}

const SHORT: u32 = 1 << 31;
const SHORT_LENGTH: usize = 16;
const INITIAL_SLOTS: usize = 512;
/// A table of up to this size is emptied for the next parse.
const KEPT_SLOTS: usize = 8192;
/// A literal that is longer is not worth looking up.
const LONGEST_INTERNED_LITERAL: usize = 64;

static SHORT_MASKS: [[u64; 2]; SHORT_LENGTH + 1] = {
    let mut masks = [[0; 2]; SHORT_LENGTH + 1];
    let mut length = 1;
    while length <= SHORT_LENGTH {
        let mask = u128::MAX >> (128 - 8 * length);
        masks[length] = [mask as u64, (mask >> 64) as u64];
        length += 1;
    }
    masks
};

#[inline(always)]
fn mix(a: u64, b: u64) -> u64 {
    let product = u128::from(a ^ 0x2d35_8dcc_aa6c_78a5) * u128::from(b ^ 0x8bb8_4b93_962e_acc9);
    product as u64 ^ (product >> 64) as u64
}

#[inline(always)]
fn words(chunk: &[u8; SHORT_LENGTH]) -> (u64, u64) {
    let both = u128::from_le_bytes(*chunk);
    (both as u64, (both >> 64) as u64)
}

impl Key {
    /// Of the first `length` bytes of `chunk`.
    #[inline(always)]
    fn short(chunk: &[u8; SHORT_LENGTH], length: usize, kind: NameKind) -> Key {
        let (head, tail) = words(chunk);
        let [head_mask, tail_mask] = SHORT_MASKS[length];
        Key {
            head: head & head_mask,
            tail: tail & tail_mask,
            shape: SHORT | (kind as u32) << 8 | length as u32,
        }
    }

    fn narrow(characters: &[u8], kind: NameKind) -> Key {
        match characters.last_chunk() {
            Some(last) if characters.len() > SHORT_LENGTH => {
                let step = |hash: u64, chunk| {
                    let (head, tail) = words(chunk);
                    mix(head ^ hash, tail)
                };
                let (chunks, _) = characters.as_chunks();
                Key {
                    head: step(chunks.iter().fold(0, step), last),
                    tail: kind as u64,
                    shape: characters.len() as u32,
                }
            }
            _ => {
                let mut chunk = [0; SHORT_LENGTH];
                chunk[..characters.len()].copy_from_slice(characters);
                Key::short(&chunk, characters.len(), kind)
            }
        }
    }

    fn wide(units: &[u16]) -> Key {
        Key {
            head: mix(u64::from(Text::Utf16(units).hash()), 0),
            tail: u64::MAX,
            shape: units.len() as u32,
        }
    }

    #[inline(always)]
    fn hash(self) -> usize {
        if self.shape & SHORT != 0 {
            mix(self.head, self.tail) as usize
        } else {
            self.head as usize
        }
    }
}

impl Slot {
    #[inline(always)]
    fn key(self) -> Key {
        Key {
            head: self.head,
            tail: self.tail,
            shape: self.shape,
        }
    }
}

pub struct Names {
    slots: Vec<Slot>,
    entries: Vec<Entry>,
    /// How many more names the table takes.
    room: usize,
    latin1: Vec<u8>,
    utf16: Vec<u16>,
}

impl Default for Names {
    fn default() -> Names {
        let mut names = Names::unallocated();
        names.allocate(INITIAL_SLOTS);
        names
    }
}

impl Names {
    /// Takes the place of one that is lent out.
    pub(crate) const fn unallocated() -> Names {
        Names {
            slots: Vec::new(),
            entries: Vec::new(),
            room: 0,
            latin1: Vec::new(),
            utf16: Vec::new(),
        }
    }

    /// The constants go in first, so they are found whichever of the others are taken out. Returns the old table.
    fn allocate(&mut self, size: usize) -> Vec<Slot> {
        let old = core::mem::replace(&mut self.slots, vec![Slot::default(); size]);
        for (index, text) in KNOWN[..Name::THIS_VALUE.index()].iter().enumerate().skip(1) {
            let key = Key::narrow(text.as_bytes(), NameKind::Ordinary);
            self.put(key, index as u32);
        }
        self.room = size / 2 - KNOWN.len();
        old
    }

    fn put(&mut self, key: Key, name: u32) -> usize {
        let mask = self.slots.len() - 1;
        let mut index = key.hash() & mask;
        while self.slots[index].name != 0 {
            index = (index + 1) & mask;
        }
        self.slots[index] = Slot {
            head: key.head,
            tail: key.tail,
            shape: key.shape,
            name,
        };
        index
    }

    pub(crate) fn clear(&mut self) {
        if self.slots.len() > KEPT_SLOTS {
            *self = Names::default();
            return;
        }
        for entry in self.entries.drain(..) {
            if let Some(slot) = self.slots.get_mut(entry.slot as usize) {
                (slot.shape, slot.name) = (0, 0);
            }
        }
        self.room = self.slots.len() / 2 - KNOWN.len();
        crate::clear_buffer(&mut self.entries);
        crate::clear_buffer(&mut self.latin1);
        crate::clear_buffer(&mut self.utf16);
    }

    /// One more than the greatest index of a name.
    #[inline]
    pub fn limit(&self) -> usize {
        KNOWN.len() + self.entries.len()
    }

    #[inline]
    fn entry(&self, name: Name) -> Option<&Entry> {
        self.entries.get(name.index().wrapping_sub(KNOWN.len()))
    }

    /// Without the `@`.
    #[inline]
    pub fn text<'a>(&'a self, name: Name, source: Text<'a>) -> Text<'a> {
        let Some(entry) = self.entry(name) else {
            return Text::Latin1(KNOWN[name.index()].as_bytes());
        };
        let range = entry.start as usize..entry.start as usize + entry.length as usize;
        match entry.storage {
            Storage::Source => source.slice(range),
            Storage::Latin1 => Text::Latin1(&self.latin1[range]),
            Storage::Utf16 => Text::Utf16(&self.utf16[range]),
        }
    }

    #[inline]
    pub fn length(&self, name: Name) -> u32 {
        match self.entry(name) {
            Some(entry) => entry.length,
            None => KNOWN[name.index()].len() as u32,
        }
    }

    #[inline]
    pub fn kind(&self, name: Name) -> NameKind {
        self.entry(name)
            .map_or(NameKind::Ordinary, |entry| entry.kind)
    }

    /// Where the text is in the source, if it is there as it is.
    #[inline]
    pub fn source_start(&self, name: Name) -> Option<u32> {
        self.entry(name)
            .filter(|entry| entry.storage == Storage::Source)
            .map(|entry| entry.start)
    }

    /// The start and the length in the source. For a constant, its index and zero.
    #[inline]
    pub(crate) fn place(&self, name: Name) -> Option<(u32, u32, NameKind)> {
        match self.entry(name) {
            Some(entry) if entry.storage == Storage::Source && entry.length != 0 => {
                Some((entry.start, entry.length, entry.kind))
            }
            Some(_) => None,
            None => Some((name.index() as u32, 0, NameKind::Ordinary)),
        }
    }

    /// A range of the source of up to `SHORT_LENGTH` Latin-1 characters that is not at its very end.
    #[inline(always)]
    pub(crate) fn intern_short(
        &mut self,
        source: Source<'_>,
        start: usize,
        length: usize,
    ) -> Option<Name> {
        if length > SHORT_LENGTH {
            return None;
        }
        let chunk = source.bytes.get(start..)?.first_chunk()?;
        let key = Key::short(chunk, length, NameKind::Ordinary);
        Some(match self.find(key, |_| true) {
            Ok(name) => name,
            Err(slot) => self.insert(key, slot, start, Storage::Source, NameKind::Ordinary),
        })
    }

    /// A range of the source that has only Latin-1 characters.
    pub(crate) fn intern_narrow(
        &mut self,
        source: Source<'_>,
        start: usize,
        end: usize,
        kind: NameKind,
    ) -> Name {
        let characters = &source.bytes[start..end];
        let key = Key::narrow(characters, kind);
        match self.find_narrow(source, key, characters) {
            Ok(name) => name,
            Err(slot) => self.insert(key, slot, start, Storage::Source, kind),
        }
    }

    #[inline]
    pub(crate) fn intern_source(&mut self, source: Source<'_>, start: usize, end: usize) -> Name {
        if let Text::Utf16(units) = source.text
            && !Text::Utf16(&units[start..end]).fits_latin1()
        {
            return self.intern_wide(source, start, &units[start..end]);
        }
        self.intern_short(source, start, end - start)
            .unwrap_or_else(|| self.intern_narrow(source, start, end, NameKind::Ordinary))
    }

    fn intern_wide(&mut self, source: Source<'_>, start: usize, units: &[u16]) -> Name {
        let key = Key::wide(units);
        match self.find(key, |name| {
            self.text(name, source.text) == Text::Utf16(units)
        }) {
            Ok(name) => name,
            Err(slot) => self.insert(key, slot, start, Storage::Source, NameKind::Ordinary),
        }
    }

    /// A text that is not in the source as it is.
    pub(crate) fn intern_text(&mut self, source: Source<'_>, text: Text<'_>) -> Name {
        let (storage, start) = self.store(text);
        let (key, found) = if storage == Storage::Latin1 {
            let characters = &self.latin1[start..];
            let key = Key::narrow(characters, NameKind::Ordinary);
            (key, self.find_narrow(source, key, characters))
        } else {
            let key = Key::wide(&self.utf16[start..]);
            let found = self.find(key, |name| self.text(name, source.text) == text);
            (key, found)
        };
        match found {
            Ok(name) if storage == Storage::Latin1 => {
                self.latin1.truncate(start);
                name
            }
            Ok(name) => {
                self.utf16.truncate(start);
                name
            }
            Err(slot) => self.insert(key, slot, start, storage, NameKind::Ordinary),
        }
    }

    /// The value of a literal, which is not compared with other names.
    #[inline]
    pub(crate) fn literal_in_source(
        &mut self,
        source: Source<'_>,
        start: usize,
        end: usize,
    ) -> Name {
        if end - start <= LONGEST_INTERNED_LITERAL {
            return self.intern_source(source, start, end);
        }
        self.add_entry(
            start,
            end - start,
            usize::MAX,
            Storage::Source,
            NameKind::Ordinary,
        )
    }

    pub(crate) fn literal(&mut self, source: Source<'_>, text: Text<'_>) -> Name {
        if text.len() <= LONGEST_INTERNED_LITERAL {
            return self.intern_text(source, text);
        }
        let (storage, start) = self.store(text);
        self.add_entry(start, text.len(), usize::MAX, storage, NameKind::Ordinary)
    }

    fn store(&mut self, text: Text<'_>) -> (Storage, usize) {
        let (latin1, utf16) = (self.latin1.len(), self.utf16.len());
        match text {
            Text::Latin1(characters) => self.latin1.extend_from_slice(characters),
            Text::Utf16(units) if text.fits_latin1() => {
                self.latin1.extend(units.iter().map(|&unit| unit as u8));
            }
            Text::Utf16(units) => {
                self.utf16.extend_from_slice(units);
                return (Storage::Utf16, utf16);
            }
        }
        (Storage::Latin1, latin1)
    }

    #[inline(always)]
    fn find_narrow(&self, source: Source<'_>, key: Key, characters: &[u8]) -> Result<Name, usize> {
        self.find(key, |name| {
            key.shape & SHORT != 0
                || self.entry(name).is_some_and(|entry| {
                    let storage = match entry.storage {
                        Storage::Source => source.bytes,
                        Storage::Latin1 | Storage::Utf16 => &self.latin1,
                    };
                    storage[entry.start as usize..][..characters.len()] == *characters
                })
        })
    }

    /// The name, or the slot for it.
    #[inline(always)]
    fn find(&self, key: Key, has_same_text: impl Fn(Name) -> bool) -> Result<Name, usize> {
        let mask = self.slots.len().wrapping_sub(1);
        let mut index = key.hash() & mask;
        loop {
            let slot = self.slots[index];
            let Some(name) = Name::from_word(slot.name) else {
                return Err(index);
            };
            if slot.key() == key && has_same_text(name) {
                return Ok(name);
            }
            index = (index + 1) & mask;
        }
    }

    #[inline(never)]
    fn insert(
        &mut self,
        key: Key,
        slot: usize,
        start: usize,
        storage: Storage,
        kind: NameKind,
    ) -> Name {
        let length = if key.shape & SHORT != 0 {
            key.shape & 0xff
        } else {
            key.shape
        };
        let name = self.add_entry(start, length as usize, slot, storage, kind);
        self.slots[slot] = Slot {
            head: key.head,
            tail: key.tail,
            shape: key.shape,
            name: name.index() as u32,
        };
        self.room -= 1;
        if self.room == 0 {
            self.grow();
        }
        name
    }

    #[inline]
    fn add_entry(
        &mut self,
        start: usize,
        length: usize,
        slot: usize,
        storage: Storage,
        kind: NameKind,
    ) -> Name {
        let name = Name::new(self.limit());
        self.entries.push(Entry {
            start: start as u32,
            length: length as u32,
            slot: slot as u32,
            storage,
            kind,
        });
        name
    }

    #[cold]
    fn grow(&mut self) {
        for slot in self.allocate(self.slots.len() * 4) {
            if let Some(index) = (slot.name as usize).checked_sub(KNOWN.len()) {
                self.entries[index].slot = self.put(slot.key(), slot.name) as u32;
                self.room -= 1;
            }
        }
    }
}
