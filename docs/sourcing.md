<!-- source: hand-written for rust-video; tool: hand -->
<!-- date: 2026-10-11; author: rust-video -->
# Sourcing

Every external value in this tree carries its tier where it is written. The
tier decides what gets built.

## Tiers

| Tier | Meaning | May encode |
|---|---|---|
| 1 | spec states it, or measured with the method recorded | the value, with its source at the call site |
| 2 | independent sources agree on shape, none publishes the figure | a tunable default, never a precise constant |
| 3 | unpublished and unverifiable | nothing; build the control that lets the user supply it |

## Provenance headers

Every file under `docs/specs/` opens with a header naming the spec, its
version, and the fetch date. No header, no table.

## Two copies, one gate

Level and cap numbers live twice: the human map in `docs/specs/*.md` and the
machine map in `rust-video-caps` const tables. Both cite the same spec
section, both list values in the same column order, and
`scripts/spec-check.py` (wired into `just ci`) diffs their normalized number
sequences. A drifted row fails the gate.

## Refusals

- No unsourced numbers. A value without a spec section does not land.
- No invented constants. A gap stays labeled `unmapped`, never filled.
- No restated numbers outside the two gated copies. Docs narrate structure;
  prose never repeats a figure the tables hold.
- Tier 3 stays out of `const`. Hardware caps the vendor does not publish are
  runtime queries, not constants.
