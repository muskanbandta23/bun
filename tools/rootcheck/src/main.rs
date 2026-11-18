//! A compiler driver that finds references to cells where the engine's collector does not look. See README.md.

#![feature(rustc_private)]

extern crate rustc_abi;
extern crate rustc_ast;
extern crate rustc_attr_ir;
extern crate rustc_borrowck;
extern crate rustc_data_structures;
extern crate rustc_driver;
extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_index;
extern crate rustc_infer;
extern crate rustc_interface;
extern crate rustc_lint_defs;
extern crate rustc_middle;
extern crate rustc_span;
extern crate rustc_trait_selection;

mod allow;
mod bodies;
mod borrows;
mod calls;
mod engine;
mod items;
mod types;

use std::process::ExitCode;

use rustc_driver::{Callbacks, Compilation};
use rustc_interface::interface::Compiler;
use rustc_middle::ty::TyCtxt;
use rustc_span::Symbol;

use crate::engine::Engine;

const LIST_LENDERS: &str = "ROOTCHECK_LENDERS";

struct Rootcheck;

impl Callbacks for Rootcheck {
    /// Has cargo check the crate again when this program changes.
    fn after_expansion<'tcx>(&mut self, _: &Compiler, tcx: TyCtxt<'tcx>) -> Compilation {
        if Engine::is_visible(tcx) {
            let program = std::env::current_exe().expect("a path to this program");
            let program = Symbol::intern(&program.to_string_lossy());
            tcx.sess.file_depinfo.borrow_mut().insert(program);
            let value = std::env::var(LIST_LENDERS).ok().map(|it| Symbol::intern(&it));
            tcx.sess.env_depinfo.borrow_mut().insert((Symbol::intern(LIST_LENDERS), value));
        }
        Compilation::Continue
    }

    fn after_analysis<'tcx>(&mut self, _: &Compiler, tcx: TyCtxt<'tcx>) -> Compilation {
        if let Some(engine) = Engine::find(tcx) {
            let cx = types::Types::new(tcx, engine);
            items::check(&cx);
            if std::env::var_os(LIST_LENDERS).is_some() {
                borrows::list_lenders(&cx);
            }
            let suspects = borrows::find_suspects(&cx);
            let calls = calls::Calls::new(&cx);
            calls::check(&calls, &cx);
            borrows::report(&calls, &cx, suspects);
            allow::check(&cx);
        }
        Compilation::Continue
    }
}

fn main() -> ExitCode {
    // Cargo passes the path of the compiler first, which takes the place of the name of the program.
    let mut arguments: Vec<String> = std::env::args().skip(1).collect();
    arguments.extend(
        [
            "--cfg=rootcheck",
            "--check-cfg=cfg(rootcheck)",
            "-Zcrate-attr=feature(register_tool)",
            "-Zcrate-attr=register_tool(rootcheck)",
            "-Zalways-encode-mir",
            "-Zmir-opt-level=0",
        ]
        .map(String::from),
    );
    rustc_driver::install_ice_hook("tools/rootcheck", |_| ());
    rustc_driver::catch_with_exit_code(|| {
        rustc_driver::compiler_entrypoint(&arguments, &mut Rootcheck)
    })
}
