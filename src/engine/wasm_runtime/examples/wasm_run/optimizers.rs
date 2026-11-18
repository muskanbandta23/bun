use bun_engine_wasm_compiler::optimize::{CallFeedback, Feedback, InliningBudget, Level};
use bun_engine_wasm_compiler::{CompiledFunction, Environment};
use bun_engine_wasm_format::CodeIndex;
use bun_engine_wasm_runtime::{Compilers, Optimizer, optimize_fully};

use crate::support::baseline_without_counters;

/// Every call is hot, and indirect ones seem to go anywhere.
struct Fake<'a> {
    real: &'a dyn Feedback,
    functions: u32,
    seed: u64,
}

impl Feedback for Fake<'_> {
    fn entry_count(&self, _: CodeIndex) -> u32 {
        1000
    }

    fn call(&self, code: CodeIndex, site: u32) -> CallFeedback {
        let key = self.seed ^ (u64::from(code.0) << 20) ^ u64::from(site);
        let mut random = key.wrapping_add(1).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        let targets = 1 + (random >> 62) as u32;
        let target = |_| {
            random = (random ^ random >> 29).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            (CodeIndex((random >> 32) as u32 % self.functions), 1000 / targets)
        };
        let mut targets: Vec<_> = (0..targets).map(target).collect();
        targets.sort_unstable();
        targets.dedup();
        CallFeedback {
            count: 1000,
            targets: targets.into_iter().collect(),
        }
    }

    fn inlining_budget(&self) -> InliningBudget {
        self.real.inlining_budget()
    }
}

fn optimize<const IS_FULL: bool, const FAKES: bool>(
    compilers: &mut Compilers,
    env: &Environment<'_>,
    code: CodeIndex,
    loop_entry: Option<u32>,
    real: &dyn Feedback,
) -> Option<CompiledFunction> {
    let fake = Fake {
        real,
        functions: env.module.code_count(),
        seed: env.seed,
    };
    let feedback = if FAKES { &fake } else { real };
    let level = if IS_FULL { Level::Full } else { Level::Quick };
    let optimizing = &mut compilers.optimizing;
    optimizing.compile(env, code, loop_entry, level, feedback).ok()
}

pub(crate) fn named(name: &str, fakes_feedback: bool) -> Option<Optimizer> {
    Some(match (name, fakes_feedback) {
        ("full", false) => optimize_fully,
        ("full", true) => optimize::<true, true>,
        ("quick", false) => optimize::<false, false>,
        ("quick", true) => optimize::<false, true>,
        ("baseline", _) => baseline_without_counters,
        _ => return None,
    })
}
