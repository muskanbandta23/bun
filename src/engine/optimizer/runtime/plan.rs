//! Compiling a code block on the thread of the `Vm`, and installing what comes out.

use std::rc::Rc;

use bun_engine_asm::Features;
use bun_engine_codegen::passes::Level;
use bun_engine_exec_memory::{Priority, link};
use bun_engine_vm::{
    CallFrame, CodeBlock, Gc, GlobalObject, JsCell, Shape, Tier, Vm, Watcher, Watchpoint, options,
};

use super::code::Compiled;
use crate::builder::{self, Dependencies};
use crate::graph::Entry;
use crate::lower;
use crate::lower::output::{Output, ResolvedAssumption, symbol};

/// The counter of the code block has run out, at the entry or at the head of a loop. Returns where the frame goes on,
/// or zero if it stays where it is.
pub fn tier_up(
    vm: &Vm,
    code_block: &CodeBlock,
    frame: &CallFrame,
    loop_offset: Option<u32>,
) -> usize {
    let code_block: Gc<CodeBlock> = code_block.into();
    // Nothing asks again until a compile arms the counter.
    code_block.execution_counter().set(i32::MIN);
    let Some(level) = level_for(code_block) else {
        return 0;
    };
    let entry = match loop_offset {
        Some(offset) => Entry::Loop(offset),
        None => Entry::Function,
    };
    let Some(compiled) = compile(vm, code_block, level, entry) else {
        return 0;
    };
    match entry {
        Entry::Function => {
            install(vm, code_block, tier(level), compiled);
            0
        }
        Entry::Loop(_) => {
            // Code that takes over a frame is entered only by this one, so nothing else is told of it.
            let replacement = replacement(vm, code_block, tier(level), compiled);
            let entry = entry_of(replacement);
            // SAFETY: the frame runs that code from now on, and it has the same shape.
            unsafe { frame.set_code_block(Some(replacement)) };
            arm(code_block, options().threshold_for_quick_optimizer);
            entry
        }
    }
}

/// A return from code that may be optimized further. The next call runs what this makes.
#[bun_engine_macros::native_abi]
pub(super) fn tier_up_at_return(vm: &Vm, frame: &CallFrame) {
    let Some(code_block) = frame.code_block() else {
        return;
    };
    code_block.execution_counter().set(i32::MIN);
    let baseline = code_block.baseline();
    if let Some(level) = level_for(baseline)
        && let Some(compiled) = compile(vm, baseline, level, Entry::Function)
    {
        install(vm, baseline, tier(level), compiled);
    }
}

/// How hard to try next, or `None` if there is nothing left to do for the code block.
fn level_for(code_block: Gc<CodeBlock>) -> Option<Level> {
    if !options().use_jit {
        return None;
    }
    let installed = code_block
        .executable()
        .code_block(code_block.specialization());
    match installed.map_or(Tier::Interpreter, |installed| installed.tier()) {
        Tier::Interpreter | Tier::Baseline => options().use_quick_optimizer.then_some(Level::Quick),
        Tier::QuickOptimized => options().use_full_optimizer.then_some(Level::Full),
        Tier::FullyOptimized => None,
    }
}

fn tier(level: Level) -> Tier {
    match level {
        Level::Quick => Tier::QuickOptimized,
        Level::Full => Tier::FullyOptimized,
    }
}

/// Optimized code is on its way out: running frames leave it, and calls go back to the tier below.
pub fn jettison(vm: &Vm, code_block: &CodeBlock) {
    if let Some(compiled) = code_block.compiled() {
        compiled.invalidate();
    }
    let baseline = code_block.baseline();
    let executable = code_block.executable();
    let specialization = code_block.specialization();
    let installed = executable.code_block(specialization);
    if installed.is_some_and(|installed| installed.address() == code_block.address()) {
        // SAFETY: the interpreter runs any code block, and this is the one it was linked with.
        unsafe {
            executable.install(specialization, vm.hooks().interpreter_entry, Some(baseline));
        }
    }
    let attempts = baseline.reoptimizations().min(6);
    arm(
        baseline,
        options().threshold_for_quick_optimizer << attempts,
    );
}

/// Lets the counter run out again after that many weighted executions.
fn arm(code_block: Gc<CodeBlock>, threshold: i32) {
    code_block
        .execution_counter()
        .set(threshold.saturating_neg());
}

/// `None` where the function cannot be compiled or an assumption has failed since it was read.
fn compile(
    vm: &Vm,
    code_block: Gc<CodeBlock>,
    level: Level,
    entry: Entry,
) -> Option<(Output, Dependencies)> {
    let (graph, dependencies) = builder::build(vm, code_block, level, entry).ok()?;
    let mut compiler = lower::Compiler::default();
    let output = compiler.compile(graph, Features::host()).ok()?;
    holds(vm, &output, &dependencies).then_some((output, dependencies))
}

/// Whether what the compiler read is still true. It ran while the heap could change.
fn holds(vm: &Vm, output: &Output, dependencies: &Dependencies) -> bool {
    let mut sets = dependencies.watchpoints.iter();
    sets.all(|set| set.is_still_valid())
        && output
            .assumptions
            .iter()
            .all(|&assumption| with_watchpoints(vm, assumption, |set| set.is_still_valid()))
}

/// What the assumption rests on. The addresses are of cells that the dependencies keep.
fn with_watchpoints<T>(
    vm: &Vm,
    assumption: ResolvedAssumption,
    body: impl FnOnce(&bun_engine_vm::InlineWatchpointSet) -> T,
) -> T {
    match assumption {
        ResolvedAssumption::Stable(shape) => {
            // SAFETY: `Dependencies::shapes` keeps it.
            let shape: Gc<Shape> = unsafe { Gc::from_address(shape) };
            body(shape.transition_watchpoints())
        }
        ResolvedAssumption::Realm(global, which) => {
            // SAFETY: `Dependencies::realms` keeps it.
            let global: Gc<GlobalObject> = unsafe { Gc::from_address(global) };
            body(global.watchpoints(vm, which))
        }
    }
}

/// Jettisons the code block it belongs to, which is where it is added from.
struct Assumption;

impl Watcher for Assumption {
    fn fire(&self, vm: &Vm, owner: Gc<JsCell>, _: &Watchpoint) {
        owner.cast::<CodeBlock>().jettison(vm);
    }
}

/// A code block of its own for the optimized code, with the watchpoints that it dies by.
pub(super) fn replacement(
    vm: &Vm,
    code_block: Gc<CodeBlock>,
    tier: Tier,
    (output, dependencies): (Output, Dependencies),
) -> Gc<CodeBlock> {
    let handle = link(&output.code, Priority::Optional, |which| match which {
        symbol::EXIT => super::exit_thunk(),
        _ => unreachable!("the compiler asks for no other address"),
    })
    .expect("there is room for the code");
    let replacement = code_block.create_replacement(vm, tier);
    let kept = dependencies
        .strong
        .iter()
        .map(|cell| cell.address())
        .collect();
    let named = (dependencies.weak.iter().map(|cell| cell.address()))
        .chain(dependencies.shapes.iter().map(|shape| shape.address()))
        .chain(dependencies.realms.iter().map(|global| global.address()))
        .chain(output.weak_references.iter().copied())
        .collect();
    // SAFETY: the code block holds the only `Compiled`, which drops the watchpoint with itself.
    let watchpoint = unsafe { Watchpoint::new(&replacement, Assumption) };
    for set in &dependencies.watchpoints {
        set.add(&watchpoint);
    }
    for &assumption in &output.assumptions {
        with_watchpoints(vm, assumption, |set| set.add(&watchpoint));
    }
    let compiled = Compiled::new(output, handle, kept, named, vec![watchpoint]);
    replacement.set_compiled(tier, Rc::new(compiled));
    // Code that may be optimized further counts its turns and its returns towards that.
    arm(replacement, options().threshold_for_full_optimizer);
    replacement
}

fn entry_of(code_block: Gc<CodeBlock>) -> usize {
    let compiled = code_block.compiled().expect("just installed");
    let any: &dyn core::any::Any = &*compiled;
    any.downcast_ref::<Compiled>().expect("ours").entry()
}

/// Calls from now on run the optimized code.
pub(super) fn install(
    vm: &Vm,
    code_block: Gc<CodeBlock>,
    tier: Tier,
    compiled: (Output, Dependencies),
) {
    let replacement = replacement(vm, code_block, tier, compiled);
    let specialization = code_block.specialization();
    // SAFETY: the code was made for that code block and is entered as a call of that kind.
    unsafe {
        executable_of(code_block).install(specialization, entry_of(replacement), Some(replacement));
    }
}

fn executable_of(code_block: Gc<CodeBlock>) -> Gc<bun_engine_vm::Executable> {
    code_block.executable().upcast()
}
