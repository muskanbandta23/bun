#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::disallowed_macros
)]

use std::env;
use std::fs;
use std::path::Path;

/// The release that scripts/build/deps/icu.ts builds, for a check of a target whose headers are not here.
const PINNED_MAJOR: u32 = 79;

fn major_in_headers(include: &Path) -> Option<u32> {
    let header = include.join("unicode/uvernum.h");
    let text = fs::read_to_string(&header).ok()?;
    println!("cargo:rerun-if-changed={}", header.display());
    text.lines()
        .find_map(|line| line.strip_prefix("#define U_ICU_VERSION_MAJOR_NUM "))
        .and_then(|major| major.trim().parse().ok())
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=BUN_ICU_ROOT");
    println!("cargo:rustc-check-cfg=cfg(system_icucore)");

    // The library of macOS keeps its names across releases; every other build appends the major version.
    if env::var("CARGO_CFG_TARGET_OS").unwrap() == "macos" {
        println!("cargo:rustc-cfg=system_icucore");
        println!("cargo:rustc-env=BUN_ICU_SYMBOL_SUFFIX=");
        return;
    }
    let major = match env::var_os("BUN_ICU_ROOT") {
        Some(root) => major_in_headers(&Path::new(&root).join("include")),
        None if env::var("HOST") == env::var("TARGET") => {
            major_in_headers(Path::new("/usr/include"))
        }
        None => None,
    };
    println!(
        "cargo:rustc-env=BUN_ICU_SYMBOL_SUFFIX=_{}",
        major.unwrap_or(PINNED_MAJOR)
    );
}
