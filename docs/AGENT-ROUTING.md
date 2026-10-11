# Agent routing

Starter table. Each row names the task, the doc that owns it, and the command
that proves it.

| Task | Doc | Command |
|---|---|---|
| add codec or tier behaviour | `README.md` scope, `.claude/skills/realtime-scope/SKILL.md` | `just ci` |
| change the workspace layout | `README.md` layout table | `just build` |
| fix a lint or warning | `AGENTS.md` directive 1 | `just check` |
| add a dependency | `AGENTS.md` directive 5 | `just ci` |
| write skill or routing docs | `.claude/skills/INDEX.md` | `just ci` |
