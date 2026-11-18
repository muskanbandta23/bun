//! Cuts `js/*.js` into functions. A function starts with `function`, `async function` or `function*` in the first
//! column and ends with `}` in the first column. A line `// @intrinsic Name` before it names its `Intrinsic`.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::disallowed_macros
)]

use std::fmt::Write;
use std::path::PathBuf;
use std::{env, fs};

fn snake_case(name: &str) -> String {
    let mut out = String::new();
    for character in name.chars() {
        if character.is_ascii_uppercase() && !out.is_empty() {
            out.push('_');
        }
        out.push(character.to_ascii_lowercase());
    }
    out
}

fn main() {
    println!("cargo:rerun-if-changed=js");
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let mut files: Vec<PathBuf> = fs::read_dir(manifest.join("js"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|it| it == "js"))
        .collect();
    files.sort();

    let (mut ids, mut arms, mut count) = (String::new(), String::new(), 0);
    for file in files {
        let module = snake_case(file.file_stem().unwrap().to_str().unwrap());
        writeln!(ids, "pub(crate) mod {module} {{ use super::*;").unwrap();
        let source = fs::read_to_string(&file).unwrap();
        let mut lines = source.lines();
        let mut intrinsic = "None";
        while let Some(line) = lines.next() {
            if let Some(name) = line.strip_prefix("// @intrinsic ") {
                intrinsic = name;
            }
            let header = line.strip_prefix("async ").unwrap_or(line);
            let Some(rest) = header.strip_prefix("function") else {
                continue;
            };
            let name = rest
                .trim_start_matches(['*', ' '])
                .split('(')
                .next()
                .unwrap();
            let mut text = format!("({}", line.replacen(name, "", 1));
            for line in lines.by_ref() {
                let trimmed = line.trim();
                if !trimmed.is_empty() && !trimmed.starts_with("//") {
                    text.push('\n');
                    text.push_str(trimmed);
                }
                if line == "}" {
                    break;
                }
            }
            text.push(')');
            assert!(text.is_ascii(), "{name}");
            writeln!(
                ids,
                "pub(crate) const r#{name}: BuiltinId = BuiltinId({count});"
            )
            .unwrap();
            writeln!(
                arms,
                "{count} => BuiltinSource {{ name: names::r#{name}, text: {text:?}, intrinsic: Intrinsic::{intrinsic} }},"
            )
            .unwrap();
            count += 1;
            intrinsic = "None";
        }
        writeln!(ids, "}}").unwrap();
    }
    let out = format!(
        "{ids}pub fn builtin_source(id: BuiltinId) -> BuiltinSource {{ match id.0 {{ {arms} _ => unreachable!() }} }}"
    );
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(out_dir.join("js.rs"), out).unwrap();
}
