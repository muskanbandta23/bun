use bun_engine_text::{Progress, StringBuilder, StringRef};

use crate::support::*;

fn decoded(bytes: &[u8]) -> Vec<u16> {
    let mut units = Vec::new();
    for chunk in bytes.utf8_chunks() {
        units.extend(chunk.valid().encode_utf16());
        units.extend((!chunk.invalid().is_empty()).then_some(0xfffd));
    }
    units
}

fn check_decoding(bytes: &[u8]) {
    let expected = decoded(bytes);
    let lossy = StringRef::try_from_utf8_lossy(bytes).unwrap();
    assert_eq!(units_of(lossy.text()), expected, "{bytes:x?}");
    assert_eq!(lossy.is_latin1(), lossy.text().fits_latin1());
    let strict = StringRef::try_from_utf8(bytes).unwrap();
    assert_eq!(
        strict.is_some(),
        str::from_utf8(bytes).is_ok(),
        "{bytes:x?}"
    );
    assert!(strict.is_none_or(|it| it == lossy));
    let mut builder = StringBuilder::new();
    builder.push_unit(if bytes.len() % 2 == 0 { 0x61 } else { 0x100 });
    builder.push_utf8_lossy(bytes);
    assert!(builder.text().slice_from(1) == lossy.text());
}

fn characters(units: &[u16]) -> impl Iterator<Item = (char, usize)> {
    char::decode_utf16(units.iter().copied())
        .map(|it| it.map_or(('\u{fffd}', 1), |it| (it, it.len_utf16())))
}

fn check_encoding(units: &[u16], rooms: &[usize]) {
    let expected: String = characters(units).map(|it| it.0).collect();
    let is_well_formed = char::decode_utf16(units.iter().copied()).all(|it| it.is_ok());
    each_width(units, |text| {
        assert_eq!(text.utf8_len(), expected.len());
        assert_eq!(text.try_to_utf8().unwrap(), expected.as_bytes());
        assert_eq!(text.is_well_formed(), is_well_formed);
        let repaired = text.to_well_formed().unwrap();
        assert_eq!(repaired.is_none(), is_well_formed);
        assert!(repaired.is_none_or(|it| units_of(it.text()) == wide(&expected)));
        for &room in rooms {
            let (mut read, mut written) = (0, 0);
            for (character, width) in characters(units) {
                if written + character.len_utf8() > room {
                    break;
                }
                (read, written) = (read + width, written + character.len_utf8());
            }
            let mut target = vec![0xfe; room];
            assert_eq!(
                text.encode_utf8_into(&mut target),
                Progress { read, written }
            );
            assert_eq!(target[..written], expected.as_bytes()[..written]);
            assert!(target[written..].iter().all(|&byte| byte > 0xfd));
        }
    });
}

#[test]
fn every_class_of_sequence() {
    let classes: &[u8] = if cfg!(miri) {
        &[0x41, 0x80, 0xc2, 0xed, 0xf4]
    } else {
        &[
            0x00, 0x7f, 0x80, 0x8f, 0x90, 0x9f, 0xa0, 0xbf, 0xc0, 0xc1, 0xc2, 0xc3, 0xc4, 0xdf,
            0xe0, 0xe1, 0xec, 0xed, 0xee, 0xef, 0xf0, 0xf1, 0xf3, 0xf4, 0xf5, 0xff,
        ]
    };
    let count = classes.len();
    for index in 0..count.pow(4) {
        let sequence = [1, count, count.pow(2), count.pow(3)].map(|it| classes[index / it % count]);
        check_decoding(&sequence[..4 - usize::from(index < count.pow(3))]);
        if index % 61 == 0 {
            check_decoding(
                &[
                    "é".repeat(30 + index % 7).as_bytes(),
                    &sequence,
                    &[b'a'; 70],
                ]
                .concat(),
            );
        }
    }
}

#[test]
fn long_texts_in_several_scripts() {
    let mut random = Random::new();
    let scripts =
        ["ab", "aé", "éÿ", "яa", "漢字", "a😀", "aé漢😀"].map(|it| it.chars().collect::<Vec<_>>());
    let long: &[usize] = if cfg!(miri) {
        &[700]
    } else {
        &[1500, 2500, 4000, 20_000]
    };
    for &length in lengths().iter().chain(long) {
        for script in &scripts {
            let text: String = (0..length).map(|_| random.pick(script)).collect();
            check_decoding(text.as_bytes());
            let rooms = [
                0,
                text.len() / 2,
                text.len().saturating_sub(1),
                text.len(),
                3 * text.len(),
            ];
            check_encoding(&wide(&text), &rooms);

            let mut bytes = text.clone().into_bytes();
            let mut units = wide(&text);
            for _ in 0..[1, length / 40, length][length % 3].min(length) {
                let at = random.below(bytes.len());
                bytes[at] = random.pick(&[0x80, 0xc3, 0xe2, 0xf0, 0xff]);
                let at = random.below(units.len());
                units[at] = random.pick(&[0xd800, 0xdbff, 0xdc00, 0xdfff]);
            }
            check_decoding(&bytes);
            check_encoding(&units, &rooms);
        }
    }
}

#[test]
fn one_lone_surrogate_anywhere() {
    for length in lengths().into_iter().filter(|&length| length < 200) {
        for at in indices(length) {
            let mut units = vec![0x61; length];
            units[at] = if (at + length) % 2 == 0 {
                0xd800
            } else {
                0xdc00
            };
            check_encoding(&units, &[at + 2, at + 3]);
        }
    }
}
