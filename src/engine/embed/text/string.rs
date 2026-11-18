//! Making, sharing and converting strings.

use core::ffi::c_void;
use core::ptr::NonNull;

use bun_engine_text::{
    Atom, CodeUnit, Cursor, FreeExternalBuffer, StringBuilder, StringImpl, StringRef, SymbolRef,
    Text, hash_latin1, hash_utf16, with_text,
};

use super::abi::{RawText, or_abort, room, slice};

#[unsafe(no_mangle)]
unsafe extern "C" fn engine_text_destroy(string: NonNull<StringImpl>) {
    // SAFETY: C++ has given up the last reference.
    unsafe { StringImpl::destroy(string) }
}

#[unsafe(no_mangle)]
extern "C" fn engine_text_compute_hash(string: &StringImpl) -> u32 {
    string.hash()
}

#[unsafe(no_mangle)]
unsafe extern "C" fn engine_text_hash_latin1(characters: *const u8, length: usize) -> u32 {
    // SAFETY: a span.
    hash_latin1(unsafe { slice(characters, length) })
}

#[unsafe(no_mangle)]
unsafe extern "C" fn engine_text_hash_utf16(units: *const u16, length: usize) -> u32 {
    // SAFETY: a span.
    hash_utf16(unsafe { slice(units, length) })
}

#[unsafe(no_mangle)]
extern "C" fn engine_text_hash_ignoring_ascii_case(text: RawText<'_>) -> u32 {
    let text = text.text();
    StringBuilder::on_stack::<256, _>(|folded| {
        with_text!(text, |units| folded.write_latin1(units.len(), |cursor| {
            cursor.push_mapped(units, |unit| unit.to_ascii_lower().to_u16() as u8);
        }));
        folded.text().hash()
    })
}

#[unsafe(no_mangle)]
extern "C" fn engine_text_set_never_atomize(string: &StringImpl) {
    string.set_never_atomize();
}

#[unsafe(no_mangle)]
extern "C" fn engine_text_take_cost(string: &StringImpl) -> usize {
    string.take_cost()
}

#[unsafe(no_mangle)]
extern "C" fn engine_text_shared_cost(string: &StringImpl) -> usize {
    string.shared_cost()
}

#[unsafe(no_mangle)]
unsafe extern "C" fn engine_text_borrow_latin1(characters: *const u8, length: usize) -> StringRef {
    // SAFETY: they outlive the string.
    StringRef::from_static_units(unsafe { slice(characters, length) })
}

#[unsafe(no_mangle)]
unsafe extern "C" fn engine_text_borrow_utf16(units: *const u16, length: usize) -> StringRef {
    // SAFETY: they outlive the string.
    StringRef::from_static_units(unsafe { slice(units, length) })
}

#[unsafe(no_mangle)]
unsafe extern "C" fn engine_text_narrowed(units: *const u16, length: usize) -> StringRef {
    // SAFETY: a span.
    let text = Text::Utf16(unsafe { slice(units, length) });
    or_abort(StringRef::try_narrowed(text))
}

/// # Safety
/// As for `StringRef::external`.
unsafe fn external<T: CodeUnit>(
    units: *const T,
    length: usize,
    hash: u32,
    context: *mut c_void,
    free: Option<FreeExternalBuffer>,
) -> StringRef {
    // SAFETY: the caller's.
    let string = unsafe { StringRef::external(slice(units, length), context, free) };
    match hash {
        0 => string,
        hash => string.with_hash(hash),
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn engine_text_external_latin1(
    characters: *const u8,
    length: usize,
    hash: u32,
    context: *mut c_void,
    free: Option<FreeExternalBuffer>,
) -> StringRef {
    // SAFETY: they stay until `free` is called.
    unsafe { external(characters, length, hash, context, free) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn engine_text_external_utf16(
    units: *const u16,
    length: usize,
    hash: u32,
    context: *mut c_void,
    free: Option<FreeExternalBuffer>,
) -> StringRef {
    // SAFETY: they stay until `free` is called.
    unsafe { external(units, length, hash, context, free) }
}

#[unsafe(no_mangle)]
extern "C" fn engine_text_shared_substring(
    string: &StringImpl,
    start: u32,
    length: u32,
) -> StringRef {
    let start = start as usize;
    StringRef::substring(string, start..start + length as usize)
}

#[unsafe(no_mangle)]
extern "C" fn engine_text_isolated_copy(string: &StringImpl) -> StringRef {
    StringRef::isolated_copy(string).into_string()
}

#[unsafe(no_mangle)]
unsafe extern "C" fn engine_text_atom_of_latin1(characters: *const u8, length: usize) -> Atom {
    // SAFETY: a span.
    Atom::new(Text::Latin1(unsafe { slice(characters, length) }))
}

#[unsafe(no_mangle)]
unsafe extern "C" fn engine_text_atom_of_utf16(units: *const u16, length: usize) -> Atom {
    // SAFETY: a span.
    Atom::new(Text::Utf16(unsafe { slice(units, length) }))
}

#[unsafe(no_mangle)]
unsafe extern "C" fn engine_text_atom_of_literal(characters: *const u8, length: usize) -> Atom {
    // SAFETY: a literal is in static memory.
    Atom::from_static_latin1(unsafe { slice(characters, length) })
}

#[unsafe(no_mangle)]
extern "C" fn engine_text_atom_of_string(string: &StringImpl) -> Atom {
    Atom::from_string(string)
}

#[unsafe(no_mangle)]
extern "C" fn engine_text_find_atom(text: RawText<'_>) -> Option<Atom> {
    Atom::find(text.text())
}

#[unsafe(no_mangle)]
extern "C" fn engine_text_find_atom_of_string(string: &StringImpl) -> Option<Atom> {
    Atom::find_string(string)
}

#[unsafe(no_mangle)]
extern "C" fn engine_text_length_of_longest_atom() -> usize {
    Atom::length_of_longest()
}

#[unsafe(no_mangle)]
extern "C" fn engine_text_symbol(description: &StringImpl) -> SymbolRef {
    SymbolRef::new(description)
}

#[unsafe(no_mangle)]
extern "C" fn engine_text_symbol_without_description() -> SymbolRef {
    SymbolRef::without_description()
}

#[unsafe(no_mangle)]
extern "C" fn engine_text_private_symbol(description: &StringImpl) -> SymbolRef {
    SymbolRef::private(description)
}

#[unsafe(no_mangle)]
extern "C" fn engine_text_registered_symbol(key: &StringImpl) -> SymbolRef {
    SymbolRef::registered(key)
}

/// Null if the bytes are not UTF-8.
#[unsafe(no_mangle)]
unsafe extern "C" fn engine_text_from_utf8(bytes: *const u8, length: usize) -> Option<StringRef> {
    // SAFETY: a span.
    or_abort(StringRef::try_from_utf8(unsafe { slice(bytes, length) }))
}

#[unsafe(no_mangle)]
unsafe extern "C" fn engine_text_from_utf8_lossy(bytes: *const u8, length: usize) -> StringRef {
    // SAFETY: a span.
    let bytes = unsafe { slice(bytes, length) };
    or_abort(StringRef::try_from_utf8_lossy(bytes))
}

#[unsafe(no_mangle)]
extern "C" fn engine_text_utf8_length(text: RawText<'_>) -> usize {
    text.text().utf8_len()
}

/// As many whole characters as fit. Returns how many bytes it has written.
#[unsafe(no_mangle)]
unsafe extern "C" fn engine_text_write_utf8(
    text: RawText<'_>,
    target: *mut u8,
    capacity: usize,
) -> usize {
    // SAFETY: a span.
    let target = unsafe { room(target, capacity) };
    let write = |cursor: &mut Cursor<'_, u8>| _ = text.text().write_utf8(cursor);
    Cursor::fill_uninit(target, write).len()
}

/// This and the next four return null if nothing changes.
#[unsafe(no_mangle)]
extern "C" fn engine_text_to_ascii_lower_case(text: RawText<'_>) -> Option<StringRef> {
    or_abort(text.text().to_ascii_lower_case())
}

#[unsafe(no_mangle)]
extern "C" fn engine_text_to_ascii_upper_case(text: RawText<'_>) -> Option<StringRef> {
    or_abort(text.text().to_ascii_upper_case())
}

#[unsafe(no_mangle)]
extern "C" fn engine_text_to_upper_case(text: RawText<'_>) -> Option<StringRef> {
    or_abort(bun_engine_unicode::to_upper_case(text.text()))
}

#[unsafe(no_mangle)]
extern "C" fn engine_text_to_well_formed(text: RawText<'_>) -> Option<StringRef> {
    or_abort(text.text().to_well_formed())
}

#[unsafe(no_mangle)]
extern "C" fn engine_text_replace_all(
    text: RawText<'_>,
    pattern: RawText<'_>,
    replacement: RawText<'_>,
) -> Option<StringRef> {
    let (text, pattern, replacement) = (text.text(), pattern.text(), replacement.text());
    if pattern.is_empty() {
        return None;
    }
    let mut matches = text.matches(pattern).peekable();
    matches.peek()?;
    StringBuilder::on_stack::<256, _>(|result| {
        let mut copied = 0;
        for start in matches {
            result.push_text(text.slice(copied..start));
            result.push_text(replacement);
            copied = start + pattern.len();
        }
        result.push_text(text.slice_from(copied));
        Some(or_abort(result.take()))
    })
}
