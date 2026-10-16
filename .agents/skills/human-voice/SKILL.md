---
name: human-voice
description: "Use when writing or editing long-form prose: articles, README, docs/*.md, marketing copy, any multi-paragraph piece. MANDATORY, load with no-slop before writing. Long-form delta: lede craft, headings, tense-accuracy for unlaunched claims, uniformity self-checks, cross-batch consistency."
---

# Human voice

Long-form specialization of [[no-slop]]. That skill owns fabrication, hollow
statements, context-poisoning, attribution, terminology consistency, register, and every AI-tell
- read it first, it applies here unchanged. This file adds only what needs multiple paragraphs to
matter: lede craft, heading quality, pacing, and consistency across a batch of pieces.

## Lead with one true specific

Open on the single most concrete fact, never a rhetorical question, a generic framing sentence,
or an invented anecdote. The lede states what/who/when/where; a nut graf right after explains why
it matters. Inverted pyramid - the reason to keep reading comes first, not last.

## Every section earns its place

Grounded in newsroom discipline (AP Style, Reuters-style rigor): a section either reveals a new
fact or interprets one already given, never pads to hit a length. Strip filler paragraphs, never
strip facts - a short, complete piece beats a padded one.

## Headings

Parenthetical-in-a-heading is banned at the base ([[no-slop]], checked in any
markdown file). On top of that, a long-form heading also gets no narrative, dramatic, or
clickbait framing - self-check: could this heading be a thriller chapter title? If yes, rewrite
it to name what the section actually contains.

## Tense accuracy for anything not live yet

An unlaunched, unconfirmed, or in-progress feature is never stated in the present tense as
already working. Frame it as expected, returning, or planned; mark unconfirmed specifics as
such; never invent a date or detail to fill a gap in what's actually known.

## Statistical self-check: uniformity is itself a tell

Generated prose is suspiciously even. Skim a finished piece for:

- Paragraph lengths within 15% of each other throughout the piece - vary them.
- No sentence under 8 or over 30 words across a 500-word stretch - vary sentence length instead
  of smoothing it flat.
- Transition words ("however", "additionally", "furthermore") opening more than roughly 30% of
  paragraphs - cut most of them and let the facts connect on their own.

Uneven pacing reads human. Even pacing reads manufactured, regardless of what the words say.

## Common MT/AI phrasing failures

- **False personification**: an inanimate noun given a verb only something living would take
  ("the report argues", "the config decides"). Would this noun actually do that, in plain
  English, natively?
- **Fragment posing as punch**: a grammatically incomplete clause dressed up as a parallel-
  structure flourish. Read it aloud - does it parse as a complete thought, or just sound like one?
- **Register mismatch**: childish diction in copy written for adults, or needlessly formal diction
  where plain words would do.

## Cross-batch consistency

Before a batch of pieces ships together: every internal link resolves, no two pieces in the
batch contradict each other on the same fact, a fact introduced in one piece is still
re-established in a sibling piece rather than assumed known (see
[[no-slop]]'s context-poisoning ban - it applies across a batch, not just within
one file), and a recurring term uses the same word in every piece, not a different synonym per
author or per session.

## Self-check

Run [[no-slop]]'s self-check first, then:

1. Lede is one true specific, not a question, a wind-up, or an invented scene?
2. Headings name what the section contains, not a tease or a verdict?
3. Paragraph and sentence lengths vary, or does the piece read machine-even end to end?
4. Any unlaunched or in-progress feature stated as present-tense settled fact?
5. False personification or a fragment dressed as a flourish anywhere?
6. If this is one of several pieces shipping together: links resolve, no contradictions, terms
   consistent, no fact assumed known from a sibling piece?
7. Would this survive a professional proofreader circling it in red, or does any sentence feel
   like it's performing prose instead of saying something real?
