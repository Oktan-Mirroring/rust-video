# Commit lint gate

`commit-lint-gate.sh` scans the staged diff and names the skill files to
read before a commit proceeds. Each distinct staged state is scanned once,
keyed on a hash of the diff.

Invoke it with the tool call on stdin:

```sh
echo 'git commit' | just commit-lint-gate
```

The script reads tool-call JSON from stdin and answers with a
`permissionDecision` payload on stdout. Register that pipeline as a
pre-commit hook in whatever harness runs the repo.
