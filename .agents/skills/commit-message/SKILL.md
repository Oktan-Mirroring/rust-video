---
name: commit-message
description: Use when drafting ANY git commit message. MANDATORY, load before writing the message. Conventional format, developer voice (no AI trailers, no mention of Claude / LLMs / sessions), commit-between-things cadence.
---

# Commit message style

`<type>(<scope>): <imperative summary>`. Types `feat` `fix` `refactor` `chore` `docs` `test` `perf` `build` `ci`. Scope is the module touched. Summary imperative, lowercase, no trailing period, under 72 chars.

```
feat(cli): add --dry-run flag to the sync command
fix(parser): reject trailing commas in object literals
```

Commit between things, not at the end. Each commit builds clean.

## Body

Optional. When present: one short paragraph or a few bullets carrying the one reason a reviewer cannot get from the diff. Body that inventories sub-changes or narrates the investigation is reasoning-chain narration in the wrong place, the diff already lists the changes. See [[no-slop]].

## Developer voice

Never `Co-Authored-By` for Claude or any AI. Never mention the assistant, the conversation, the prompt, this session. Log reads as one hand.

## Claims

Commit claiming faster runtime, lower allocation, smaller binary, or "no perf impact" on a hot path carries the measured delta, see [[perf-bench-claim]]. Before committing, delete staged comments restating the code ([[no-slop]], or [[rust-docs]] for Rust).
