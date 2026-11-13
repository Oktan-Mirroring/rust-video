# Commit lint gate

`commit-lint-gate.py` scans the staged diff and names the skill files to
read before a commit proceeds. Each distinct staged state is scanned once,
keyed on a hash of the diff. The `.sh` twin keeps the same logic for
bash-only harnesses; `just commit-lint-gate` runs the Python entry point
on every OS.

Invoke it with the tool call on stdin:

```sh
echo 'git commit' | just commit-lint-gate
```

The script reads tool-call JSON from stdin and answers with a
`permissionDecision` payload on stdout. Register that pipeline as a
pre-commit hook in whatever harness runs the repo.
