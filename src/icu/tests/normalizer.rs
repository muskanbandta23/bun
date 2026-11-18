use bun_engine_standalone as _;
use bun_icu::Buffer;
use bun_icu::normalizer::Form;

fn normalize(form: Form, text: &str) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut out = Buffer::new();
    form.normalize(&units, &mut out).unwrap();
    assert!(form.is_normalized(&out).unwrap());
    String::from_utf16(&out).unwrap()
}

#[test]
fn each_form() {
    let text = "\u{1e9b}\u{323}";
    assert_eq!(normalize(Form::Nfc, text), "\u{1e9b}\u{323}");
    assert_eq!(normalize(Form::Nfd, text), "\u{17f}\u{323}\u{307}");
    assert_eq!(normalize(Form::Nfkc, text), "\u{1e69}");
    assert_eq!(normalize(Form::Nfkd, text), "s\u{323}\u{307}");
}

#[test]
fn results_of_any_length() {
    for count in [0, 1, 15, 16, 17, 1000] {
        assert_eq!(
            normalize(Form::Nfd, &"\u{e9}".repeat(count)),
            "e\u{301}".repeat(count)
        );
    }
}
