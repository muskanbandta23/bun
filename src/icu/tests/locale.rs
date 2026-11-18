use bun_engine_standalone as _;
use bun_icu::{CText, locale};

#[test]
fn language_tags() {
    assert_eq!(
        &*locale::to_language_tag(c"sr_Latn_RS@calendar=gregorian;numbers=latn").unwrap(),
        b"sr-Latn-RS-u-ca-gregory-nu-latn"
    );
}

#[test]
fn identifiers_end_at_their_only_zero() {
    assert_eq!(&*CText::new(b"en").unwrap(), c"en");
    assert!(CText::new(b"e\0n").is_err());
}
