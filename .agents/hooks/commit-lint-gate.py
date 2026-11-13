#!/usr/bin/env python3
"""PreToolUse gate: deny an unscanned `git commit`, name skills, allow retry."""
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path

COMMIT = re.compile(r"git\s+commit")
CHAINED = re.compile(r"git\s+add[^&]*&&[^&]*git\s+commit")
LEAK = re.compile(
    r"\bas requested\b|\bas asked\b|\byou (asked|requested|wanted)\b|\bper your\b"
    r"|\bwe (just|now) |\bi just |\bfor (this|the) (fix|task|change|pr|commit)\b"
    r"|\bin (this|the) (fix|task|pr|commit)\b|\bas we discussed\b|\bno longer\b"
    r"|\bpreviously\b|\bused to\b|\bnow (does|uses|returns|handles|computes|works|reads|gates)\b",
    re.IGNORECASE,
)
TELL = re.compile(
    r"let's dive in|in the landscape of|in today's fast-paced|it is worth noting"
    r"|it is important to|that being said|at the end of the day|not only .* but also"
    r"|as an ai|i cannot|left this empty|fill this in later|you can add .* here|here is the",
    re.IGNORECASE,
)
DASH = re.compile(r"—|–")
PROSE_RS = re.compile(r"^[ \t]*(//|/\*|\*)")
DOC_LINE = re.compile(r"^[ \t]*//[/!]")
DOC_HIDDEN = re.compile(r"^[ \t]*//[/!][ \t]*#")
IGNORE_ADD = re.compile(r"^\+.*#\[ignore")
IGNORE_OK = re.compile(r"#\[ignore[ \t]*=|reason")
PROBE = re.compile(r"^scripts/[^/]*-probe/")

BLURBS = {
    "no-slop": "no-slop (global skill)          - any text, any language or file type: fabrication, hollow statements, context-poisoning, session-leak, every AI-tell.",
    "rust-docs": "rust-docs (global skill)        - any /// //! or // in the diff: the prune pass, comment voice, section order, mandatory public-API/panics docs.",
    "rust-architecture": "rust-architecture (global skill) - any module, type, trait, or public API: narrowest visibility; behaviour on types, not free-fn soup.",
    "rust-file-layout": "rust-file-layout (global skill)  - any new or restructured .rs file or top-level item.",
    "human-voice": "human-voice (global skill)       - any prose (README, docs/*.md, comment bodies, the commit message): no AI tells, no em dashes.",
    "commit-message": "commit-message (global skill)    - the message itself.",
}


def deny(reason: str) -> None:
    print(json.dumps({"hookSpecificOutput": {"hookEventName": "PreToolUse",
                                             "permissionDecision": "deny",
                                             "permissionDecisionReason": reason}},
                     ensure_ascii=False))


def git(*args: str) -> str:
    completed = subprocess.run(["git", *args], capture_output=True, text=True)
    return completed.stdout if completed.returncode == 0 else ""


def blob_starts(path: Path) -> list[int]:
    starts = []
    run = 0
    start = 0

    def flush() -> None:
        nonlocal run, start
        if run >= 4:
            starts.append(start)
        run = 0
        start = 0

    try:
        text = path.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError:
        return []
    for lineno, line in enumerate(text, 1):
        if DOC_HIDDEN.match(line):
            flush()
        elif DOC_LINE.match(line):
            rest = re.sub(r"[ \t]", "", DOC_LINE.sub("", line, count=1))
            if not rest:
                flush()
            else:
                if run == 0:
                    start = lineno
                run += 1
        else:
            flush()
    flush()
    return starts


def scan_file(name: str) -> tuple[list[str], list[str], list[str], list[str]]:
    leaks: list[str] = []
    tells: list[str] = []
    dashes: list[str] = []
    blobs: list[str] = []
    path = Path(name)
    if not path.is_file():
        return leaks, tells, dashes, blobs
    try:
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError:
        return leaks, tells, dashes, blobs
    if name.endswith(".rs"):
        prose = [f"{n}:{line}" for n, line in enumerate(lines, 1) if PROSE_RS.match(line)]
    elif name.endswith((".md", ".toml")):
        prose = [f"{n}:{line}" for n, line in enumerate(lines, 1)]
    else:
        return leaks, tells, dashes, blobs
    leaks = [f"    {name}:{hit}" for hit in prose if LEAK.search(hit)][:12]
    tells = [f"    {name}:{hit}" for hit in prose if TELL.search(hit)][:12]
    dashes = [f"    {name}:{hit}" for hit in prose if DASH.search(hit)][:12]
    if name.endswith(".rs"):
        blobs = [f"    {name}:{start}" for start in blob_starts(path)]
    return leaks, tells, dashes, blobs


def main() -> int:
    raw = sys.stdin.read().lstrip("\ufeff")
    if not COMMIT.search(raw):
        return 0
    if CHAINED.search(raw):
        deny("Chained git add with git commit: this deny drops the whole command, so the add never ran and nothing staged. Run git add as its own call, then git commit on its own.")
        return 0
    found = subprocess.run(["git", "rev-parse", "--git-dir"],
                           capture_output=True, text=True)
    if found.returncode != 0:
        return 0
    gitdir = git("rev-parse", "--absolute-git-dir").strip()
    digest = hashlib.sha1(git("diff", "--cached").rstrip("\n").encode()).hexdigest()
    marker = Path(gitdir) / "commit-lint-gate.stamp"
    try:
        prev = re.sub(r"\s", "", marker.read_text(encoding="utf-8", errors="replace"))
    except OSError:
        prev = ""
    if prev == digest:
        return 0
    try:
        marker.write_text(digest, encoding="utf-8")
    except OSError:
        pass
    changed = [line for line in git("diff", "--cached", "--name-only").splitlines() if line]
    leaks: list[str] = []
    tells: list[str] = []
    dashes: list[str] = []
    blobs: list[str] = []
    for name in changed:
        if name.startswith(".agents/hooks/") or "/.agents/hooks/" in name:
            continue
        file_leaks, file_tells, file_dashes, file_blobs = scan_file(name)
        leaks += file_leaks
        tells += file_tells
        dashes += file_dashes
        blobs += file_blobs
    harness = ""
    ignores = [line for line in git("diff", "--cached", "-U0").splitlines()
               if IGNORE_ADD.match(line) and not IGNORE_OK.search(line)][:10]
    if ignores:
        harness += "\n\n[harness] a new #[ignore] carries no reason. Write `#[ignore = \"...\"]`:\n" + "\n".join(ignores)
    faults: list[str] = []
    for name in changed:
        if not PROBE.match(name):
            continue
        top = "/".join(name.split("/")[:2])
        if top == name:
            continue
        if not Path(top, "README.md").is_file():
            faults.append(f"  {top} has no README.md")
        run_sh = Path(top, "run.sh")
        if run_sh.is_file():
            try:
                body = run_sh.read_text(encoding="utf-8", errors="replace")
            except OSError:
                body = ""
            if "mktemp" not in body or "trap" not in body:
                faults.append(f"  {top}/run.sh has no mktemp + trap cleanup")
        else:
            faults.append(f"  {top} has no run.sh")
    if faults:
        harness += "\n\n[harness] a new probe ships a README and a run.sh that builds under `mktemp -d` with a trap cleanup:\n" + "\n".join(faults)
    scan = ""
    if blobs:
        scan += "\n\n[rust-docs] paragraph-blob doc comments (4+ prose /// lines, no rustdoc section) - collapse to a noun phrase + sections, or delete:\n" + "\n".join(blobs)
    if leaks:
        scan += "\n\n[rust-docs] refactor history or prompt/session context in comments - delete unless it is a decision/constraint/footgun:\n" + "\n".join(leaks)
    if dashes:
        scan += "\n\n[human-voice] em/en dash, the loudest AI tell - replace with a period, comma, or colon:\n" + "\n".join(dashes)
    if tells:
        scan += "\n\n[human-voice] AI-tell phrasing or placeholder leak - rewrite plainly or cut:\n" + "\n".join(tells)
    try:
        session = json.loads(raw).get("session_id") or "nosession"
    except (ValueError, AttributeError):
        session = "nosession"
    state_dir = Path.home() / ".agents" / "state" / "commit-lint-gate"
    state_dir.mkdir(parents=True, exist_ok=True)
    want = ["no-slop", "human-voice", "commit-message"]
    if any(name.endswith(".rs") for name in changed):
        want += ["rust-docs", "rust-architecture", "rust-file-layout"]
    unread = []
    for skill in want:
        mark = state_dir / f"{session}-{skill}"
        if mark.is_file():
            continue
        unread.append(skill)
        try:
            mark.touch()
        except OSError:
            pass
    if not scan and not unread and not harness:
        return 0
    skills_text = "".join(f"  {n}. {BLURBS[skill]}\n" for n, skill in enumerate(unread, 1))
    if skills_text:
        reason = ("Commit gate. The rules are not in this message - they live in the skill files. Open and read IN FULL the ones named below (already covered this session are skipped), then enforce every rule on the changed files. Re-stage and re-run the same commit; it passes once this staged state is gated.\n"
                  f"{skills_text}Read the file, apply it, fix in place, re-stage, commit again. Do not reconstruct a rule from memory - read it.")
    else:
        reason = "Commit gate. Lint skills already loaded this session, nothing new to read."
    reason += ("\nThe scan below is only proof the pass was skipped: a few offenders the skill would have caught. It is a heuristic spot-check, not the rulebook - judge each, fix every real one in the file, and apply the skill everywhere it does not reach."
               f"{scan}{harness}")
    deny(reason)
    return 0


if __name__ == "__main__":
    sys.exit(main())
