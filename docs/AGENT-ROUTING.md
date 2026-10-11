# Agent routing

Starter table. Each row names the task, the doc that owns it, and the command
that proves it.

| Task | Doc | Command |
|---|---|---|
| add codec or tier behaviour | `README.md` scope, `.agents/skills/realtime-scope/SKILL.md` | `just ci` |
| change the workspace layout | `README.md` layout table | `just build` |
| fix a lint or warning | `AGENTS.md` directive 1 | `just check` |
| add a dependency | `AGENTS.md` directive 5 | `just ci` |
| write skill or routing docs | `.agents/skills/INDEX.md` | `just ci` |
| add a unit test | `docs/testing/README.md` tier table | `just test` |
| add an e2e or golden test | `docs/testing/README.md`, `docs/testing/vectors.md` | `just e2e` |
| fetch the vector corpus | `docs/testing/vectors.md` | `just vectors` |
| generate or cover goldens | `docs/testing/goldens.md` | `just goldens` |
| register a capability | `docs/testing/manifest.toml` | `just ci` |
| touch a codec cap or level | `docs/specs/<codec>.md`, `docs/sourcing.md` | `just spec-check` |
| add a sourced value | `docs/sourcing.md` tier table | `just ci` |
| touch versions, tiers, or CI matrix | `docs/compat.md` | `just ci` |
| touch frames, pools, queues, or exports | `docs/runtime.md` | `just ci` |
