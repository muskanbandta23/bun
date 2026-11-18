//! Compiles a function in one pass over its instructions.

mod atomic;
mod calls;
mod cast;
mod cold;
mod condition;
mod control;
mod division;
mod edges;
mod exceptions;
mod frame;
mod loops;
mod memory;
mod moves;
mod numeric;
mod operand;
mod prologue;
mod reference;
mod registers;
mod table;
mod variable;
mod vector;

use core::mem;

use bun_engine_asm::prelude::*;
use bun_engine_asm::{CodeTooLarge, Options};
use bun_engine_wasm_format::{
    BlockType, Catch, CodeIndex, Flow, FunctionBody, FunctionFlags, ModuleHeader, Visit,
};

use self::cold::Cold;
use self::condition::Pending;
use self::control::{Block, BlockKind};
use self::edges::{Detour, Target};
use self::exceptions::{Handler, Handlers};
pub use self::frame::Frame;
use self::loops::LoopPlans;
use self::memory::FirstMemory;
use self::moves::{Moves, Step};
use self::operand::Operand;
use self::registers::{Local, Owners, Registers};
use crate::abi::{Signature, Trap};
use crate::output::{CompiledFunction, Counters, DirectCall, Environment};

type Compiled = Result<(), CodeTooLarge>;

macro_rules! with_buffers {
    ($($(#[$meta:meta])* $buffer:ident: $type:ty,)* ; $($(#[$field_meta:meta])* $field:ident: $field_type:ty,)*) => {
        /// Memory that the compilation of one function leaves to the next.
        #[derive(Default)]
        pub struct Compiler {
            $($buffer: $type,)*
        }

        struct Function<'a> {
            $($(#[$field_meta])* $field: $field_type,)*
            $($(#[$meta])* $buffer: $type,)*
        }

        impl Function<'_> {
            fn exchange_buffers(&mut self, compiler: &mut Compiler) {
                $(mem::swap(&mut self.$buffer, &mut compiler.$buffer);)*
            }
        }
    };
}

with_buffers! {
    stack: Vec<Operand>,
    locals: Vec<Local>,
    blocks: Vec<Block>,
    else_targets: Vec<Target>,
    snapshots: Vec<Owners>,
    moves: Moves,
    /// Those of `detours`, then those that are about to be emitted.
    steps: Vec<Step>,
    detours: Vec<Detour>,
    cold: Vec<Cold>,
    table_labels: Vec<Label>,
    table_targets: Vec<u32>,
    catches: Vec<Catch>,
    handlers: Handlers,
    loops: LoopPlans,
    direct_calls: Vec<(Label, CodeIndex)>,
    ;
    masm: MacroAssembler,
    env: &'a Environment<'a>,
    module: &'a ModuleHeader,
    body: FunctionBody<'a>,
    bytes: &'a [u8],
    signature: Signature,
    frame: Frame,
    counters: Option<Counters>,
    loop_entry: Option<u32>,
    entry: Label,
    first_memory: FirstMemory,
    uses_simd: bool,
    keeps_all_locals: bool,
    /// Of the instruction that is being compiled.
    offset: usize,
    is_dead: bool,
    pending: Option<Pending>,
    registers: Registers,
    /// Below this position no operand is a constant or a local.
    lowest_lazy: u32,
    /// From the stack pointer up to the frame pointer.
    stack_depth: u32,
    loop_depth: u32,
    catch_depth: u32,
    traps: [Option<Label>; Trap::COUNT],
}

/// A guess.
const CODE_BYTES_FOR_EACH_BYTE: usize = 3;
/// What a larger function needed is not kept.
const RETAINED_BODY_SIZE: usize = 1 << 16;

impl Compiler {
    /// Without `counters` the code never asks for better code. With `loop_entry` it is entered at the start of that
    /// loop and takes over a [`Frame`].
    pub fn compile(
        &mut self,
        env: &Environment<'_>,
        code: CodeIndex,
        counters: Option<Counters>,
        loop_entry: Option<u32>,
    ) -> Result<CompiledFunction, CodeTooLarge> {
        let body = env.module.body(code);
        let signature = Signature::new(body.func_type());
        self.locals.clear();
        let frame = Frame::new(&body, &signature, |ty, offset| {
            self.locals.push(Local::new(ty, offset));
        });
        let first_memory = FirstMemory::new(env);
        let mut masm = MacroAssembler::new(Options {
            features: env.features,
            shorten_jumps: false,
            seed: env.seed ^ u64::from(code.0),
        });
        masm.reserve(body.bytes().len() * CODE_BYTES_FOR_EACH_BYTE);
        let mut function = Function {
            entry: masm.new_label(),
            masm,
            env,
            module: body.header(),
            body,
            bytes: body.bytes(),
            signature,
            frame,
            counters,
            loop_entry,
            first_memory,
            uses_simd: body.facts().flags.contains(FunctionFlags::USES_SIMD),
            keeps_all_locals: false,
            offset: 0,
            is_dead: false,
            pending: None,
            registers: Registers::new(first_memory),
            lowest_lazy: 0,
            stack_depth: frame.size(),
            loop_depth: 0,
            catch_depth: 0,
            traps: [None; Trap::COUNT],
            stack: Vec::new(),
            locals: Vec::new(),
            blocks: Vec::new(),
            else_targets: Vec::new(),
            snapshots: Vec::new(),
            moves: mem::take(&mut self.moves),
            steps: Vec::new(),
            detours: Vec::new(),
            cold: Vec::new(),
            table_labels: Vec::new(),
            table_targets: Vec::new(),
            catches: Vec::new(),
            handlers: Handlers::default(),
            loops: LoopPlans::default(),
            direct_calls: Vec::new(),
        };
        function.exchange_buffers(self);
        let result = function.emit();
        function.exchange_buffers(self);
        let result = result.and_then(|()| self.finish(function.masm, function.entry, &frame));
        if result.is_err() || body.bytes().len() > RETAINED_BODY_SIZE {
            *self = Self::default();
        }
        result
    }

    fn finish(
        &mut self,
        masm: MacroAssembler,
        entry: Label,
        frame: &Frame,
    ) -> Result<CompiledFunction, CodeTooLarge> {
        let code = masm.finish()?;
        let direct_call = |(end, callee)| DirectCall {
            end_offset: code.offset_of(end),
            callee,
        };
        let direct_calls = self.direct_calls.drain(..).map(direct_call).collect();
        let call_sites = self.handlers.call_sites(&code);
        self.stack.clear();
        self.snapshots.clear();
        self.cold.clear();
        self.handlers.clear();
        Ok(CompiledFunction {
            entry_offset: code.offset_of(entry),
            frame_size: frame.size(),
            call_sites,
            inline_frames: Vec::new(),
            inline_ranges: Vec::new(),
            direct_calls,
            code,
        })
    }
}

impl Function<'_> {
    fn emit(&mut self) -> Compiled {
        self.emit_prologue();
        let ty = BlockType::Func(self.body.type_index());
        self.open(BlockKind::Body, ty, Handler::CALLER);
        self.body.driver().run(self)?;
        self.emit_cold_code();
        Ok(())
    }
}

impl Visit for Function<'_> {
    type Error = CodeTooLarge;

    fn unhandled(&mut self) -> Compiled {
        unreachable!()
    }

    #[inline(always)]
    fn at(&mut self, offset: u32) {
        self.offset = offset as usize;
        self.registers.recent |= mem::take(&mut self.registers.locked);
    }

    #[inline(always)]
    fn flow(&self) -> Flow {
        if self.is_dead { Flow::Dead } else { Flow::Live }
    }
}
