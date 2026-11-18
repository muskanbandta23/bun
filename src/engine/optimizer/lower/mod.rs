//! From the graph to machine code: what stands for run-time work becomes plain instructions and patchpoints.

mod allocate;
mod calls;
mod checks;
mod client;
mod interpret;
mod numbers;
pub(crate) mod output;
mod places;
mod runtime_calls;
mod sites;
mod strings;
mod types;
mod values;

use bun_engine_asm::{Gpr, MacroAssembly, abi};
use bun_engine_codegen::ir::{
    self, Block, Entity, Exit, Origin, State, Successor, Type, Value as Ssa,
};
use bun_engine_vm::Value;

use self::client::{Emitter, Tables};
use self::output::Output;
use self::sites::Sites;
use crate::builder::{Built, Refusal};
use crate::graph::{ExitKind, Gives, Graph, Kind};

/// Memory that the compilation of one function leaves to the next.
#[derive(Default)]
pub(crate) struct Compiler {
    sites: Sites,
    tables: Tables,
    back_end: bun_engine_codegen::Compiler,
}

impl Compiler {
    /// Machine code for a graph that the passes are done with, and the tables that the run time needs.
    pub(crate) fn compile(
        &mut self,
        mut graph: Graph,
        features: bun_engine_asm::Features,
    ) -> Result<Output, Refusal> {
        self.sites.clear();
        run(&mut graph, &mut self.sites)?;
        let mut masm = bun_engine_asm::MacroAssembler::new(bun_engine_asm::Options {
            features,
            shorten_jumps: true,
            seed: 0,
        });
        let options = bun_engine_codegen::Options {
            level: graph.level,
            records_origins: false,
        };
        // The back end wants the function to itself, and the emitter the rest of the graph.
        let placeholder = ir::Function::new(bun_engine_asm::RegisterSet::EMPTY);
        let mut function = core::mem::replace(&mut graph.function, placeholder);
        let (compiled, code) = {
            let mut emitter = Emitter::new(&graph, &self.sites, &mut self.tables);
            let compiled = (self.back_end)
                .compile(&mut function, options, &mut masm, &mut emitter)
                .map_err(|_| Refusal::TooLarge)?;
            (compiled, masm.finish().map_err(|_| Refusal::TooLarge)?)
        };
        Ok(client::finish(&mut self.tables, code, &compiled, graph))
    }
}

struct Lowering<'a> {
    graph: &'a mut Graph,
    sites: &'a mut Sites,
    /// The node that is being replaced.
    at: Ssa,
    origin: Origin,
    state: State,
}

/// Turns every node of the graph into instructions that the back end knows.
fn run(graph: &mut Graph, sites: &mut Sites) -> Built {
    let mut lowering = Lowering {
        graph,
        sites,
        at: Ssa::NONE,
        origin: Origin::default(),
        state: State::NONE,
    };
    let mut index = 0;
    while index < lowering.graph.function.block_count() {
        let block = Block::from_index(index);
        if !lowering.graph.function.is_removed(block) {
            lowering.walk(block)?;
        }
        index += 1;
    }
    lowering.graph.function.resolve_aliases();
    Ok(())
}

impl Lowering<'_> {
    fn walk(&mut self, block: Block) -> Built {
        let mut value = self.graph.function.first(block);
        while !value.is_none() {
            self.graph.function.resolve_operands(value);
            if let Some((kind, data)) = self.graph.node(value) {
                (self.at, self.origin, self.state) = (
                    value,
                    self.graph.function.origin(value),
                    self.graph.function.state_of(value),
                );
                self.lower(kind, data)?;
            }
            value = self.graph.function.next(value);
        }
        Ok(())
    }

    fn lower(&mut self, kind: Kind, data: u32) -> Built {
        let replacement = match kind {
            // Places
            Kind::Checkpoint | Kind::Refine => return self.erase(kind),
            Kind::InvalidationPoint => return self.invalidation_point(),
            Kind::ForceExit => return self.force_exit(data),
            Kind::PollTraps => return self.poll_traps(),
            Kind::TierUpInLoop => return self.tier_up_in_loop(),
            Kind::StoreBarrier => return self.store_barrier(),
            Kind::PublishFence => return self.publish_fence(),

            // Representations
            Kind::BoxInt32 => self.box_int32(self.operand(0)),
            Kind::BoxInt52 => self.box_int52(),
            Kind::BoxDouble => self.box_double(self.operand(0)),
            Kind::BoxBoolean => self.box_boolean(),
            Kind::NumberToDouble => return self.number_to_double(data != 0),
            Kind::PrimitiveToDouble => return self.primitive_to_double(),
            Kind::TruncateDouble => return self.truncate_double(),
            Kind::DoubleAsInt32 => return self.double_as_int32(),
            Kind::DoubleAsInt52 => return self.double_as_int52(),

            // Checks
            Kind::CheckType => return self.check_type(data),
            Kind::CheckShape => return self.check_shape(data),
            Kind::CheckElements => return self.check_elements(data),
            Kind::CheckIdentical => return self.check_identical(),
            Kind::CheckExecutable => return self.check_executable(),

            // Tests
            Kind::IsType => self.is_type(data)?,
            Kind::IsCellOfType => self.is_cell_of_type(data)?,
            Kind::GenericEqualNull => self.generic_equal_null(),
            Kind::AtomEqual => self.atom_equal(),

            // Objects
            Kind::LoadShapeId => {
                let cell = self.operand(0);
                self.load_shape_id_of(cell)
            }
            Kind::SetShape => return self.set_shape(data),
            Kind::NewObject => return self.new_object(data),
            Kind::NewScope => return self.new_scope(data),
            Kind::NewFunction => return self.new_function(data),

            // Strings
            Kind::StringLength => self.string_length(),
            Kind::StringUnitAt => self.string_unit_at(),

            // The interpreter
            Kind::ShadowFrame => self.shadow_frame(),
            Kind::Interpret => return self.interpret(data),

            // Calls
            Kind::Call | Kind::DirectCall | Kind::Construct | Kind::DirectConstruct => {
                return self.call(kind, data);
            }
            Kind::TailCall | Kind::DirectTailCall => return self.tail_call(kind, data),

            // What needs run-time support that is not in yet
            _ => return Err(Refusal::Unsupported),
        };
        self.graph.function.replace(self.at, replacement);
        Ok(())
    }

    /// For a node that stands for nothing once the graph is lowered.
    fn erase(&mut self, kind: Kind) -> Built {
        if let Gives::Operand = kind.gives() {
            let operand = self.operand(0);
            self.graph.function.replace(self.at, operand);
        } else {
            self.graph.function.remove(self.at);
        }
        Ok(())
    }

    // The instruction being lowered

    #[inline]
    fn operand(&self, position: usize) -> Ssa {
        self.graph.operand(self.at, position)
    }

    fn operands(&self) -> smallvec::SmallVec<[Ssa; 8]> {
        self.graph.function.operands(self.at).into()
    }

    #[inline]
    fn ir(&mut self) -> ir::Builder<'_> {
        ir::Builder::before(&mut self.graph.function, self.at)
    }

    fn at_end(&mut self, block: Block) -> ir::Builder<'_> {
        let mut builder = ir::Builder::at_end(&mut self.graph.function, block);
        builder.origin = self.origin;
        builder
    }

    #[inline]
    fn block(&self) -> Block {
        self.graph.function.block_of(self.at)
    }

    fn exit(&mut self, kind: ExitKind) -> Exit {
        let state = self.state;
        self.graph.exit(kind, self.origin, state)
    }

    fn exit_if(&mut self, condition: Ssa, kind: ExitKind) {
        if !self.graph.function.is_int_constant(condition, 0) {
            let exit = self.exit(kind);
            self.ir().check(condition, exit);
        }
    }

    // Values that the code always has

    #[inline]
    fn vm(&mut self) -> Ssa {
        self.graph.function.pinned(abi::VM)
    }

    #[inline]
    fn frame_pointer(&mut self) -> Ssa {
        self.graph.function.pinned(abi::FRAME_POINTER)
    }

    /// A constant that a pinned register holds, which costs no instruction where it can be used as it is.
    fn tag(&mut self, register: Gpr) -> Ssa {
        self.graph.function.pinned(register)
    }

    fn word(&mut self, bits: u64) -> Ssa {
        self.graph.function.const64(bits as i64)
    }

    fn address(&mut self, address: usize) -> Ssa {
        self.graph.function.const64(address as i64)
    }

    fn int32(&mut self, value: i32) -> Ssa {
        self.graph.function.const32(value)
    }

    /// The bits of a tagged value that is no cell.
    fn immediate(&mut self, value: Value) -> Ssa {
        debug_assert!(!value.is_cell());
        self.graph.function.const64(value.to_bits() as i64)
    }

    // Control flow

    /// Moves what follows the node into a new block, which gets a parameter for the node's result. The node is
    /// gone and its own block is left without an end, for the caller to fill in.
    fn split(&mut self) -> (Block, Ssa) {
        let function = &mut self.graph.function;
        let after = function.next(self.at);
        debug_assert!(!after.is_none(), "the node ends its block");
        let tail = function.split_before(after);
        if function.type_of(self.at) == Type::Void {
            function.remove(self.at);
            return (tail, Ssa::NONE);
        }
        let ty = function.type_of(self.at);
        let result = function.add_param(tail, ty);
        function.replace(self.at, result);
        (tail, result)
    }

    /// Branches on `condition` to a block that `build` fills in, which gives the result that reaches the rest of
    /// the function from there; where the condition does not hold, the result is `otherwise`.
    fn branch_out(
        &mut self,
        condition: Ssa,
        otherwise: Ssa,
        is_rare: bool,
        build: impl FnOnce(&mut Self, Block) -> Ssa,
    ) {
        self.diamond(condition, is_rare, build, |_, _| otherwise);
    }

    /// Both ways of a branch are blocks of their own, which `when` and `otherwise` fill in. Each gives the
    /// result that reaches the rest of the function from its side.
    fn diamond(
        &mut self,
        condition: Ssa,
        is_rare: bool,
        when: impl FnOnce(&mut Self, Block) -> Ssa,
        otherwise: impl FnOnce(&mut Self, Block) -> Ssa,
    ) {
        let source = self.block();
        let (taken, fallen) = (
            self.graph.function.new_block(),
            self.graph.function.new_block(),
        );
        let (tail, result) = self.split();
        let first = Successor::new(taken, &[]);
        let first = if is_rare { first.rare() } else { first };
        self.at_end(source)
            .branch(condition, first, Successor::new(fallen, &[]));
        let given = when(self, taken);
        self.join(taken, tail, result, given);
        let given = otherwise(self, fallen);
        self.join(fallen, tail, result, given);
    }

    /// Branches before the node, which stays at the start of the block that both ways lead to, so that what the
    /// node lowers to can still be inserted before it. Gives what the two ways agree on.
    fn branch_before(
        &mut self,
        condition: Ssa,
        ty: Type,
        is_rare: bool,
        when: impl FnOnce(&mut Self, Block) -> Ssa,
        otherwise: impl FnOnce(&mut Self, Block) -> Ssa,
    ) -> Ssa {
        let source = self.block();
        let tail = self.graph.function.split_before(self.at);
        let result = self.graph.function.add_param(tail, ty);
        let (taken, fallen) = (
            self.graph.function.new_block(),
            self.graph.function.new_block(),
        );
        let first = Successor::new(taken, &[]);
        let first = if is_rare { first.rare() } else { first };
        self.at_end(source)
            .branch(condition, first, Successor::new(fallen, &[]));
        let given = when(self, taken);
        self.at_end(taken).jump(tail, &[given]);
        let given = otherwise(self, fallen);
        self.at_end(fallen).jump(tail, &[given]);
        result
    }

    fn join(&mut self, from: Block, tail: Block, result: Ssa, given: Ssa) {
        let passed: &[Ssa] = if result.is_none() {
            &[]
        } else {
            core::slice::from_ref(&given)
        };
        self.at_end(from).jump(tail, passed);
    }
}
