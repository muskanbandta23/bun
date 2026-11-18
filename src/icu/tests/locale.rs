use bun_engine_standalone as _;
use bun_icu::{CText, locale};

#[test]
fn language_tags() {
    let id = locale::for_language_tag(c"sr-Latn-RS-u-ca-gregory-nu-latn").unwrap();
    assert_eq!(&*id, c"sr_Latn_RS@calendar=gregorian;numbers=latn");
    assert_eq!(
        &*locale::to_language_tag(&id).unwrap(),
        b"sr-Latn-RS-u-ca-gregory-nu-latn"
    );
    assert!(locale::for_language_tag(c"en-*").is_err());

    let mut keywords = locale::keywords(&id).unwrap().unwrap();
    assert_eq!(keywords.next().unwrap(), Some(&b"calendar"[..]));
    assert_eq!(keywords.next().unwrap(), Some(&b"numbers"[..]));
    assert_eq!(keywords.next().unwrap(), None);
    assert!(locale::keywords(c"en").unwrap().is_none());
}

#[test]
fn identifiers_end_at_their_only_zero() {
    assert_eq!(&*CText::new(b"en").unwrap(), c"en");
    assert!(CText::new(b"e\0n").is_err());
}
