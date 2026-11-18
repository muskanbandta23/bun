mod support;

use std::cell::Cell;
use std::rc::Rc;

use bun_engine_asm::Features;
use bun_engine_regexp::{Context, Mode, Outcome};
use bun_engine_regexp_jit::{Options, compile};
use bun_engine_text::Text;
use support::{in_each_width, natives, program};

include!("../../regexp/tests/support/cases.rs");

fn assert_same_as_interpreter(source: &str, letters: &str, subject: Text<'_>, starts: &[u32]) {
    let mut context = Context::new();
    for mode in [Mode::MatchOnly, Mode::Captures] {
        let program = program(source, letters, subject, mode);
        let entries = match mode {
            Mode::MatchOnly => 0,
            Mode::Captures => program.capture_entries(),
        };
        for native in natives(&program) {
            for &start in starts {
                let (mut expected, mut found) = (vec![7; entries], vec![8; entries]);
                let outcome = program.exec(subject, start, &mut expected, &mut context);
                assert_eq!(
                    native.exec(subject, start, &mut found, &mut context),
                    outcome,
                    "/{source}/{letters} on {subject:?} from {start}"
                );
                if outcome != Outcome::NoMatch {
                    assert_eq!(
                        found, expected,
                        "/{source}/{letters} on {subject:?} from {start}"
                    );
                }
            }
        }
    }
}

#[test]
fn known_answers() {
    for &(source, letters, subject, start, expected) in CASES {
        in_each_width(subject, |subject| {
            let program = program(source, letters, subject, Mode::MatchOnly);
            let outcome = program.exec(subject, start, &mut [], &mut Context::new());
            assert_eq!(outcome == Outcome::NoMatch, expected.is_empty());
            let starts: Vec<u32> = (0..subject.len() as u32 + 2).collect();
            assert_same_as_interpreter(source, letters, subject, &starts);
        });
    }
}

#[test]
fn every_unit_against_classes() {
    let units: Vec<u16> = (0..=u16::MAX).collect();
    let starts: Vec<u32> = (0..0x400)
        .chain((0x400..0x10000).step_by(7))
        .chain(0x2000..0x2060)
        .collect();
    let characters: Vec<u8> = (0..=u8::MAX).collect();
    for class in [
        r"\d",
        r"\D",
        r"\w",
        r"\W",
        r"\s",
        r"\S",
        ".",
        "[a-zA-Z]",
        "[^a-zA-Z]",
        "[+*/%&-]",
        r"[^\\\x22]",
        r"[\0-\x1f]",
        r"[\x80-￿]",
        "[a-fxz\u{e9}\u{3b1}-\u{3c9}]",
        r"\p{L}",
        r"\P{L}",
        r"[\p{L}_]",
        r"[\p{Lu}\p{Nd}]",
    ] {
        assert_same_as_interpreter(class, "uy", Text::Utf16(&units), &starts);
        assert_same_as_interpreter(class, "uy", Text::Latin1(&characters), &starts[..0x100]);
    }
}

#[test]
fn more_cells_than_registers() {
    for count in [10, 200] {
        let source = format!(
            "(?:{}x|y|{})*z",
            "(a*)ab".repeat(count),
            "aab".repeat(count)
        );
        let subject = format!("y{0}x{0}{0}x{0}z", "aab".repeat(count));
        in_each_width(&subject, |subject| {
            assert_same_as_interpreter(&source, "", subject, &[0, 1]);
        });
    }
}

#[test]
fn the_stack_grows_up_to_a_limit() {
    let subject = [b'a'; 50_000];
    let subject = Text::Latin1(&subject);
    let program = program("^(?:a|ab)*bc", "", subject, Mode::MatchOnly);
    for native in natives(&program) {
        let mut context = Context::new();
        assert_eq!(
            native.exec(subject, 0, &mut [], &mut context),
            Outcome::NoMatch
        );
        context.set_max_stack_size(4096);
        assert_eq!(
            native.exec(subject, 0, &mut [], &mut context),
            Outcome::OutOfMemory
        );
    }
}

#[test]
fn a_long_match_is_interrupted() {
    let subject = [b'a'; 64];
    let subject = Text::Latin1(&subject);
    let program = program(r"^(a|aa)+\1b", "", subject, Mode::MatchOnly);
    for native in natives(&program) {
        let polls = Rc::new(Cell::new(0));
        let seen = Rc::clone(&polls);
        let mut context = Context::new();
        context.set_poll(Box::new(move || {
            seen.set(seen.get() + 1);
            seen.get() == 3
        }));
        let outcome = native.exec(subject, 0, &mut [], &mut context);
        assert_eq!((outcome, polls.get()), (Outcome::Interrupted, 3));
    }
}

#[test]
fn the_size_of_the_code_has_a_limit() {
    let program = program(&"a*b".repeat(100), "", Text::EMPTY, Mode::MatchOnly);
    let options = Options {
        features: Features::host(),
        seed: 1,
        max_code_size: 1000,
    };
    assert!(compile(program.verify().unwrap(), &options).is_err());
}
