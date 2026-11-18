# rootcheck

The engine's collector finds references to cells on the native stack, in memory of `RootAlloc`, and in what cells and the `Vm` show it (`src/engine/CLAUDE.md`, "The heap"). A reference that is anywhere else keeps nothing alive. rustc has no way to say so; this compiler driver checks it on the MIR.

```sh
rustup component add rustc-dev llvm-tools     # once for each toolchain
bun run rust:rootcheck                        # bun_engine_vm and the crates that name it as a dependency, tests included
bun run rust:rootcheck -p bun_engine_builtins # other arguments go to `cargo check`
bun run rust:rootcheck --test                 # tests/*.rs
```

It works in `target/rootcheck` and is incremental like `cargo check`.

## Terms

- A type **refers** to cells if it is not `GcFree`. A type parameter is judged where it is instantiated, and a trait object where it is made.
- A type **hides** references if it owns memory, from an allocator other than `RootAlloc`, that holds a type which refers. There is no list of containers. A type owns what it has a raw pointer and a `PhantomData<T>` for, or a raw pointer, a destructor and no lifetime. The memory of `RootAlloc` is looked into: `RootVec<Vec<Value>>` hides.

## Rules

| Rule     | Reports                                                                                                                          |
| -------- | -------------------------------------------------------------------------------------------------------------------------------- |
| `hidden` | a local, parameter or temporary whose type hides, unless it is only passing through (below)                                      |
| `hidden` | a field whose type hides, unless the type around it is a cell, `Trace`, a `Service`, `GcFree` by hand, or a part of one of these |
| `hidden` | the `Init` of a cell that hides: `Vm::allocate` may collect                                                                      |
| `hidden` | a safe function of a crate below `vm` that lends out a type which refers from memory whose type does not, as an arena does       |
| `static` | a static or thread-local whose type refers                                                                                       |
| `erased` | `&Cell` made `&dyn Trait`                                                                                                        |
| `guard`  | `ManuallyDrop` around a guard such as `StringView`                                                                               |
| `allow`  | an opt-out that is misplaced, has no reason or allows nothing                                                                    |

References to a container are fine: whoever holds it is checked.

**Passing through.** The state of a cell is made of such containers, so they have to get there. A value may be moved, dropped, matched on and asked for its size (`len`, `is_some`), provided that nothing may collect before it is moved on. It may not be borrowed, looked into, leaked or made a trait object. So `state.list = vec![value]`, `state.done.push(state.queue.pop_front()?)` and `root_vec.extend(mem::take(&mut state.list))` pass, and `for value in mem::take(&mut state.list)` does not.

**Generic functions** are checked as declared and again with each set of types that they are called with, in whatever crate they are. Code from outside the repository is held to less: it may not make such a value a trait object or hold it while something may collect. That finds the scratch memory of `sort_by` and `splice` when the callback allocates.

**May collect** follows calls to `Heap::collect`. A call that cannot be followed may collect if it is given a `Vm`, `Cx`, `Heap`, trait object or `unsafe fn`.

`unsafe` code is out of scope: casts, `transmute`, addresses kept as integers.

## Opting out

```rust
#[cfg_attr(rootcheck, rootcheck::allow("collection is deferred"))]
```

On a function (what is in it and what it instantiates), a static, a field or a type (it does not hide; a container of it still does). Only where the `unsafe_code` lint is allowed, and like `unsafe` it is a promise.

## Tests

Each file in `tests/` is a crate that is compiled against the real `bun_engine_vm`. A line that ends in `//~ rule: text` has a finding that says that, and no other line has any.

## A new toolchain

The compiler's interface changes with every nightly. Its source comes with `rustc-dev`: `$(rustc --print sysroot)/lib/rustlib/rustc-src`.
