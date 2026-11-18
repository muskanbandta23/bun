//! `String.prototype.normalize`

use bun_engine_text::{StringBuilder, StringRef, Text};
use bun_icu::normalizer::Form;

use crate::latin1::{Accent, split_accent};
use crate::{Error, Result};

option_enum! {
    #[derive(Default)]
    pub enum NormalizationForm: "argument does not match any normalization form" {
        #[default]
        Nfc = "NFC",
        Nfd = "NFD",
        Nfkc = "NFKC",
        Nfkd = "NFKD",
    }
}

/// `None` if `text` is normalized.
pub fn normalize(text: Text<'_>, form: NormalizationForm) -> Result<Option<StringRef>> {
    match text {
        Text::Latin1(_) if form == NormalizationForm::Nfc || text.is_ascii() => Ok(None),
        Text::Latin1(characters) => normalize_latin1(characters, form),
        Text::Utf16(units) => normalize_utf16(units, form),
    }
}

fn normalize_latin1(characters: &[u8], form: NormalizationForm) -> Result<Option<StringRef>> {
    let mut out = StringBuilder::new();
    let mut done = 0;
    for (index, &character) in characters.iter().enumerate() {
        let (canonical, mapping);
        if let Some(pair) = canonical_decomposition(character)
            && form != NormalizationForm::Nfkc
        {
            canonical = pair;
            mapping = &canonical[..];
        } else if let Some(units) = compatibility_decomposition(character)
            && form != NormalizationForm::Nfd
        {
            mapping = units;
        } else {
            continue;
        }
        if done == 0 {
            out.reserve(characters.len() + characters.len() / 8 + mapping.len());
        }
        out.push_latin1_slice(&characters[done..index]);
        out.push_utf16(mapping);
        done = index + 1;
    }
    if done == 0 {
        return Ok(None);
    }
    out.push_latin1_slice(&characters[done..]);
    Ok(Some(out.finish()?))
}

#[inline]
fn canonical_decomposition(character: u8) -> Option<[u16; 2]> {
    let (base, accent) = split_accent(character)?;
    let mark = match accent {
        Accent::Grave => 0x300,
        Accent::Acute => 0x301,
        Accent::Circumflex => 0x302,
        Accent::Tilde => 0x303,
        Accent::Diaeresis => 0x308,
        Accent::Ring => 0x30a,
        Accent::Cedilla => 0x327,
        Accent::Stroke => return None,
    };
    Some([base.into(), mark])
}

#[inline]
fn compatibility_decomposition(character: u8) -> Option<&'static [u16]> {
    Some(match character {
        0xa0 => &[0x20],
        0xa8 => &[0x20, 0x308],
        0xaa => &[0x61],
        0xaf => &[0x20, 0x304],
        0xb2 => &[0x32],
        0xb3 => &[0x33],
        0xb4 => &[0x20, 0x301],
        0xb5 => &[0x3bc],
        0xb8 => &[0x20, 0x327],
        0xb9 => &[0x31],
        0xba => &[0x6f],
        0xbc => &[0x31, 0x2044, 0x34],
        0xbd => &[0x31, 0x2044, 0x32],
        0xbe => &[0x33, 0x2044, 0x34],
        _ => return None,
    })
}

fn normalize_utf16(units: &[u16], form: NormalizationForm) -> Result<Option<StringRef>> {
    let (form, first_affected) = match form {
        NormalizationForm::Nfc => (Form::Nfc, 0x300),
        NormalizationForm::Nfd => (Form::Nfd, 0xc0),
        NormalizationForm::Nfkc => (Form::Nfkc, 0xa0),
        NormalizationForm::Nfkd => (Form::Nfkd, 0xa0),
    };
    let unaffected = count_below(units, first_affected);
    if unaffected == units.len() {
        return Ok(None);
    }
    // The last of them can combine with what follows.
    let mut unchanged = unaffected.saturating_sub(1);
    unchanged += (form.normalized_prefix(&units[unchanged..])).map_err(|_| Error::OutOfMemory)?;

    let mut out: Option<StringBuilder> = None;
    form.normalize_pieces(&units[unchanged..], |piece, normalized| match &mut out {
        Some(out) => out.push_utf16(normalized),
        None if piece == normalized => unchanged += piece.len(),
        None => {
            let out = out.insert(StringBuilder::with_capacity(units.len()));
            out.push_utf16(&units[..unchanged]);
            out.push_utf16(normalized);
        }
    })
    .map_err(|_| Error::OutOfMemory)?;
    Ok(out.map(StringBuilder::finish).transpose()?)
}

/// How many units come before the first that is `limit` or more.
fn count_below(units: &[u16], limit: u16) -> usize {
    const CHUNK_LENGTH: usize = 16;

    let (chunks, _) = units.as_chunks::<CHUNK_LENGTH>();
    let is_below = |chunk: &&[u16; CHUNK_LENGTH]| chunk.iter().fold(0, |a, &b| b.max(a)) < limit;
    let skipped = chunks.iter().take_while(is_below).count() * CHUNK_LENGTH;
    let rest = units[skipped..].iter();
    skipped + rest.take_while(|&&unit| unit < limit).count()
}
