//! Cuts `js/*.js` into functions. A function starts with `function`, `async function` or `function*` in the first
//! column and ends with `}` in the first column. The lines before it can say more:
//!
//! - `// @intrinsic Name`: its `Intrinsic`.
//! - `// @name text`: what its `name` is, if that is not what it is declared as.
//! - `// @private`: other built-ins call it as `@name`, which is in `names.txt`. Stack traces do not show it.
//! - `// @visibility PrivateRecursive`: its `Visibility`.
//! - `// @nakedConstructor`: `new` calls it with no `this`.

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

#[derive(Default)]
struct Annotations<'a> {
    intrinsic: Option<&'a str>,
    name: Option<&'a str>,
    visibility: Option<&'a str>,
    is_private: bool,
    is_naked_constructor: bool,
}

impl<'a> Annotations<'a> {
    fn add(&mut self, annotation: &'a str) {
        match annotation.split_once(' ').unwrap_or((annotation, "")) {
            ("intrinsic", name) => self.intrinsic = Some(name),
            ("name", name) => self.name = Some(name),
            ("visibility", name) => self.visibility = Some(name),
            ("private", "") => self.is_private = true,
            ("nakedConstructor", "") => self.is_naked_constructor = true,
            _ => panic!("unknown annotation @{annotation}"),
        }
    }
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

    let (mut ids, mut arms, mut private, mut text) =
        (String::new(), String::new(), String::new(), String::new());
    let mut count = 0;
    for file in files {
        let module = snake_case(file.file_stem().unwrap().to_str().unwrap());
        writeln!(ids, "pub(crate) mod {module} {{ use super::*;").unwrap();
        let source = fs::read_to_string(&file).unwrap();
        let mut lines = source.lines();
        let mut annotations = Annotations::default();
        while let Some(line) = lines.next() {
            if let Some(annotation) = line.strip_prefix("// @") {
                annotations.add(annotation);
            }
            let header = line.strip_prefix("async ").unwrap_or(line);
            let Some(rest) = header.strip_prefix("function") else {
                continue;
            };
            let declared = rest
                .trim_start_matches(['*', ' '])
                .split('(')
                .next()
                .unwrap();
            let (before, after) = line.split_once('(').unwrap();
            let start = text.len();
            write!(text, "({}({after}", before.strip_suffix(declared).unwrap()).unwrap();
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
            assert!(text.is_ascii(), "{declared}");
            writeln!(
                ids,
                "pub(crate) const r#{declared}: BuiltinId = BuiltinId({count});"
            )
            .unwrap();
            let annotations = std::mem::take(&mut annotations);
            if annotations.is_private {
                writeln!(
                    private,
                    "(private_names::r#{declared}, BuiltinId({count})),"
                )
                .unwrap();
            }
            writeln!(
                arms,
                "{count} => BuiltinSource {{ name: {:?}, range: {start}..{}, kind: FunctionKind::{}, visibility: Visibility::{}, intrinsic: Intrinsic::{} }},",
                annotations.name.unwrap_or(declared),
                text.len(),
                if annotations.is_naked_constructor { "NakedConstructor" } else { "Method" },
                annotations.visibility.unwrap_or(if annotations.is_private { "Private" } else { "Public" }),
                annotations.intrinsic.unwrap_or("None"),
            )
            .unwrap();
            count += 1;
        }
        writeln!(ids, "}}").unwrap();
    }
    let out = format!(
        "{ids}
pub static BUILTINS_TEXT: &str = {text:?};
pub fn builtin_source(id: BuiltinId) -> BuiltinSource {{ match id.0 {{ {arms} _ => unreachable!() }} }}
pub fn private_builtins() -> &'static [(PropertyName<'static>, BuiltinId)] {{ const {{ &[{private}] }} }}"
    );
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(out_dir.join("js.rs"), out).unwrap();
}
