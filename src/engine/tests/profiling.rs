use bun_engine_inspector::profiling::*;
use bun_engine_standalone as _;
use bun_engine_text::{StringRef, Text};
use bun_engine_vm::heap::{HeapKind, Scope};
use bun_engine_vm::*;

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

fn script(text: &str) -> SourceCode {
    let text = StringRef::from_str(text);
    SourceCode::whole(SourceProvider::build(text, None, SourceType::Program).finish())
}

fn evaluate(cx: &mut Cx, text: &str) -> Value {
    evaluate_script(cx, &script(text), Value::UNDEFINED).unwrap()
}

/// What the function makes of the parsed text.
fn inspect(cx: &mut Cx, json: Text<'_>, function: &str) -> String {
    let (function, parsed) = (
        evaluate(cx, function),
        json::parse(cx, json.into()).unwrap(),
    );
    let result = cx.call(function, Value::UNDEFINED, &[parsed]).unwrap();
    result.to_string_view(cx).unwrap().text().to_string()
}

fn counted<'a>(text: &'a str, blocks: &[BlockRange]) -> Vec<(usize, &'a str)> {
    let mut blocks = blocks.to_vec();
    blocks.sort_by_key(|block| (block.start, -block.end));
    let text_of = |block: &BlockRange| &text[block.start as usize..=block.end as usize];
    (blocks.iter().map(|it| (it.execution_count, text_of(it)))).collect()
}

#[test]
fn blocks_nest_as_the_source_does() {
    with_realm(|cx| {
        let coverage = Coverage::of(cx);
        assert!(coverage.enable(cx));
        let text = "function f(x) { if (x) { return 1; } return 2; } f(1); f(0); f(0);";
        let source = script(text);
        evaluate_script(cx, &source, Value::UNDEFINED).unwrap();
        let id = source.provider.id();
        assert_eq!(
            counted(text, &coverage.blocks(id, Gaps::With)),
            [
                (1, text),
                (3, "(x) { if (x) { return 1; } return 2; }"),
                (1, "{ return 1; }"),
                (2, " return 2; "),
            ]
        );
        assert_eq!(
            counted(text, &coverage.blocks(id, Gaps::Without)),
            [
                (3, "(x) { if (x) "),
                (1, "{ return 1; }"),
                (2, " return 2; "),
                (3, "}"),
                (1, " f(1); f(0); f(0);"),
            ]
        );
    });
}

#[test]
fn a_body_that_has_not_begun_is_one_block() {
    with_realm(|cx| {
        let coverage = Coverage::of(cx);
        coverage.enable(cx);
        let text = "function* g(a) { if (a) () => 1; } var it = g(1);";
        let source = script(text);
        evaluate_script(cx, &source, Value::UNDEFINED).unwrap();
        let id = source.provider.id();
        let functions = || {
            let ranges = coverage.code_ranges(id).into_iter();
            let functions = ranges.filter(|range| range.kind == CodeRangeKind::Function);
            (functions.map(|it| (it.has_executed, &text[it.start as usize..=it.end as usize])))
                .collect::<Vec<_>>()
        };
        let generator = (true, "function* g(a) { if (a) () => 1; }");
        assert_eq!(
            counted(text, &coverage.blocks(id, Gaps::Without)),
            [
                (1, "(a) "),
                (0, "{ if (a) () => 1; }"),
                (1, " var it = g(1);")
            ]
        );
        assert_eq!(functions(), [generator]);

        evaluate(cx, "it.next()");
        assert_eq!(
            counted(text, &coverage.blocks(id, Gaps::Without)),
            [
                (1, "(a) "),
                (1, "{ if (a) "),
                (1, ";"),
                (1, " }"),
                (1, " var it = g(1);")
            ]
        );
        assert_eq!(functions(), [generator, (false, "() => 1")]);
    });
}

#[test]
fn code_goes_on_counting_after_the_counts_are_dropped() {
    with_realm(|cx| {
        let coverage = Coverage::of(cx);
        assert!(coverage.enable(cx) && !coverage.enable(cx));
        evaluate(
            cx,
            "function* g() { for (;;) yield 1; } var it = g(); it.next()",
        );
        assert!(!coverage.disable(cx) && coverage.disable(cx) && !coverage.disable(cx));
        cx.heap.collect_and_sweep(Some(Scope::Full));
        evaluate(cx, "it.next()");
        assert!(coverage.source_ids().is_empty());
    });
}

const EDGES_FROM_HOLDER: &str = "({ nodes, nodeClassNames, edges, edgeTypes, edgeNames }) => {
    const classes = new Map, found = [];
    for (let i = 0; i < nodes.length; i += 4) classes.set(nodes[i], nodeClassNames[nodes[i + 2]]);
    for (let i = 0; i < edges.length; i += 4) {
        const [from, to] = [classes.get(edges[i]), classes.get(edges[i + 1])], type = edgeTypes[edges[i + 2]];
        if (from === 'Held' || to !== 'Held') continue;
        found.push([from, type, type === 'Index' || type === 'Internal' ? edges[i + 3] : edgeNames[edges[i + 3]]].join(' '));
    }
    return found.sort().join();
}";

#[test]
fn every_reference_is_an_edge() {
    with_realm(|cx| {
        evaluate(
            cx,
            "class Held {} class Holder { property = new Held }
             var variable = new Held; let lexical = new Held;
             var keep = [new Held, new Holder, (captured => () => captured)(new Held)];
             with (new Held) keep.push(() => 1);",
        );
        let profiler = HeapProfiler::of(cx);
        let json = profiler.snapshot(&mut cx.into(), SnapshotKind::Inspector, &|_| true);
        assert_eq!(
            inspect(cx, json.unwrap().text(), EDGES_FROM_HOLDER),
            "Array Index 0,GlobalObject Variable lexical,GlobalObject Variable variable,Holder Property property,\
             JSLexicalEnvironment Variable captured,WithScope Internal 0"
        );
    });
}

#[test]
fn identifiers_last_as_long_as_their_cells() {
    const IDENTIFIER: &str = "({ nodes, nodeClassNames }) => {
        for (let i = 0; i < nodes.length; i += 4) if (nodeClassNames[nodes[i + 2]] === 'Once') return nodes[i];
    }";
    with_realm(|cx| {
        let profiler = HeapProfiler::of(cx);
        let identifier = |cx: &mut Cx| {
            let json = profiler.snapshot(&mut cx.into(), SnapshotKind::Inspector, &|_| true);
            inspect(cx, json.unwrap().text(), IDENTIFIER)
        };
        evaluate(cx, "class Once {} var once = new Once; 0");
        assert!(!profiler.has_snapshot());
        let first = identifier(cx);
        assert_eq!(identifier(cx), first);
        let first = first.parse().unwrap();
        assert!(profiler.has_snapshot() && profiler.cell_with_identifier(cx, first).is_some());
        assert!(profiler.cell_with_identifier(cx, u32::MAX).is_none());
        profiler.clear_snapshots();
        assert!(!profiler.has_snapshot() && profiler.cell_with_identifier(cx, first).is_none());
    });
}

#[test]
fn roots_say_why_they_are() {
    const REASONS: &str = "({ nodes, nodeClassNames, roots, labels }) => {
        const classes = new Map, found = [];
        for (let i = 0; i < nodes.length; i += 7) classes.set(nodes[i], nodeClassNames[nodes[i + 2]]);
        for (let i = 0; i < roots.length; i += 3) found.push(classes.get(roots[i]) + ' ' + labels[roots[i + 1]]);
        return found.filter(it => /^(Both|Handle|Follower|GlobalObject)/.test(it)).sort().join();
    }";
    with_realm(|cx| {
        let both = evaluate(cx, "new (class Both {})");
        cx.heap.protect(both);
        let _handles = [both, evaluate(cx, "new (class Handle {})")].map(|it| Strong::new(cx, it));
        evaluate(
            cx,
            "var map = new WeakMap([[globalThis, new (class Follower {})]])",
        );
        let profiler = HeapProfiler::of(cx);
        let json = profiler.snapshot(&mut cx.into(), SnapshotKind::GcDebugging, &|_| true);
        assert!(!profiler.has_snapshot());
        assert_eq!(
            inspect(cx, json.unwrap().text(), REASONS),
            "Both ProtectedValues,Follower Output,Handle StrongHandles"
        );
    });
}

#[test]
fn what_is_left_out_has_no_edges() {
    with_realm(|cx| {
        evaluate(
            cx,
            "class Held {} var keep = [new Held]; var variable = new Held",
        );
        let includes = |cell: Gc<JsCell>| !cell.is::<JsArray>();
        let json =
            HeapProfiler::of(cx).snapshot(&mut cx.into(), SnapshotKind::Inspector, &includes);
        assert_eq!(
            inspect(cx, json.unwrap().text(), EDGES_FROM_HOLDER),
            "GlobalObject Variable variable"
        );
    });
}

#[test]
fn both_encodings_of_the_v8_format_say_the_same() {
    const NAMES: &str = "({ snapshot, nodes, edges, strings }) => {
        let counted = 0; const named = [];
        for (let i = 0; i < nodes.length; i += 7) {
            for (let e = counted * 3; e < (counted + nodes[i + 4]) * 3; e += 3) {
                if (strings[nodes[i + 1]] !== 'Names' || nodes[i] !== 3 || edges[e] !== 2) continue;
                const to = edges[e + 2];
                named.push([strings[edges[e + 1]], snapshot.meta.node_types[0][nodes[to]], JSON.stringify(strings[nodes[to + 1]])].join(' '));
            }
            counted += nodes[i + 4];
        }
        return [counted === snapshot.edge_count, nodes.length === 7 * snapshot.node_count, ...named.sort()].join();
    }";
    with_realm(|cx| {
        evaluate(
            cx,
            r#"var part = "\ud800é"; var names = new (class Names {
                 a = part; b = "long enough not to be copied when it is added to" + part; c = Symbol("s"); d = /x/g; e = function named() {}; f = [];
               })"#,
        );
        let profiler = HeapProfiler::of(cx);
        let expected = r#"true,true,a string "\ud800é",b concatenated string "long enough not to be copied when it is added to\ud800é",c symbol "Symbol(s)",d regexp "/x/g",e closure "named",f object "Array""#;
        let text = profiler.v8_snapshot(&mut cx.into()).unwrap();
        assert_eq!(inspect(cx, text.text(), NAMES), expected);
        let bytes = profiler.v8_snapshot_bytes(&mut cx.into()).unwrap();
        let text = StringRef::try_from_utf8(&bytes).unwrap().unwrap();
        assert_eq!(inspect(cx, text.text(), NAMES), expected);
    });
}

#[test]
fn cells_are_counted_by_kind() {
    with_realm(|cx| {
        let count = |cx: &Cx, name: &str| {
            let counts = count_cells(cx);
            let found = counts.iter().find(|(it, _)| it.as_str() == name);
            found.map_or(0, |found| found.1)
        };
        evaluate(cx, "var keep = [async () => {}]; (function () {})()");
        let before = count(cx, "AsyncFunction");
        evaluate(cx, "keep.push(async () => {}, async function () {})");
        assert_eq!(count(cx, "AsyncFunction"), before + 2);
        assert_eq!(count(cx, "FunctionCodeBlock"), 1);
        assert_eq!(count(cx, "UnlinkedFunctionCodeBlock"), 1);
    });
}
