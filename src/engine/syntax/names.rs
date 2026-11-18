//! The names and strings of one parse, each as a small number.

use core::num::NonZeroU32;

use bun_engine_text::{CodeUnit, Text, with_text};

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

const KNOWN_SLOT_BITS: u32 = 10;
const LONGEST_KNOWN: usize = 14;

#[inline(always)]
const fn known_slot(first: u32, second: u32, last: u32, length: u32, multiplier: u32) -> usize {
    let key = first | second << 8 | last << 16 | length << 24;
    (key.wrapping_mul(multiplier) >> (32 - KNOWN_SLOT_BITS)) as usize
}

/// A multiplier that gives each name a slot of its own, and the slots.
const KNOWN_TABLE: (u32, [u8; 1 << KNOWN_SLOT_BITS]) = {
    let mut multiplier = 0x9e37_79b1u32;
    'search: loop {
        let mut slots = [0; 1 << KNOWN_SLOT_BITS];
        let mut index = 1;
        while index < Name::EMPTY.index() {
            let text = KNOWN[index].as_bytes();
            assert!(text.len() >= 2 && text.len() <= LONGEST_KNOWN);
            let slot = known_slot(
                text[0] as u32,
                text[1] as u32,
                text[text.len() - 1] as u32,
                text.len() as u32,
                multiplier,
            );
            if slots[slot] != 0 {
                multiplier = multiplier.wrapping_add(0x0123_4568);
                continue 'search;
            }
            slots[slot] = index as u8;
            index += 1;
        }
        break (multiplier, slots);
    }
};
static KNOWN_SLOTS: [u8; 1 << KNOWN_SLOT_BITS] = KNOWN_TABLE.1;

/// The constant of [`Name`] with this text, but for the empty one.
#[inline(always)]
pub(crate) fn find_known<C: CodeUnit>(units: &[C]) -> Option<Name> {
    let &[first, second, ..] = units else {
        return None;
    };
    if units.len() > LONGEST_KNOWN {
        return None;
    }
    let slot = known_slot(
        first.to_u32(),
        second.to_u32(),
        units[units.len() - 1].to_u32(),
        units.len() as u32,
        KNOWN_TABLE.0,
    );
    let name = Name::from_word(u32::from(KNOWN_SLOTS[slot]))?;
    (C::text(units) == Text::Latin1(KNOWN[name.index()].as_bytes())).then_some(name)
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
    storage: Storage,
    kind: NameKind,
}

/// For a text of up to eight Latin-1 characters the key is the text itself.
#[derive(Clone, Copy, Default)]
struct Slot {
    key: u64,
    /// The length of such a text, and the kind.
    shape: u32,
    name: u32,
}

const INLINE_LENGTH: usize = 8;
const INITIAL_SLOTS: usize = 128;

pub struct Names {
    entries: Vec<Entry>,
    slots: Vec<Slot>,
    latin1: Vec<u8>,
    utf16: Vec<u16>,
}

impl Default for Names {
    fn default() -> Names {
        Names {
            entries: Vec::new(),
            slots: vec![Slot::default(); INITIAL_SLOTS],
            latin1: Vec::new(),
            utf16: Vec::new(),
        }
    }
}

#[inline(always)]
fn mix(a: u64, b: u64) -> u64 {
    let product = u128::from(a) * u128::from(b);
    product as u64 ^ (product >> 64) as u64
}

/// The key of a text, and whether texts of one length with this key are equal.
#[inline(always)]
fn key_of(text: Text<'_>) -> (u64, bool) {
    match text {
        Text::Latin1(characters) => {
            if let (Some(head), Some(tail)) = (characters.first_chunk(), characters.last_chunk()) {
                let (head, tail) = (u64::from_le_bytes(*head), u64::from_le_bytes(*tail));
                match characters.len() {
                    INLINE_LENGTH => (head, true),
                    length @ ..=16 => (
                        mix(head ^ 0x2d35_8dcc_aa6c_78a5, tail ^ 0x8bb8_4b93_962e_acc9)
                            ^ length as u64,
                        false,
                    ),
                    _ => (u64::from(text.hash()), false),
                }
            } else {
                (packed(characters), true)
            }
        }
        Text::Utf16(units) => {
            if units.len() <= INLINE_LENGTH && text.fits_latin1() {
                (packed(units), true)
            } else {
                (u64::from(text.hash()), false)
            }
        }
    }
}

#[inline(always)]
fn packed<C: CodeUnit>(units: &[C]) -> u64 {
    units
        .iter()
        .rfold(0, |key, unit| key << 8 | u64::from(unit.to_u16()))
}

impl Names {
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.latin1.clear();
        self.utf16.clear();
        self.slots.truncate(INITIAL_SLOTS);
        self.slots.fill(Slot::default());
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

    /// `source[start..end]`, which [`find_known`] does not find.
    #[inline(always)]
    pub(crate) fn intern_unknown<C: CodeUnit>(
        &mut self,
        source: &[C],
        start: u32,
        end: u32,
        kind: NameKind,
    ) -> Name {
        let text = C::text(&source[start as usize..end as usize]);
        match self.find(C::text(source), text, kind) {
            Ok(name) => name,
            Err(vacant) => self.insert(vacant, start, Storage::Source, kind),
        }
    }

    pub(crate) fn intern_source<C: CodeUnit>(
        &mut self,
        source: &[C],
        start: u32,
        end: u32,
    ) -> Name {
        if start == end {
            return Name::EMPTY;
        }
        find_known(&source[start as usize..end as usize])
            .unwrap_or_else(|| self.intern_unknown(source, start, end, NameKind::Ordinary))
    }

    /// A text that is not in the source as it is.
    pub(crate) fn intern_text<C: CodeUnit>(&mut self, source: &[C], text: Text<'_>) -> Name {
        if text.is_empty() {
            return Name::EMPTY;
        }
        if let Some(name) = with_text!(text, |units| find_known(units)) {
            return name;
        }
        // Equal texts get equal keys only if they have the same width: that of the source, where it will do.
        let (latin1, utf16) = (self.latin1.len(), self.utf16.len());
        let (storage, start) = if C::IS_LATIN1 && text.fits_latin1() {
            self.latin1.extend(text.code_units().map(|unit| unit as u8));
            (Storage::Latin1, latin1)
        } else {
            self.utf16.extend(text.code_units());
            (Storage::Utf16, utf16)
        };
        let copy = match storage {
            Storage::Utf16 => Text::Utf16(&self.utf16[start..]),
            _ => Text::Latin1(&self.latin1[start..]),
        };
        match self.find(C::text(source), copy, NameKind::Ordinary) {
            Ok(name) => {
                self.latin1.truncate(latin1);
                self.utf16.truncate(utf16);
                name
            }
            Err(vacant) => self.insert(vacant, start as u32, storage, NameKind::Ordinary),
        }
    }

    #[inline(always)]
    fn first_slot(&self, key: u64) -> usize {
        mix(key, 0x9e37_79b9_7f4a_7c15) as usize & (self.slots.len() - 1)
    }

    #[inline(always)]
    fn find(&self, source: Text<'_>, text: Text<'_>, kind: NameKind) -> Result<Name, Vacant> {
        let (key, is_exact) = key_of(text);
        let length = text.len() as u32;
        let shape = if is_exact { length + 1 } else { 0 } | (kind as u32) << 4;
        let mut index = self.first_slot(key);
        loop {
            let slot = &self.slots[index];
            let Some(name) = Name::from_word(slot.name) else {
                return Err(Vacant {
                    index,
                    key,
                    shape,
                    length,
                });
            };
            if slot.key == key
                && slot.shape == shape
                && (is_exact || self.text(name, source) == text)
            {
                return Ok(name);
            }
            index = (index + 1) & (self.slots.len() - 1);
        }
    }

    fn insert(&mut self, vacant: Vacant, start: u32, storage: Storage, kind: NameKind) -> Name {
        let name = Name::new(self.limit());
        self.entries.push(Entry {
            start,
            length: vacant.length,
            storage,
            kind,
        });
        self.slots[vacant.index] = Slot {
            key: vacant.key,
            shape: vacant.shape,
            name: name.index() as u32,
        };
        if self.entries.len() * 2 > self.slots.len() {
            self.grow();
        }
        name
    }

    #[cold]
    fn grow(&mut self) {
        let grown = vec![Slot::default(); self.slots.len() * 4];
        let old = core::mem::replace(&mut self.slots, grown);
        for slot in old.into_iter().filter(|slot| slot.name != 0) {
            let mut index = self.first_slot(slot.key);
            while self.slots[index].name != 0 {
                index = (index + 1) & (self.slots.len() - 1);
            }
            self.slots[index] = slot;
        }
    }
}

struct Vacant {
    index: usize,
    key: u64,
    shape: u32,
    length: u32,
}
