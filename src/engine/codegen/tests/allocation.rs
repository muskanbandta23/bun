//! Compiles random programs without running them, which puts the allocation checker to work.

mod support;

use bun_engine_asm::{
    Label, MacroAssembly, Options as AssemblerOptions, RegisterSet, X64Features, x64,
};
use bun_engine_codegen::ir::{Binary, Op, Token};
use bun_engine_codegen::passes::Level;
use bun_engine_codegen::{
    CallSite, Client, ExitSite, Frame, Options, PatchpointSite, Target, X64Compiler,
};
use support::generator::Shape;
use support::harness::{Case, Subject, World, campaign, shape_for};
use support::random::mix;

struct Silent;

impl<M: MacroAssembly> Client<M> for Silent {
    fn prologue(&mut self, _: &mut M, _: usize, _: &Frame) {}
    fn shared_exit(&mut self, _: &mut M, _: Token) -> Option<Label> {
        None
    }
    fn exit(&mut self, _: &mut M, _: &ExitSite<'_>) {}
    fn patchpoint(&mut self, _: &mut M, _: &PatchpointSite<'_>) {}
    fn after_call(&mut self, _: &mut M, _: &CallSite<'_>) {}
    fn out_of_line(&mut self, _: &mut M) {}
}

#[derive(Default)]
struct CompileOnly(X64Compiler);

impl Subject for CompileOnly {
    fn world(&mut self, seed: u64) -> World {
        World {
            target: Target::X64(X64Features::from_bits_truncate(mix(seed, 9) as u16)),
            ..World::imaginary(seed)
        }
    }

    fn shape(&mut self, seed: u64) -> Shape {
        let mut shape = shape_for(seed);
        (
            shape.weights.variables,
            shape.weights.switches,
            shape.weights.vectors,
        ) = (0, 0, 0);
        shape
    }

    fn test(&mut self, case: &Case) -> Result<(), String> {
        let Target::X64(features) = case.world.target else {
            unreachable!()
        };
        for level in [Level::Quick, Level::Full] {
            let mut function = case.program.function.clone();
            for value in function
                .blocks()
                .flat_map(|block| function.instructions(block))
                .collect::<Vec<_>>()
            {
                match function.op_mut(value) {
                    Op::Binary(op @ (Binary::TotalDiv | Binary::TotalRem), _) => *op = Binary::Div,
                    Op::Switch(..) | Op::GetVariable(_) | Op::SetVariable(..) => return Ok(()),
                    Op::VectorUnary(..)
                    | Op::VectorBinary(..)
                    | Op::VectorTernary(..)
                    | Op::VectorShift(..)
                    | Op::VectorReduce(..)
                    | Op::Shuffle(..)
                    | Op::Splat(..)
                    | Op::ExtractLane { .. }
                    | Op::ReplaceLane { .. }
                    | Op::LoadLane { .. }
                    | Op::StoreLane { .. } => return Ok(()),
                    _ => {}
                }
            }
            let _ = RegisterSet::EMPTY;
            let mut masm = x64::MacroAssembler::new(AssemblerOptions {
                features,
                shorten_jumps: true,
                seed: 0,
            });
            self.0 = X64Compiler::default();
            let options = Options {
                level,
                records_origins: true,
            };
            let text = format!("{function:?}");
            support::harness::catching(|| {
                self.0
                    .compile(&mut function, options, &mut masm, &mut Silent)
                    .map(drop)
                    .unwrap()
            })
            .map_err(|problem| format!("{problem}\n{level:?}\n{text}"))?;
            masm.finish().unwrap();
        }
        Ok(())
    }
}

#[test]
fn random_programs() {
    campaign(300, CompileOnly::default);
}
