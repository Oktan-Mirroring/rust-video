# Contributing

## Gates

Every change passes `just ci` before review: format check, clippy with warnings
denied, tests, and docs. Fix the cause of a failure, never weaken the gate.

## Commits

Terse conventional commits: `feat:`, `fix:`, `chore:`, `docs:`. One change per
commit, body only when the why is not obvious from the diff.

## Scope

Read `.claude/skills/realtime-scope/SKILL.md` before adding behaviour.
Realtime encode/decode and its tiers are in; demuxing, subtitles, filters, and
audio are out.

## License

Contributions land under MIT or Apache 2.0, matching the workspace.
