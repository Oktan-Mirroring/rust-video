<!-- source: hand-written for rust-video; tool: hand -->
<!-- date: 2026-10-11; author: rust-video -->
# Sourcing

Every external value in this tree carries its tier where it is written. The
tier decides what gets built.

## Tiers

| Tier | Meaning | May encode |
|---|---|---|
| 1 | spec states it, or measured with the method recorded | the value, with its source at the call site |
| 2 | independent sources agree on shape, none publishes the figure | a tunable default |
| 3 | unpublished and unverifiable | build the control that lets the user supply it |

## Provenance headers

Every file under `docs/specs/` opens with a header naming the spec, its
version, and the fetch date. Files without headers carry no tables.

## Two copies, one gate

Level and cap numbers live twice: the human map in `docs/specs/*.md` and the
machine map in `rust-video-caps` const tables. Both cite the same spec
section, both list values in the same column order, and
`scripts/spec-check.py` (wired into `just ci`) diffs their normalized number
sequences. A drifted row fails the gate.

## Refusals

- Every number cites its spec section.
- Gaps stay labeled `unmapped`.
- Numbers live only in the two gated copies. Docs narrate structure;
  prose repeats no figure the tables hold.
- `const` holds no Tier 3 values. Hardware caps the vendor leaves
  unpublished are runtime queries.
