---
name: rust-docs
description: "Use when touching any `.rs` file or writing any `///` or `//!`. MANDATORY, load with rust-architecture and rust-file-layout before the first edit. Rust delta on no-slop: rustdoc section order, public-API/panics/struct-field docs, struct-per-field legend fix."
---

# Rust docs

Rust specialization of [[no-slop]]. That skill owns the prune-pass test, the
forbidden/survives lists, session-leak, and the legend-gets-structure principle for any
language - read it first, it applies here unchanged. This file adds only what's Rust-specific:
`///`/`//!` syntax and mandates, rustdoc section order, and the struct-per-field legend fix.

## Comment voice

Terse and imperative. Drop articles, one point per line, no long winding sentences:
`// clamp to frame bounds`, not `// This will then clamp the value to the bounds of the frame`.
The `///` summary stays the noun phrase named below; every other comment reads as a directive.

## The prune pass, before every commit

Mandatory pre-commit pass on every commit, same standing as the simplify pass in
[[post-feat-passes]]. Re-read the staged diff and prune comments/docs **in place** before
committing, not a follow-up commit, part of preparing this one. Covers every comment kind in the
diff: Rust `///` `//!` `//`, and any markdown/TOML notes in the change. Apply
[[no-slop]]'s forbidden/survives lists to every comment in the changed files, including
lines you did not add: a file you touched is yours to clean, "pre-existing" is not an exemption.

The commit gate's denial is cleared by **doing** this pass, not by re-running the same commit
until the hash check passes. The gate's scan is a mechanical spot-check (`scan.py` from
[[no-slop]]), not the rule and not the full list.

After pruning, re-run the gate: `cargo fmt` + `cargo clippy --all-targets -- -D warnings` +
`cargo test`. Pruning comments must not change behaviour; a failing gate means an edit slipped
past the comment.

## Authoring Rust docs: `///` and `//!`

Default = no doc. When an item earns one, write a single short `///` saying what it IS, not what
it does - a noun phrase. Begin with the thing itself; never "The struct", "This function",
"Returns a ...". No `# Overview`. Plain language, no parenthetical filler, concise phrase form:
"Indentation string", not "String used for indentation". Describe external behaviour only, never
internals. Use `///` (or `//!` for module) on documented items including `pub(crate)` and
private; reserve `//` for in-body. Never reference prompt, task, user, session (see
[[no-slop]]).

### Always documented, no exceptions

- **Every public API item** gets a `///` stating what it IS in the abstract - minimalism never
  deletes this, and neither does the prune pass default-delete.
- **Every panic.** A fn that can panic carries a `# Panics` line naming the trigger, even when
  the panic looks obvious from the body.
- **Every struct field** gets its own short `///`. Keep the type-level doc abstract; never
  enumerate fields, variants, or methods in it. This is Rust's concrete fix for
  [[no-slop]]'s "a legend gets structure, not prose": a slice-of-tuples carrying a prose
  field legend becomes a named struct with a `///` per field in production code
  ([[rust-architecture]]); a throwaway or build-script constant can stay a tuple with a table or
  bullet legend.

### Banned beyond [[no-slop]]'s list, delete on sight

- WHY-implementation prose ("writes directly into a mapped buffer so no copy").
- Caller naming ("used by X", "called from the main loop").
- Test quotations.
- Prose lists of cases - use rustdoc sections or a struct instead.

`//!` blocks: three lines max.

### Rustdoc sections

Fixed order, blank line after each `#`, skip what does not apply, never reorder:

```
# Safety        # mandatory on every unsafe fn ([[lint-and-stop]] floor)
# Performance
# Panics        # mandatory whenever the fn can panic
# Errors        # mandatory when error variants carry meaning the signature does not
# Arguments     # `*` bullets, backticked names, one short line each
# Returns       # one line; omit when obvious from the type
# Examples
# Usage Notes
```

Read the fn body for explicit preconditions (`assert!`, `unwrap`/`expect` on inputs,
`if ... { return Err(...) }`) before writing or skipping a doc. Each = a contract the signature
does not express. Skip `?` lines that just propagate. Include `# Arguments` whenever a
multi-line doc takes parameters.

### Every fact lives in its owning section, nowhere else

Summary line states what the item IS, one noun phrase, nothing else. Every other fact routes to
the narrowest section that owns it. Never in the summary, never duplicated across sections,
never floated loose at the top:

- Return fact -> `# Returns`. Not "Returns the parsed config" in the summary.
- Parameter fact -> that parameter's `# Arguments` bullet.
- Failure condition -> `# Errors` (returns `Err`) or `# Panics` (panics).

A constraint scoped to one argument or to the return belongs on that argument's bullet or in
`# Returns`, never as a loose summary sentence - a summary-line constraint loses which input it
binds to.

```rust
// BAD: return fact + arg constraint melted into the summary, scope lost.
/// Parses input into a config, erroring when the path is empty.
fn load(path: &str) -> Result<Config, Error>

// GOOD: summary is the noun phrase; each fact bound to its section.
/// Configuration parsed from a TOML file.
///
/// # Arguments
///
/// * `path` - filesystem path; must be non-empty.
///
/// # Errors
///
/// Returns `Err` when `path` is empty or the file is not valid TOML.
fn load(path: &str) -> Result<Config, Error>
```

### Footgun exceptions: doc warranted even on private items

Rust's instance of [[no-slop]]'s footgun exception - when the *reason* the code looks this
way is ephemeral context a reader cannot grep:

- Magic ranges, hand-tuned thresholds (name the calibration source).
- Ordering that matters (init order, buffer/struct field offsets that must match a wire or FFI
  layout).
- External spec constraints encoded in Rust (alignment, layout, protocol field width): link the
  spec section.
- Sentinel values, magic offsets.

### BAD / GOOD

```rust
// BAD: implementation prose.
/// Serializes into a pre-sized buffer so there is no reallocation.
// GOOD: one noun phrase.
/// Wire-format encoder for outbound frames.

// BAD: caller naming + history.
/// Now computed once at init instead of per request.
// GOOD: what the thing IS.
/// Cached per-connection routing table.
```

## Self-check

Run [[no-slop]]'s self-check first, then the Rust-specific half:

1. Doc starts with the thing itself, not "The struct" / "This function" / "Returns", and no
   `# Overview`?
2. Fn can panic -> `# Panics` present. Public item -> doc present. Struct field -> own `///`
   present.
3. Summary line carries a return/argument/error/panic fact? Move it to its section. Multi-line
   doc takes parameters but has no `# Arguments`? Add it. Sections in fixed order, blank line
   after each `#`, `*` bullets, backticked names?
4. Comment terse and imperative, articles dropped? A comment enumerating a tuple's fields, a
   struct's layout, or a set of cases shaped as a table, bullets, numbered steps, or (in
   production code) a struct with a `///` per field, never an inline-prose legend?
