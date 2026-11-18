#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::disallowed_macros
)]

use std::env;
use std::fs;
use std::path::Path;

/// For a check of a target whose headers are not here: nothing is linked, so any suffix will do.
const UNLINKED_MAJOR: &str = "0";

fn major_in_headers(include: &Path) -> Option<String> {
    let header = include.join("unicode/uvernum.h");
    let text = fs::read_to_string(&header).ok()?;
    println!("cargo:rerun-if-changed={}", header.display());
    text.lines()
        .find_map(|line| line.strip_prefix("#define U_ICU_VERSION_MAJOR_NUM "))
        .map(|major| major.trim().to_owned())
}

fn major() -> Option<String> {
    // Set by scripts/build/rust.ts when the build compiles ICU.
    if let Ok(major) = env::var("BUN_ICU_MAJOR_VERSION") {
        return Some(major);
    }
    if let Some(root) = env::var_os("BUN_ICU_ROOT") {
        return major_in_headers(&Path::new(&root).join("include"));
    }
    if env::var("HOST") != env::var("TARGET") {
        return None;
    }
    ["/usr/local/include", "/usr/include"]
        .into_iter()
        .find_map(|include| major_in_headers(Path::new(include)))
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=BUN_ICU_MAJOR_VERSION");
    println!("cargo:rerun-if-env-changed=BUN_ICU_ROOT");
    println!("cargo:rustc-check-cfg=cfg(system_icucore)");

    // The library of macOS keeps its names across releases; every other build appends the major version.
    if env::var("CARGO_CFG_TARGET_OS").unwrap() == "macos" {
        println!("cargo:rustc-cfg=system_icucore");
        println!("cargo:rustc-env=BUN_ICU_SYMBOL_SUFFIX=");
        return;
    }
    println!(
        "cargo:rustc-env=BUN_ICU_SYMBOL_SUFFIX=_{}",
        major().as_deref().unwrap_or(UNLINKED_MAJOR)
    );
}
