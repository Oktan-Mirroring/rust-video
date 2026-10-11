# rust-video: agent doctrine

> rust-video is a production-grade safe Rust interface over realtime video
> handling: codec backends plus platform tiers behind one facade. This file is
> the quality bar.
> Detail lives in the README and in `.claude/skills/`. When something here grows
> into a how-to, move it there and link it below.

## Prime directives

Non-negotiable. A change that violates one is a regression.

1. **Zero warnings.** `just fmt` clean, `just check` (clippy, `-D warnings`)
   clean, `just test` green, `just build` clean, across every member. A lint
   means refactor, not `#[allow(...)]`.
2. **Windows, Linux, and Android are first-class.** No code, dependency, build
   step, or path handling may assume one platform. Gate OS-specific work behind
   `cfg(...)` with the other sides implemented, prefer cross-platform crates,
   and keep `std::path` use portable. Single-OS code is incomplete.
3. **A comment survives only a hard edge case the code cannot say by reading
   it.** Default state is deleted. A rationale (why this approach, what the fix
   was, how the code came to look this way) is forbidden. A hazard (ordering or
   timing constraint, wire/FFI layout match, platform quirk that bites a future
   editor who changes the code unknowingly) may survive, stated as present fact.
   Rustdoc on public items is separate and still mandatory: what the item is,
   never its history.
4. **Correctness over cleverness.** Idiomatic Rust, real error handling, no
   panics on reachable paths without a documented `# Panics`. `unsafe` lives at
   the FFI boundary only: the floor is `unsafe_code = "deny"`, opted back in
   per module with a soundness justification, never a blanket allow.
5. **Every dependency earns its place.** A crate joins only when a maintained
   crate solves the exact problem, hand-rolling costs more than a day plus upkeep,
   and the transitive tree stays small. It must build on Windows, Linux, and
   Android. Default is no.
6. **Design like a senior, the directive no lint can enforce.** Behaviour on
   types, not free functions; small single-purpose modules; the narrowest
   visibility that compiles; traits over concrete coupling; encapsulated
   internals. A warning-clean, fully formatted, green crate can still be badly
   designed. You are the gate.
7. **Model it, then confirm it.** A codec pass is a fixed count of fetches and
   arithmetic per block; that count does not change with the silicon, only the
   ratio does. Reason to the answer with the assumptions written down, then
   spend one cheap measurement confirming it. A number without its stated
   assumptions is unverified.
8. **Realtime or rejected.** The product is a safe interface over realtime
   video, not a pure-Rust showcase. A backend lands only when it sustains
   realtime throughput (1080p60 class) on its tier: threaded and SIMD where
   the format allows, measured, never claimed. A pure-Rust codec that cannot
   hold the frame budget stays out until it can; the facade carries the fast
   implementation (platform hardware, tuned C behind an isolated sys crate)
   instead. No toy backends behind a production API.

## Orientation

| Need | Home |
|---|---|
| the repo's shape and how to build it | [`README.md`](README.md) |
| the workspace, crates, and lint floor | [`Cargo.toml`](Cargo.toml) |
| any task's route | [`docs/AGENT-ROUTING.md`](docs/AGENT-ROUTING.md) |
| the scope rule | [`.claude/skills/realtime-scope/SKILL.md`](.claude/skills/realtime-scope/SKILL.md) |
| how to test | [`docs/testing/README.md`](docs/testing/README.md) |
| every command | `just --list` |

- Match surrounding code's idiom, naming, and comment density. The codebase
  reads as one hand.
- Prefer the smallest correct change. Verify with `just ci` and state the
  result plainly.
