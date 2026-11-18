#![cfg(unix)]
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use std::env;
use std::process::Command;

use bun_engine_standalone as _;
use bun_icu::collator::Collator;
use bun_icu::{locale, process};

const EXPECTED: &str = "BUN_ICU_TEST_DEFAULT_LOCALE";

/// ICU reads the environment once, so each case is a process.
#[test]
fn default_locale() {
    if let Ok(expected) = env::var(EXPECTED) {
        // SAFETY: this is the only test of the program, and these are its first calls.
        unsafe {
            process::use_default_allocator();
            process::choose_default_locale();
        }
        assert_eq!(locale::default().to_str(), Ok(&*expected));
        Collator::open(c"zz").unwrap();
        return;
    }
    for (language, expected) in [
        ("", ""),
        ("C.UTF-8", "en_US_POSIX"),
        ("de_AT.UTF-8", "de_AT"),
        ("de_DE@euro", "de_DE_EURO"),
        ("ks_IN@devanagari", "ks_IN"),
        ("fr_FR@a=b", "fr_FR"),
        ("fran\u{e7}ais", "en_US"),
        ("x", "en_US"),
    ] {
        let status = Command::new(env::current_exe().unwrap())
            .env("LC_ALL", language)
            .env(EXPECTED, expected)
            .status();
        assert!(status.unwrap().success(), "{language}");
    }
}
