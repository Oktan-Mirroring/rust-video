---
name: rust-file-layout
description: Use when creating ANY new `.rs` file or adding a top-level item. MANDATORY, load with rust-architecture and rust-docs before the first edit. Item ordering, `use`-group conventions, ZST encapsulation, no free-fn soup.
---

# Rust file layout

## Top-level item order

Alphabetical within each block, fixed order:

1. `mod` declarations
2. `use` statements (groups separated by blank line)
3. `const` / `static`
4. `trait`
5. `enum`
6. `struct` + its `impl` blocks immediately after

Same ordering repeats inside any scope (module body, `impl`, fn body) when that scope introduces its own definitions. Do not flatten to merge a scope's items with top-level.

## `use` groups: alphabetical within each, blank line between

1. `std::*`
2. `crate::*`, `super::*`, `self::*`
3. External crates

## ZST encapsulation: no free-fn soup

Logic lives behind named structures, not free-function soup. Either the cluster owns its file with one or more structs inside (`Parser` in `parser.rs`), or it folds into the file that already uses it.

- Helpers do not sit at module top level.
- The struct IS the entry point (`Parser::new`, `Parser::parse`).
- Free fns at top of file = only the crate's public entry points where the name IS the contract (`main`, a `lib.rs` re-export).
- Never wrap a method in a second-name free fn.

File's table of contents = list of structs.

## Constants

- Used by exactly one cluster -> associated `const` on that cluster.
- Module-level `const` only when two structs in the same file share it (doc must name consumers, see [[rust-docs]]).
- Layout constants that must match an external wire or FFI layout (offsets, alignment, field width) live next to the type they describe, doc'd against the spec.

## Cost

ZST namespaces are zero-cost. Associated fns inline like free fns, no vtable, no fn-ptr indirection. The binary stays small.
