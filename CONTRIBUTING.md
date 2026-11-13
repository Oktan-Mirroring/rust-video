# Contributing

## Gates

Every change passes `just ci` before review: format check, clippy with warnings
denied, debug build, tests, docs, and the spec-table diff. Fix the cause of a
failure, never weaken the gate.

`just test` covers default, no-default, and sw-codec features; the livepack ffmpeg oracle
(`fmp4_ffmpeg`, behind `--features rust-video-livepack/oracle`) stays out of it
and runs only via `just e2e`, which also needs `./corpus`. A missing default
corpus compiles e2e tests out silently; only an explicit `RUST_VIDEO_CORPUS`
pointing nowhere warns.

## Commits

Terse conventional commits: `feat:`, `fix:`, `chore:`, `docs:`. One change per
commit, body only when the why is not obvious from the diff.

## Scope

Read `.agents/skills/realtime-scope/SKILL.md` before adding behaviour.
Realtime encode/decode and its tiers are in; demuxing, subtitles, filters, and
audio are out.

## License

Contributions land under MIT or Apache 2.0, matching the workspace.
