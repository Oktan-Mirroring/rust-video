---
name: rust-architecture
description: "Use when writing or editing ANY `.rs` file, even a one-function change. MANDATORY, load with rust-file-layout and rust-docs before the first edit. Module, type, trait, API design: visibility, responsibility splits, trait vs concrete, error types, SOLID-as-Rust, API Guidelines as truth."
---

# Rust architecture

Design the boundary before the body. A type, module, or trait carries one responsibility behind the narrowest surface that compiles. Default to private; widen only for a real caller. Follow the [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/), not habit (see [[external-docs-as-truth]] for pinning the link at the call site).

## SOLID, mapped to Rust

- **Single responsibility.** One module, one capability; one struct, one reason to change. A type that grows a second reason splits into two.
- **Open/closed.** Extend behaviour by adding a trait impl, not by editing a `match` over a closed enum that callers outside the crate cannot reach.
- **Liskov.** A trait impl honours the trait's contract: no panicking impl behind a method the trait implies is total, no surprise side effect.
- **Interface segregation.** Small focused traits. An impl that must supply methods it does not use means the trait is fat - split it.
- **Dependency inversion.** Boundaries depend on a trait or generic, not a concrete collaborator. Inject the collaborator; do not reach for a global or construct it inline.

## Scoping and visibility

- Private by default. `pub(crate)` before `pub`. Every `pub` item is a contract you maintain across versions.
- A module is a boundary, not a drawer of loose functions. It owns a coherent capability and exposes a narrow face; internals stay `pub(self)` / private. Free-function soup is the failure mode, see [[rust-file-layout]].
- One file growing two unrelated capabilities = two modules.

## Encapsulation

- Fields private. Construct through `new` / a builder that enforces the invariant; expose behaviour, not state.
- Newtype to give a primitive meaning and hide its representation: `UserId(u64)`, not a bare `u64` threaded through signatures.
- Never return an internal type across a module boundary; that leaks the abstraction and freezes the internals.

## Behaviour on types, not free functions

This is production code. Default: data and the behaviour over it live on a struct via `impl`, not in free functions that pass the data around. A module that is a pile of functions all taking the same value as their first argument is that value's `impl` written wrong. Free-function soup is the failure mode ([[rust-file-layout]]); SOLID and DRY are the bar, not aspirations.

- **DRY.** Shared logic is one function or type. Two near-identical blocks - two poll loops, two error paths, a copy-pasted helper across crates - collapse into one. Copy-paste with a tweak is a defect, not a style choice.
- **A stateful resource is a struct.** A fallible constructor (`Type::open(..) -> Result<Self, Error>`) acquires and validates it, `Drop` releases it, methods expose behaviour, fields stay private. `let x = Type::open(..)?;` then call methods - never a `wait()` + `use(handle)` pair of free functions.
- **Unsafe/FFI is one case of this.** A newtype owns the raw pointer / OS handle / `*_sys` type and exposes safe methods; the raw thing never crosses the module boundary, and `unsafe` stays in small `// SAFETY:`-justified blocks inside the wrapper, never on the caller.

```rust
// BAD: free functions threading the same value (or a raw handle) around.
fn parse(raw: &str) -> Ast { ... }
fn render(ast: &Ast) -> String { ... }

// GOOD: behaviour on the type; construct-or-error, own the state, expose methods.
struct Document(Ast);
impl Document {
    fn parse(raw: &str) -> Result<Self, ParseError> { ... }
    fn render(&self) -> String { ... }
}
```

### A fieldless struct is not a type

Zero-sized struct whose methods never take `self` is a namespace with a dot in it. Free functions survive, wearing a prefix. Test: **does anything ever construct it?** Nothing does, nothing was encapsulated.

```rust
// BAD: the free functions are intact, the call sites just got longer.
struct Fixtures;
impl Fixtures {
    fn zones() -> Zones { ... }
    fn tap_control() -> Control { ... }
}
```

Counting free functions to zero this way is worse than leaving them, because the zero reads as done and hides that the pass never happened.

Find the real owner instead:

- **State threaded by hand on every call** is the field. Same three arguments at every call site means a struct holding those three and methods taking `&self`.
- **Behaviour about one value** goes on that value's type; on a foreign or `std` type, an extension trait (`impl Ext for [Action]`) is the honest shape.
- **Genuinely stateless helper** (a codec, a pure conversion) stays a free function in a module named for it, or becomes `From`/`TryFrom`. Wrapping it in a ZST buys nothing.
- **Compile-time constants behind accessors** become associated consts on the type that reads them.

A ZST earns its place only when the grouping itself is the contract, as a type-state marker or a generic parameter. Reached for more than once in a file, it is the anti-pattern, not the exception.

## Types over strings

- Model a closed set as an `enum`, not a `&str` matched at call sites. No stringly-typed APIs.
- Replace a bare `bool` parameter with a two-variant enum when the call site cannot read what `true` means (boolean blindness).

## Errors

- A library error is a typed `enum` implementing `std::error::Error`; no `String` errors, no `unwrap`/`expect` on a reachable path. Propagate with `?`. A panic on a reachable path needs a documented `# Panics`, see [[rust-docs]].
- Prefer `From` / `TryFrom` and the standard conversion names (`as_`, `to_`, `into_`) over ad-hoc constructors; derive the standard traits the guidelines list.

## Self-check

1. Does each module and type carry exactly one responsibility?
2. Is every `pub` item actually reached from outside its module? Else narrow it.
3. Are invariants enforced at construction, with fields private?
4. Any `&str`/`bool` standing in for an enum? Any `String` error or reachable `unwrap`?
5. Does any public signature leak an internal type?
6. Any fieldless struct whose methods never take `self`? That is a namespace; find the owner.
