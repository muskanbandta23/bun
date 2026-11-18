//! UTS #46 as the URL Standard and `node:url` use it.

use bun_engine_text::{CodeUnit, StringRef, StringWriter, Text, with_text};
use bun_icu::idna::{self, Name, Verdict};

use crate::buffer::Buffer;
use crate::punycode::{self, MAX_LABEL_LENGTH};
use crate::{Error, Punycode, Result, Url};

/// The most code units that are converted. DNS resolves names of up to 253 bytes.
const MAX_NAME_LENGTH: usize = 4096;

type Conversion = fn(&[u16], &mut Name) -> bun_icu::Result<Verdict>;

fn convert(conversion: Conversion, name: &[u16], out: &mut Name) -> Result<Verdict> {
    if name.len() > MAX_NAME_LENGTH {
        return Err(Error::OutOfMemory);
    }
    conversion(name, out).map_err(|error| match error {
        bun_icu::Error::MEMORY_ALLOCATION => Error::OutOfMemory,
        _ => Error::Invalid,
    })
}

fn convert_text(conversion: Conversion, name: Text<'_>, out: &mut Name) -> Result<Verdict> {
    match name {
        Text::Latin1(characters) => {
            let units = characters.iter().take(MAX_NAME_LENGTH + 1);
            let units: Name = units.map(|&character| u16::from(character)).collect();
            convert(conversion, &units, out)
        }
        Text::Utf16(units) => convert(conversion, units, out),
    }
}

/// Domain parser ToASCII, from UTF-8.
pub(crate) fn encode_in_place(domain: &mut Buffer) -> Result<()> {
    let text = core::str::from_utf8(domain).map_err(|_| Error::Invalid)?;
    let units: Name = text.encode_utf16().take(MAX_NAME_LENGTH + 1).collect();
    let mut ascii = Name::new();
    if convert(idna::to_ascii, &units, &mut ascii) != Ok(Verdict::Valid) {
        return Err(Error::Invalid);
    }
    domain.truncate(0);
    for &unit in &ascii {
        domain.push(unit as u8)?;
    }
    Ok(())
}

fn ace_labels(domain: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = Some(domain);
    let labels = core::iter::from_fn(move || {
        let text = rest?;
        let dot = bun_highway::index_of_char(text, b'.');
        rest = dot.map(|dot| &text[dot + 1..]);
        Some(&text[..dot.unwrap_or(text.len())])
    });
    labels.filter(|label| label.starts_with(b"xn--"))
}

/// The validity criteria, where the label alone and a table or two decide them.
fn judge_ace_label(label: &[u8]) -> Option<Verdict> {
    if label.len() > MAX_LABEL_LENGTH {
        return None;
    }
    let mut storage = ['\0'; MAX_LABEL_LENGTH];
    let characters = match punycode::decode(&label["xn--".len()..], &mut storage) {
        Some([] | ['x', 'n', '-', '-', ..]) | None => return Some(Verdict::Invalid),
        Some(characters) => characters,
    };
    if label.ends_with(b"-") || idna::is_mark(characters[0]) || characters.contains(&'\u{fffd}') {
        return Some(Verdict::Invalid);
    }
    let mut units = [0; 2 * MAX_LABEL_LENGTH];
    let mut length = 0;
    for character in characters {
        length += character.encode_utf16(&mut units[length..]).len();
    }
    if !idna::is_mapped(&units[..length]).ok()? {
        return Some(Verdict::Invalid);
    }
    let needs_context = |&character: &char| {
        !character.is_ascii()
            && (matches!(character, '\u{200c}' | '\u{200d}') || idna::is_right_to_left(character))
    };
    (!characters.iter().any(needs_context)).then_some(Verdict::Valid)
}

/// Whether ToASCII records no error for a lowercase ASCII domain.
pub(crate) fn has_valid_ace_labels(domain: &[u8]) -> bool {
    let mut is_decided = true;
    for label in ace_labels(domain) {
        match judge_ace_label(label) {
            Some(Verdict::Valid) => {}
            Some(Verdict::Invalid) => return false,
            None => is_decided = false,
        }
    }
    is_decided
        || convert_text(idna::to_ascii, Text::Latin1(domain), &mut Name::new())
            == Ok(Verdict::Valid)
}

/// `name` in lowercase, if that is all there is to do in either direction.
fn lowercase_if_plain(name: Text<'_>) -> Result<Option<StringRef>> {
    if !name.is_ascii() {
        return Ok(None);
    }
    let mut writer = StringWriter::<u8>::try_new(name.len())?;
    with_text!(name, |units| writer
        .push_mapped(units, |unit| (unit.to_u16() as u8)
            .to_ascii_lowercase()));
    let lowercase = writer.finish();
    let has_ace_label =
        u8::units(lowercase.text()).is_some_and(|it| ace_labels(it).next().is_some());
    Ok((!has_ace_label).then_some(lowercase))
}

/// What ToASCII does about the errors that it records.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OnError {
    Fail,
    Ignore,
}

/// ToASCII of a name that need not be a host.
pub fn name_to_ascii(name: Text<'_>, on_error: OnError) -> Result<StringRef> {
    if let Some(lowercase) = lowercase_if_plain(name)? {
        return Ok(lowercase);
    }
    let mut out = Name::new();
    if convert_text(idna::to_ascii, name, &mut out)? == Verdict::Invalid
        && on_error == OnError::Fail
    {
        return Err(Error::Invalid);
    }
    Ok(StringRef::try_narrowed(Text::Utf16(&out))?)
}

/// ToUnicode, whatever errors it records.
pub fn name_to_unicode(name: Text<'_>) -> Result<StringRef> {
    if let Some(lowercase) = lowercase_if_plain(name)? {
        return Ok(lowercase);
    }
    let mut out = Name::new();
    convert_text(idna::to_unicode, name, &mut out)?;
    Ok(StringRef::try_narrowed(Text::Utf16(&out))?)
}

/// The `hostname` that a URL of a special scheme has once `domain` is assigned to it.
pub fn domain_to_ascii(domain: Text<'_>) -> Result<StringRef> {
    let mut url = Url::parse(Text::from_ascii("ws://x/"), None, Punycode::Unchecked)?;
    url.set_hostname(domain, Punycode::Checked)?;
    Ok(StringRef::try_from_units(url.hostname())?)
}

/// ToUnicode of [`domain_to_ascii`].
pub fn domain_to_unicode(domain: Text<'_>) -> Result<StringRef> {
    name_to_unicode(domain_to_ascii(domain)?.text())
}
