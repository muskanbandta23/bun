# The engine

JavaScript and WebAssembly execution for Bun, in Rust. Each folder here is a crate named `bun_engine_<folder>`; this folder itself is `bun_engine`, which assembles them.

## Crates, bottom up

| Crate                                           | Owns                                                                                               | `unsafe`     |
| ----------------------------------------------- | -------------------------------------------------------------------------------------------------- | ------------ |
| `text`                                          | `StringImpl`, atoms, symbols, `Text` views, `StringBuilder`, hashing                               | string core  |
| `unicode`, `number`, `bigint`, `date`           | character data; number ↔ text and numeric semantics; digit arithmetic; calendars, zones, date text | forbidden    |
| `regexp`, `regexp_jit`                          | pattern parser, compiler, backtracking interpreter; machine code for patterns                      | forbidden    |
| `syntax`                                        | lexer, parser, scope analysis, early errors                                                        | forbidden    |
| `bytecode`                                      | instruction set, generator, verifier, liveness, serialized form                                    | forbidden    |
| `asm`, `codegen`                                | assemblers and macro assembler; SSA IR down to machine code                                        | forbidden    |
| `wasm_format`, `wasm_compiler`                  | decoding and validation of modules; machine code for functions                                     | forbidden    |
| `exec_memory`                                   | executable pages, linking, patching                                                                | yes          |
| `wasm_runtime`                                  | linear memories, tables, instances, installing and tiering of code, faults                         | yes          |
| `macros`, `vm`                                  | heap and collector, object model, realms, calls, errors, modules, promises, caches, operations     | yes          |
| `interpreter`                                   | bytecode handlers and the stubs that enter, call and leave frames                                  | yes          |
| `builtins`, `intl`, `temporal`                  | the standard library                                                                               | forbidden    |
| `baseline`, `optimizer`, `wasm`                 | compilers (safe code that produces bytes) and the code that installs and enters what they produce  | install only |
| `inspector`                                     | debugger, profilers, heap snapshots, protocol                                                      | sampling     |
| `embed`                                         | C ABI and C++ headers for Bun's bindings                                                           | yes          |
| `shell`, `standalone`                           | `engine-shell` and the native libraries it and the crates' tests link. Not part of Bun             |              |

A crate below `vm` knows nothing of the garbage collector: it is plain Rust over slices and owned data. A crate calls upwards only through a table of hooks that `bun_engine` fills in.

## Build and test

```sh
cargo build -p bun_engine_shell --release     # target/release/engine-shell
cargo test -p bun_engine_regexp               # a test file names `bun_engine_standalone` to link the native libraries
cargo clippy -p bun_engine_<crate> --no-deps && cargo fmt -p bun_engine_<crate>
cargo check -p bun_engine_<crate> --target aarch64-apple-darwin   # and x86_64-pc-windows-msvc: `cfg`-gated code is not checked otherwise
```

## Rules

- **Safe by default.** A crate with `#![forbid(unsafe_code)]` keeps it. Elsewhere `unsafe` lives in small modules that export a safe interface whose types carry the invariant, and each block says why it holds.
- **Input never panics.** Scripts, patterns, modules, cached code and protocol messages are hostile. Sizes are checked or widened before arithmetic, an allocation whose size a script picks is fallible and ends in a thrown error, and recursion over input checks the stack. `panic!`, `unwrap` and indexing are for invariants of the engine.
- **Text is Latin-1 or UTF-16**, never UTF-8. Code over characters is generic over `CodeUnit` and entered through `with_text!`.
- **SIMD is highway**: a kernel in `src/jsc/bindings/highway_*.cpp` behind a safe wrapper in `src/highway`. No `std::simd`, no `core::arch`. Machine code that the compilers emit is not bound by this.
- **Compilers are pure.** A compiler reads a snapshot taken on the JavaScript thread and returns bytes and tables. It does not touch the heap, so it runs on any thread. What it assumed is checked again when the code is installed.
- **Data layout over pointers** in compilers and parsers: arenas and `u32` indices, no allocation per node.
- **Only what Bun uses.** No option, hook or function without a caller.
- **Comments** say what the code cannot, in a line. Cite the specification (ECMA-262, ECMA-402, WebAssembly, WHATWG, UAX) for what follows from it.

## Values and frames

A `Value` is 64 bits. Int32: `0xfffe_0000_xxxx_xxxx`. Double: its bits plus 2^49, NaN canonical. Cell: a pointer with the top 15 bits and bit 1 clear. `null` 0x02, `false` 0x06, `true` 0x07, `undefined` 0x0a. Zero is no value: `Option<Value>` is a slot that can be empty (a hole, a binding before its initialization), and `JsResult<Value>` is one register, zero when an exception is pending in the `Vm`.

Frames of every tier are on the native stack, chained through the frame pointer:

```
fp + 48 + 8n   argument n
fp + 40        this (new.target in a construct call)
fp + 32        argument count including this (low half), call site (high half)
fp + 24        callee
fp + 16        code block
fp +  8        return address
fp +  0        caller's fp
fp -  8(n+1)   local n
```

The caller builds the callee's header and arguments at the bottom of its own frame and restores the stack pointer after the call. A callee entered with fewer arguments than parameters moves the frame down and pads it. No register survives a call between JavaScript functions except the pinned ones, so unwinding restores none:

| Pinned         | x86-64 | AArch64 |
| -------------- | ------ | ------- |
| frame pointer  | rbp    | x29     |
| `&Vm`          | r13    | x21     |
| number tag     | r14    | x22     |
| not-cell mask  | r15    | x23     |

Native functions and run-time operations use the C calling convention (System V on Windows too).

## The heap

Cells do not move. The collector is generational mark and sweep, finds roots by scanning the native stack conservatively, and traces cells precisely. So a `Gc<T>`, a `Value` or a `&T` of a cell is safe in a local, an argument or a struct on the stack, and in a field of another cell. It is **never** put in memory of the Rust allocator (`Vec`, `Box`, `Rc`, a hash map, a boxed closure, a static): use the containers of `vm::heap` (`ValueVec`, `Strong`, `Weak`, traced fields).
