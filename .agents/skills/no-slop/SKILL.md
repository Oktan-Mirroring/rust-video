---
name: no-slop
description: "Use when writing or editing ANY text a human reads: code comments, doc-comments, commit bodies, README/docs, articles, marketing copy, any language. MANDATORY, load before writing, never as a cleanup pass. Single source of truth for cutting AI residue: narration, fabrication, hollowness, uniformity. Base skill for rust-docs and human-voice."
---

# Decisions, not narration

One defect, many shapes: text performing writing instead of stating something real. Applies to every comment syntax, docstring, commit body, README, article, any language. [[rust-docs]] adds Rust `///`/`//!` conventions, [[human-voice]] adds long-form structure. Both build on this file.

`scan.py` beside this skill greps comments and prose for the patterns below plus structural checks (chain density, oversized blobs, duplicated explanations, uniformity, hallucinated tool tokens): `python3 scan.py <paths> [--json] [--fail-on-hit]`. Heuristic, not verdict. Project hook shells out to it instead of re-implementing the regexes.

Scope is retroactive: any comment or prose read while editing gets the same test, including text you did not write. Out of scope to fix means naming file and line, not walking past it. Stricter project doctrine beats the "Survives" list.

A rewrite is graded against the whole list below, not only the category that flagged it. Reordering a clause without naming a real fact is still Hollow. Cutting a hollow phrase but leaving two clauses restating the same fact is still Restating. Read the after fresh against every Cut entry, not just whichever one the original line tripped.

## The one test

Could a competent reader reconstruct this from the code beside it? Yes, delete. No, keep only if it states a decision, constraint, footgun, spec link, or verifiable fact.

## Cut

- **Restating**: line, name, params, types; section banners; name paraphrased back as a sentence; how the language or platform works; heading whose parenthetical does the naming.
- **Oversized blob**: 4+ lines without multiple distinct facts. Collapse. Still long after that, it was dense, not padded.
- **Reasoning-chain narration**: sentences chained by so/since/because/which means/therefore, walking observation to conclusion. State the conclusion alone. One connective counts. Decision needing its reason takes a colon: `// retry 3x: upstream rate-limiter resets on a ~1s window`.
- **Task, session, history reference**: "as requested", "for this fix", "as discussed", "we found that", "now does X instead of Y", "previously", "no longer", "since vX", "added for Z feature", "handles issue #123", "as an AI", ownerless TODOs, research-process narration ("could not be located", "no information available"). Belongs in the commit, where it dies naturally.
- **Fabrication**, the sharpest line, dishonest not sloppy: no unsourced statistics, no invented case studies, dates, attributions. Quote is exact, never paraphrased-as-quoted. User-authored prose stays verbatim.
- **Hollow statements**: importance, difference, scale with no checkable detail. Name the number, date, mechanism, file, or cut it. Contrast that never names its mechanism is the same defect.
- **Architecture apologia**: reassuring the reader the structure is fine instead of stating it. Markers: reassurance adjectives on the structure itself (optional, standalone, clean, decoupled, independent, flexible, only, merely, just) plus a second clause proving the reassurance, usually past a semicolon or colon. Test: delete the reassurance; the surviving structural fact (reads/produces/contains + the list) says everything or the sentence had nothing. BAD: "The index is an optional downstream stage; every earlier stage stands alone." GOOD: "Three stages: parse, validate, index. The index reads validated output."
- **Backwards sentence**: point sits in the tail clause, reader holds the sentence open to the end to learn what it means, then re-reads the front. Qualifier landing after other listed items with no clear anchor (`Native resolution, 144fps, matched to your monitor's refresh rate`: matched to which, the fps or the resolution? Pair cause with the number it causes, first). Verb with no named object, riding on vague adverbial filler standing in for the missing mechanism (`then converts everything it does along the way`: converts from what, to what, costing what? Name it or cut the clause). Stop at each comma while reading: an earlier clause that only resolves once the last one lands, or a last clause built from connective words with nothing real inside, gets rewritten forward, real relationship first. Reordering clauses or bolting on a colon is not the fix on its own. Object filling the gap must be a real, specific, checkable fact, not another vague noun phrase standing in its place (`converting every action the game takes` still names nothing, same hollowness in new words). Colon or connective joining two clauses has to mean one actually explains the other, never two independent facts wired together as if it did (`spread across a display 20 times the size: same game, same pixels`, the size explains nothing about the sameness, they're unrelated facts forced into one). Two clauses restating one fact (`same game, same pixels`, both saying nothing changed) get cut to one, never just reordered around each other.
- **Context poisoning**: opening mid-stream, term used before defined, context assumed from a sibling file, mind-reading ("most users think", "obviously"), one observed case asserted as general law.
- **AI-tell prose**: stock transitions (furthermore, moreover, that being said, it is worth noting); AI verbs (delve, leverage, utilize, facilitate, foster, bolster, underscore, streamline); factless intensifiers (significantly, dramatically, truly); weasel phrasing ("helps ensure", "can potentially"); **em dashes and en dashes, no exception**; over-symmetric bullets, rule-of-three padding, "it is not X, it is Y" past twice, same tic in noun-phrase form ("X, not Y") outside a real comparison; performative urgency with no named consequence; sentence opening "Whether you're"; defensive denial rebutting an objection nobody raised ("not invented for the site", "genuinely enforced"); talking point restated in new words as if that supported it; hallucinated tool artifacts (`oaicite`, `contentReference`, `turn0search0`), always delete.

## Hold the shape

- **Verbatim**: code blocks, paths, URLs, error messages, API names byte-for-byte. Paraphrasing a technical literal is corruption.
- **Attribution**: nothing unverifiable stands unattributed. Contested claim about a real person or org gets re-attributed at every occurrence.
- **Terminology**: one concept, one term, across a file and across a batch. Record a non-obvious choice once instead of re-deciding it silently.
- **Register**: confident-neutral. Fact and consequence, once. No fear-mongering, no condescension, no moralizing. Reader is a peer.
- **One canonical copy**: full decision lives on the thing that owns it, every other site gets a one-line pointer. Near-verbatim block in two places is the signal to consolidate.
- **Legend gets structure**: several named items mapped to meanings become a table, an enum, a doc-per-field. Never inline prose.
- **No design essay on one token**: [redacted]
- **Fix at the defect depth**: reword for word choice, restructure for a broken skeleton, rewrite for broken bones. Never preserve a broken structure: an earlier smaller edit landing first is not a reason to keep it.

## Survives

Decision the code cannot show (conclusion, not path), constraint no grep recovers, footgun, spec link at the call site, verifiable fact traceable to repo or cited source. Half-real, half-narration gets cut to the real part.

## Self-check

1. First and last sentence alone carry the point? Everything between was chain.
2. Banned phrasing, verb, intensifier, em dash, en dash?
3. Task, session, ticket, version, research-process reference?
4. Invented number, quote, date, case, attribution?
5. Every claim ends on a checkable fact, every contrast names its mechanism?
6. Zero-context reader follows on first read?
7. Restates a name, params, types, adjacent declaration?
8. Same explanation in more than one place?
9. Every sentence in a long block carries a fact the others do not?
10. Reads as a person recording something real, not a template performing completeness?
11. Any clause whose real point lands only at the end, or a verb with no named object riding on filler?
12. If #11 fired: fix names something real, not another vague noun? Cut the restated fact instead of just reordering it behind a new colon?
