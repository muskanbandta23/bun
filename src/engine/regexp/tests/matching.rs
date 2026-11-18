mod support;

use std::cell::Cell;
use std::rc::Rc;

use bun_engine_regexp::program::UNSET;
use bun_engine_regexp::{Context, Error, Mode, Options, Outcome, Pattern, Program, Width, compile};
use bun_engine_text::Text;
use support::{flags, in_each_width};

fn program(source: &str, letters: &str, subject: Text<'_>, mode: Mode) -> Program {
    let units: Vec<u16> = source.encode_utf16().collect();
    let pattern = Pattern::new(Text::Utf16(&units), flags(letters)).unwrap();
    let options = Options {
        width: if subject.is_latin1() {
            Width::Latin1
        } else {
            Width::Utf16
        },
        mode,
        sample: subject,
        stack_limit: 0,
    };
    let program = compile(pattern.tree(), &options).unwrap();
    assert!(program.verify().is_some(), "/{source}/{letters}");
    program
}

include!("support/cases.rs");

#[test]
fn known_answers() {
    let mut context = Context::new();
    for &(source, letters, subject, start, expected) in CASES {
        in_each_width(subject, |subject| {
            let outcome = match *expected {
                [start, end, ..] => Outcome::Match {
                    start: start as u32,
                    end: end as u32,
                },
                _ => Outcome::NoMatch,
            };
            let quick = program(source, letters, subject, Mode::MatchOnly);
            let found = quick.exec(subject, start, &mut [], &mut context);
            assert_eq!(found, outcome, "/{source}/{letters} on {subject:?}");

            let full = program(source, letters, subject, Mode::Captures);
            let mut captures = vec![7; full.capture_entries()];
            let found = full.exec(subject, start, &mut captures, &mut context);
            assert_eq!(found, outcome, "/{source}/{letters} on {subject:?}");
            if let [_, _, groups @ ..] = expected {
                let groups = groups.iter().map(|&it| u32::try_from(it).unwrap_or(UNSET));
                assert_eq!(
                    captures,
                    groups.collect::<Vec<_>>(),
                    "/{source}/{letters} on {subject:?}"
                );
            }
        });
    }
}

#[test]
fn a_long_match_is_interrupted() {
    let subject = [b'a'; 64];
    let subject = Text::Latin1(&subject);
    let program = program(r"^(a|aa)+\1b", "", subject, Mode::MatchOnly);
    let polls = Rc::new(Cell::new(0));
    let seen = Rc::clone(&polls);
    let mut context = Context::new();
    context.set_poll(Box::new(move || {
        seen.set(seen.get() + 1);
        seen.get() == 3
    }));
    let outcome = program.exec(subject, 0, &mut [], &mut context);
    assert_eq!((outcome, polls.get()), (Outcome::Interrupted, 3));
    assert!(context.take_work() > 0);
}

#[test]
fn the_stack_has_a_limit() {
    let subject = [b'a'; 5000];
    let subject = Text::Latin1(&subject);
    let program = program("^(?:a|ab)*bc", "", subject, Mode::MatchOnly);
    let mut context = Context::new();
    assert_eq!(
        program.exec(subject, 0, &mut [], &mut context),
        Outcome::NoMatch
    );
    context.set_max_stack_size(4096);
    assert_eq!(
        program.exec(subject, 0, &mut [], &mut context),
        Outcome::OutOfMemory
    );
}

#[test]
fn nesting_ends_at_the_limit_of_the_native_stack() {
    let source = format!("{}a{}", "(".repeat(5000), ")*".repeat(5000));
    let pattern = Pattern::new(Text::from_ascii(&source), flags("")).unwrap();
    let here = 0u8;
    let options = Options {
        width: Width::Latin1,
        mode: Mode::Captures,
        sample: Text::EMPTY,
        stack_limit: (&raw const here).addr() - (64 << 10),
    };
    let error = compile(pattern.tree(), &options).err();
    assert_eq!(error, Some(Error::TooDeeplyNested));
}
