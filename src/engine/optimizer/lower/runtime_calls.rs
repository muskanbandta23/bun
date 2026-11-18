//! Calls of the run-time functions that generated code leans on.

use bun_engine_asm::{Target as CallTarget, abi};
use bun_engine_codegen::ir::{
    Action, Block, EffectFlags, Effects, Input, Output, PatchpointData, PatchpointFlags, Type,
    Value as Ssa,
};
use smallvec::SmallVec;

use super::Lowering;
use super::sites::Site;

type Inputs = SmallVec<[(Ssa, Input); 6]>;

/// What a run-time function may do to the heap and to the frame.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Call {
    /// Reads and writes nothing that the graph describes, and neither throws nor collects.
    Leaf,
    /// May do anything, so the stack has to be walkable and the call site recorded.
    Operation,
}

/// A call that has been described but not yet placed.
struct Prepared {
    data: PatchpointData,
    operands: Inputs,
    results: SmallVec<[(Type, Output); 1]>,
}

impl Lowering<'_> {
    fn prepare(&mut self, target: usize, args: &[Ssa], result: Type, call: Call) -> Prepared {
        let (effects, site) = match call {
            Call::Leaf => (
                Effects::with_flags(EffectFlags::CONTROL_DEPENDENT),
                Site::Leaf,
            ),
            Call::Operation => (Effects::CALL, Site::Operation),
        };
        let token = self.sites.add(site, self.origin);
        let action = Action::Call(CallTarget::Address(target));
        let mut data = PatchpointData::new(action, token, effects);
        data.flags = PatchpointFlags::UNEXTENDED_RESULTS;
        data.clobbers_late = abi::CALLER_SAVED;
        data.clobbers_upper_halves = abi::CALLEE_SAVED_LOW_HALF;
        if call == Call::Operation {
            data.state = self.state;
        }
        let (mut gprs, mut fprs) = (0, 0);
        let operands = args
            .iter()
            .map(|&arg| {
                let place = if self.graph.function.type_of(arg).is_float() {
                    fprs += 1;
                    abi::ARGUMENT_FPRS[fprs - 1].into()
                } else {
                    gprs += 1;
                    abi::ARGUMENT_GPRS[gprs - 1].into()
                };
                (arg, Input::Fixed(place))
            })
            .collect();
        let place = if result.is_float() {
            abi::RETURN_FPR.into()
        } else {
            abi::RETURN_GPRS[0].into()
        };
        let results = (result != Type::Void)
            .then_some((result, Output::Fixed(place)))
            .into_iter()
            .collect();
        Prepared {
            data,
            operands,
            results,
        }
    }

    /// Calls before the node that is being lowered.
    pub(super) fn call_runtime(
        &mut self,
        target: usize,
        args: &[Ssa],
        result: Type,
        call: Call,
    ) -> Ssa {
        let it = self.prepare(target, args, result, call);
        self.ir().patchpoint(&it.data, &it.operands, &it.results)
    }

    /// Calls at the end of a block of its own.
    pub(super) fn call_runtime_in(
        &mut self,
        block: Block,
        target: usize,
        args: &[Ssa],
        result: Type,
        call: Call,
    ) -> Ssa {
        let it = self.prepare(target, args, result, call);
        self.at_end(block)
            .patchpoint(&it.data, &it.operands, &it.results)
    }
}
