#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::disallowed_macros
)]

use std::fmt::Write as _;
use std::io::Write as _;
use std::process::{Command, Stdio};

fn llvm_mc() -> Option<&'static str> {
    [
        "llvm-mc",
        "llvm-mc-23",
        "/usr/lib/llvm-23/bin/llvm-mc",
        "/opt/homebrew/opt/llvm/bin/llvm-mc",
    ]
    .into_iter()
    .find(|path| {
        Command::new(path)
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    })
}

/// One line for each instruction, with single spaces and no comments. `None` without LLVM.
pub(crate) fn disassemble(arguments: &[&str], bytes: &[u8]) -> Option<Vec<String>> {
    let mut child = Command::new(llvm_mc()?)
        .arg("--disassemble")
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = String::new();
    for byte in bytes {
        write!(input, "0x{byte:02x} ").unwrap();
    }
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()).unwrap());
    let output = child.wait_with_output().unwrap();
    writer.join().unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    let text = String::from_utf8(output.stdout).unwrap();
    let lines = text.lines().map(|line| {
        let code = line
            .split(" # ")
            .next()
            .unwrap()
            .split("//")
            .next()
            .unwrap();
        code.split_whitespace().collect::<Vec<_>>().join(" ")
    });
    Some(
        lines
            .filter(|line| !line.is_empty() && !line.starts_with('.'))
            .collect(),
    )
}

pub(crate) fn assert_disassembly(arguments: &[&str], bytes: &[u8], expected: &[String]) {
    let Some(actual) = disassemble(arguments, bytes) else {
        eprintln!("skipped: llvm-mc is not installed");
        return;
    };
    for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        assert_eq!(actual, expected, "instruction {index}");
    }
    assert_eq!(actual.len(), expected.len());
}
