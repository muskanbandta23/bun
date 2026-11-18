//! Hand-written code and hand-built tables, which exercise installing and leaving without the compiler.

#![allow(clippy::undocumented_unsafe_blocks)]

use bun_engine_asm::prelude::*;
use bun_engine_asm::{Code, Features, FloatWidth, Options as Assembly, Target};
use bun_engine_bytecode::{BlockIndex, Register, Specialization};
use bun_engine_codegen::ir::Origin;
use bun_engine_standalone as _;
use bun_engine_text::StringRef;
use bun_engine_vm::heap::HeapKind;
use bun_engine_vm::{
    CodeBlock, Cx, ExitKind, Gc, GlobalObject, JsFunction, ModuleHooks, Options, RealmHooks,
    ScriptExecutionStatus, SourceCode, SourceProvider, SourceType, Tier, Value, evaluate_script,
    set_options,
};

use super::{plan, values};
use crate::builder::Dependencies;
use crate::graph::{
    CallKind, Format, FrameIndex, FrameInfo, Layout, NO_LAYOUT, NO_RECIPE, Position,
};
use crate::lower::output::{EXIT_REGISTER, ExitRecord, ExitValue, Output, Place, symbol};

static HOOKS: RealmHooks = RealmHooks {
    modules: ModuleHooks::DEFAULT,
    promise_rejection_tracker: |_, _, _| {},
    report_uncaught_exception: |_, _| {},
    current_script_execution_owner: |cx| cx.global().upcast(),
    script_execution_status: |_, _| ScriptExecutionStatus::Running,
    default_language: StringRef::empty,
    derive_shadow_realm: |cx| Ok(cx.global()),
    compile_wasm_streaming: None,
    instantiate_wasm_streaming: None,
};

fn with_realm(body: impl FnOnce(&mut Cx)) {
    set_options(Options::default());
    let vm = bun_engine::create_vm(HeapKind::Small);
    // SAFETY: nothing else uses the `Vm`.
    unsafe { body(Cx::from_global(GlobalObject::create(vm.get(), &HOOKS))) };
}

fn evaluate(cx: &mut Cx, text: &str) -> Value {
    let text = StringRef::from_str(text);
    let provider = SourceProvider::build(text, None, SourceType::Program).finish();
    evaluate_script(cx, &SourceCode::whole(provider), Value::UNDEFINED).unwrap()
}

/// A function that has run once, so that its code block is there.
fn prepared(cx: &mut Cx, text: &str) -> (Value, Gc<CodeBlock>) {
    let function = evaluate(cx, text);
    cx.call(function, Value::UNDEFINED, &[Value::int32(0)])
        .unwrap();
    let executable = function.downcast::<JsFunction>().unwrap().executable();
    let code_block = executable.code_block(Specialization::Call).unwrap();
    (function, code_block)
}

fn assembler() -> MacroAssembler {
    MacroAssembler::new(Assembly {
        features: Features::host(),
        shorten_jumps: false,
        seed: 1,
    })
}

/// What a frame of the lower tier is to be given.
#[derive(Default)]
struct State {
    registers: Vec<Register>,
    values: Vec<ExitValue>,
}

impl State {
    fn put(&mut self, register: Register, place: Place, format: Format) -> &mut State {
        self.registers.push(register);
        self.values.push(ExitValue { place, format });
        self
    }
}

/// Code that enters a frame and leaves it again through the exit with number zero.
fn stand_in(frame_size: u32, prepare: impl FnOnce(&mut MacroAssembler)) -> (Code, u32) {
    let mut masm = assembler();
    let entry = masm.here();
    masm.enter_frame();
    masm.sub64(abi::STACK_POINTER, abi::STACK_POINTER, frame_size as i32);
    prepare(&mut masm);
    masm.move32(EXIT_REGISTER, 0);
    let exit = masm.external(Target::Symbol(symbol::EXIT));
    masm.jump(exit);
    let code = masm.finish().unwrap();
    let offset = code.offset_of(entry);
    (code, offset)
}

/// Installs code that stands in for what the compiler would have made.
fn install(
    cx: &mut Cx,
    code_block: Gc<CodeBlock>,
    resume: u32,
    state: State,
    prepare: impl FnOnce(&mut MacroAssembler),
) {
    let frame_size = (code_block.locals() * 8 + 8).next_multiple_of(16);
    let (code, entry) = stand_in(frame_size, prepare);
    let output = Output {
        code,
        entry,
        frame_size,
        call_sites: Vec::new(),
        call_site_values: Vec::new(),
        exits: vec![ExitRecord {
            kind: ExitKind::BadType,
            offset: 0,
            frame: FrameIndex::ROOT,
            bytecode_offset: resume,
            layout: 0,
            first_value: 0,
            value_count: state.values.len() as u32,
        }],
        exit_values: state.values,
        handlers: Vec::new(),
        invalidation_points: Vec::new(),
        weak_references: Vec::new(),
        assumptions: Vec::new(),
        frames: vec![FrameInfo {
            code_block: code_block.address(),
            global: code_block.global().address(),
            caller: Origin(0),
            kind: CallKind::Call,
            frame_words: code_block.locals(),
            parameters: code_block.parameters(),
            depth: 0,
        }],
        layouts: vec![Layout {
            resume: Origin(0),
            caller: NO_LAYOUT,
            first_register: 0,
            register_count: state.registers.len() as u32,
            recipe: NO_RECIPE,
        }],
        layout_registers: state.registers,
        positions: vec![Position {
            frame: FrameIndex::ROOT,
            offset: resume,
        }],
    };
    let mut dependencies = Dependencies::default();
    dependencies.strong.push(code_block.cell());
    plan::install(cx, code_block, Tier::QuickOptimized, (output, dependencies));
}

fn call(cx: &mut Cx, function: Value, argument: Value) -> Value {
    cx.call(function, Value::UNDEFINED, &[argument]).unwrap()
}

#[test]
fn code_that_leaves_at_once_runs_interpreted_from_the_start() {
    with_realm(|cx| {
        let (function, code_block) = prepared(cx, "(function (a) { return a + 1 })");
        install(cx, code_block, 0, State::default(), |_| {});
        assert_eq!(call(cx, function, Value::int32(7)).as_number(), Some(8.0));
    });
}

#[test]
fn an_exit_puts_a_value_of_each_format_where_the_interpreter_reads_it() {
    let cases: [(Place, Format, f64); 5] = [
        (
            Place::Constant(Value::int32(41).to_bits()),
            Format::Tagged,
            42.0,
        ),
        (Place::Constant(41), Format::Int32, 42.0),
        (
            Place::Constant(1 << 40),
            Format::Int52,
            (1u64 << 40) as f64 + 1.0,
        ),
        (Place::Constant(41.5f64.to_bits()), Format::Double, 42.5),
        (Place::Constant(1), Format::Boolean, 2.0),
    ];
    with_realm(|cx| {
        for (place, format, expected) in cases {
            let (function, code_block) = prepared(cx, "(function (a) { return a + 1 })");
            let mut state = State::default();
            state.put(Register::argument(0), place, format);
            install(cx, code_block, 0, state, |_| {});
            let result = call(cx, function, Value::int32(0));
            assert_eq!(result.as_number(), Some(expected), "{place:?} {format:?}");
        }
    });
}

#[test]
fn an_exit_reads_values_out_of_the_registers_and_the_frame() {
    with_realm(|cx| {
        let gpr = abi::TEMPORARY_GPRS[0];
        let (function, code_block) = prepared(cx, "(function (a) { return a + 1 })");
        let mut state = State::default();
        state.put(
            Register::argument(0),
            Place::Gpr(gpr.number()),
            Format::Int32,
        );
        install(cx, code_block, 0, state, |masm| masm.move32(gpr, 41));
        assert_eq!(call(cx, function, Value::int32(0)).as_number(), Some(42.0));

        let fpr = abi::ARGUMENT_FPRS[0];
        let (function, code_block) = prepared(cx, "(function (a) { return a + 1 })");
        let mut state = State::default();
        state.put(
            Register::argument(0),
            Place::Fpr(fpr.number()),
            Format::Double,
        );
        install(cx, code_block, 0, state, |masm| {
            masm.move_float_constant(FloatWidth::F64, fpr, 41.5f64.to_bits());
        });
        assert_eq!(call(cx, function, Value::int32(0)).as_number(), Some(42.5));

        let (function, code_block) = prepared(cx, "(function (a) { return a + 1 })");
        let mut state = State::default();
        let slot = Register::argument(0).offset() * 8;
        state.put(Register::argument(0), Place::Frame(slot), Format::Tagged);
        install(cx, code_block, 0, state, |_| {});
        assert_eq!(call(cx, function, Value::int32(7)).as_number(), Some(8.0));
    });
}

/// The block that a later block jumps back to.
fn loop_header(code_block: Gc<CodeBlock>) -> BlockIndex {
    let liveness = code_block.liveness().unwrap();
    let graph = liveness.graph();
    let is_header = |block: &BlockIndex| {
        let start = graph.range(*block).start;
        (graph.predecessors(*block).iter()).any(|edge| graph.range(edge.block).start >= start)
    };
    graph.blocks().find(is_header).expect("the loop is there")
}

#[test]
fn an_exit_at_a_loop_gives_the_interpreter_the_locals_that_are_live() {
    with_realm(|cx| {
        let text = "(function (n) { let s = 0; while (n > 0) { s = s + n; n = n - 1; } return s })";
        let (function, code_block) = prepared(cx, text);
        let header = loop_header(code_block);
        let liveness = code_block.liveness().unwrap();
        let live: Vec<Register> = liveness.live_in(header).iter().collect();
        assert_eq!(
            live.len(),
            1,
            "only the sum is live at the head of the loop"
        );
        let mut state = State::default();
        state.put(Register::argument(0), Place::Constant(3), Format::Int32);
        state.put(live[0], Place::Constant(0), Format::Int32);
        let resume = liveness.graph().range(header).start;
        drop(liveness);
        install(cx, code_block, resume, state, |_| {});
        assert_eq!(call(cx, function, Value::int32(0)).as_number(), Some(6.0));
    });
}

#[test]
fn an_exit_is_counted_and_the_code_goes_once_it_happens_too_often() {
    with_realm(|cx| {
        let (function, code_block) = prepared(cx, "(function (a) { return a + 1 })");
        install(cx, code_block, 0, State::default(), |_| {});
        call(cx, function, Value::int32(0));
        assert!(code_block.exits_at(0).contains(ExitKind::BadType));
        assert_eq!(code_block.reoptimizations(), 0);
        for _ in 0..200 {
            call(cx, function, Value::int32(0));
        }
        assert_eq!(code_block.reoptimizations(), 1);
        let executable = function.downcast::<JsFunction>().unwrap().executable();
        assert_eq!(
            executable.code_block(Specialization::Call).map(Gc::address),
            Some(code_block.address()),
        );
    });
}

fn tables(layouts: Vec<Layout>, registers: Vec<Register>, positions: Vec<Position>) -> Output {
    Output {
        code: assembler().finish().unwrap(),
        entry: 0,
        frame_size: 0,
        call_sites: Vec::new(),
        call_site_values: Vec::new(),
        exits: Vec::new(),
        exit_values: Vec::new(),
        handlers: Vec::new(),
        invalidation_points: Vec::new(),
        weak_references: Vec::new(),
        assumptions: Vec::new(),
        frames: Vec::new(),
        layouts,
        layout_registers: registers,
        positions,
    }
}

#[test]
fn a_state_is_sliced_by_the_chain_of_its_layouts() {
    let layout = |caller, first_register, register_count, resume| Layout {
        resume: Origin(resume),
        caller,
        first_register,
        register_count,
        recipe: NO_RECIPE,
    };
    let position = |frame, offset| Position {
        frame: FrameIndex(frame),
        offset,
    };
    let output = tables(
        vec![layout(NO_LAYOUT, 0, 2, 0), layout(0, 2, 3, 1)],
        (0..5).map(Register::local).collect(),
        vec![position(0, 10), position(1, 20)],
    );
    let value = ExitValue {
        place: Place::Constant(0),
        format: Format::Tagged,
    };
    let values = [value; 5];
    let frames = values::frames(&output, 1, &values);
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].frame, FrameIndex(0));
    assert_eq!(frames[0].resume, 10);
    assert_eq!(
        frames[0].registers,
        &[Register::local(0), Register::local(1)]
    );
    assert_eq!(frames[0].values.len(), 2);
    assert_eq!(frames[1].frame, FrameIndex(1));
    assert_eq!(frames[1].resume, 20);
    assert_eq!(frames[1].registers.len(), 3);
    assert_eq!(frames[1].values.len(), 3);

    let only = values::frames(&output, 0, &values[..2]);
    assert_eq!(only.len(), 1);
    assert_eq!(only[0].registers.len(), 2);
}

#[test]
fn a_value_takes_the_form_that_its_format_names() {
    let tagged = |bits, format| values::tagged(bits, format);
    assert_eq!(tagged(7, Format::Int32), Value::int32(7).to_bits());
    assert_eq!(
        tagged(-7i64 as u64, Format::Int32),
        Value::int32(-7).to_bits()
    );
    assert_eq!(tagged(7, Format::Int52), Value::int32(7).to_bits());
    let wide = 1i64 << 40;
    assert_eq!(
        tagged(wide as u64, Format::Int52),
        Value::double(wide as f64).to_bits()
    );
    assert_eq!(
        tagged(1.5f64.to_bits(), Format::Double),
        Value::double(1.5).to_bits()
    );
    assert_eq!(
        tagged(f64::NAN.to_bits() | 7, Format::Double),
        Value::NAN.to_bits()
    );
    assert_eq!(tagged(0, Format::Boolean), Value::FALSE.to_bits());
    assert_eq!(tagged(1, Format::Boolean), Value::TRUE.to_bits());
    let cell = 0x1234_5678u64;
    assert_eq!(tagged(cell, Format::Tagged), cell);
}
