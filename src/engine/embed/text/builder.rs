//! What `StringBuilder.h` does not do in place.

use bun_engine_text::{StringBuilder, StringRef};

use super::abi::{RawText, slice};

const _: () = assert!(size_of::<StringBuilder>() == 56);

#[unsafe(no_mangle)]
extern "C" fn engine_text_builder_clear(builder: &mut StringBuilder) {
    builder.clear();
}

#[unsafe(no_mangle)]
extern "C" fn engine_text_builder_overflow(builder: &mut StringBuilder) {
    builder.overflow();
}

#[unsafe(no_mangle)]
extern "C" fn engine_text_builder_reserve(builder: &mut StringBuilder, additional: usize) {
    builder.reserve(additional);
}

#[unsafe(no_mangle)]
extern "C" fn engine_text_builder_grow(builder: &mut StringBuilder, additional: usize) {
    builder.grow(additional);
}

#[unsafe(no_mangle)]
extern "C" fn engine_text_builder_widen(builder: &mut StringBuilder, additional: usize) {
    builder.widen(additional);
}

#[unsafe(no_mangle)]
extern "C" fn engine_text_builder_push_json_quoted(builder: &mut StringBuilder, text: RawText<'_>) {
    builder.push_json_quoted(text.text());
}

#[unsafe(no_mangle)]
unsafe extern "C" fn engine_text_builder_push_utf8(
    builder: &mut StringBuilder,
    bytes: *const u8,
    length: usize,
) {
    // SAFETY: a span.
    builder.push_utf8_lossy(unsafe { slice(bytes, length) });
}

/// Null if there is no memory.
#[unsafe(no_mangle)]
extern "C" fn engine_text_builder_take(builder: &mut StringBuilder) -> Option<StringRef> {
    builder.take().ok()
}
