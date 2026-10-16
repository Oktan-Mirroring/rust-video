# Skill registry

| Skill | Trigger |
|---|---|
| [commit-message](commit-message/SKILL.md) | Drafting any commit message |
| [human-voice](human-voice/SKILL.md) | Writing long-form prose |
| [no-slop](no-slop/SKILL.md) | Writing any text a human reads |
| [realtime-scope](realtime-scope/SKILL.md) | Adding or scoping any behaviour |
| [rust-architecture](rust-architecture/SKILL.md) | Designing any module, type, trait, or public API |
| [rust-docs](rust-docs/SKILL.md) | Writing any `///`, `//!`, or `//` comment |
| [rust-file-layout](rust-file-layout/SKILL.md) | Creating a `.rs` file or adding a top-level item |

New skills land as `.agents/skills/<name>/SKILL.md` with a row here.
`.claude/skills` symlinks to this directory so both paths resolve.
