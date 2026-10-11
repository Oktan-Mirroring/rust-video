#!/usr/bin/env bash
# PreToolUse gate: deny a `git commit` whose staged diff has not been scanned yet, feed back
# a mechanical scan of the changed files plus the lint skills that diff's file types touch,
# then allow the retry. Scan keyed on a SHA1 of the staged diff, runs once per distinct diff.
# Skill demands keyed on session + skill name, once per session.
set -uo pipefail

raw="$(cat)"

printf '%s' "$raw" | grep -Eq 'git[[:space:]]+commit' || exit 0

if printf '%s' "$raw" | grep -Eq 'git[[:space:]]+add[^&]*&&[^&]*git[[:space:]]+commit'; then
  msg='Chained git add with git commit: this deny drops the whole command, so the add never ran and nothing staged. Run git add as its own call, then git commit on its own.'
  printf '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"%s"}}\n' "$msg"
  exit 0
fi

git rev-parse --git-dir >/dev/null 2>&1 || exit 0
gitdir="$(git rev-parse --absolute-git-dir)"

diff="$(git diff --cached 2>/dev/null || true)"

sumcmd="sha1sum"
command -v sha1sum >/dev/null 2>&1 || sumcmd="shasum"
hash="$(printf '%s' "$diff" | "$sumcmd" | cut -d' ' -f1)"

marker="$gitdir/commit-lint-gate.stamp"
prev=""
[ -f "$marker" ] && prev="$(tr -d '[:space:]' < "$marker")"
[ "$prev" = "$hash" ] && exit 0
printf '%s' "$hash" > "$marker"

changed="$(git diff --cached --name-only 2>/dev/null)"

leak_re='\bas requested\b|\bas asked\b|\byou (asked|requested|wanted)\b|\bper your\b|\bwe (just|now) |\bi just |\bfor (this|the) (fix|task|change|pr|commit)\b|\bin (this|the) (fix|task|pr|commit)\b|\bas we discussed\b|\bno longer\b|\bpreviously\b|\bused to\b|\bnow (does|uses|returns|handles|computes|works|reads|gates)\b'
tell_re="let's dive in|in the landscape of|in today's fast-paced|it is worth noting|it is important to|that being said|at the end of the day|not only .* but also|as an ai|i cannot|left this empty|fill this in later|you can add .* here|here is the"

leak_hits=""; tell_hits=""; dash_hits=""; blob_hits=""
while IFS= read -r f; do
  [ -n "$f" ] && [ -f "$f" ] || continue
  case "$f" in .claude/*|*/.claude/*) continue ;; esac
  case "$f" in
    *.rs)   prose="$(grep -nE '^[[:space:]]*(//|/\*|\*)' -- "$f" 2>/dev/null)" ;;
    *.md|*.toml) prose="$(grep -n '' -- "$f" 2>/dev/null)" ;;
    *) continue ;;
  esac
  [ -n "$prose" ] || true
  add() { [ -n "$2" ] && printf '%s\n' "$2" | sed "s#^#    $f:#"; }
  leak_hits+="$(add x "$(printf '%s' "$prose" | grep -iE "$leak_re" | head -12)")"$'\n'
  tell_hits+="$(add x "$(printf '%s' "$prose" | grep -iE "$tell_re" | head -12)")"$'\n'
  dash_hits+="$(add x "$(printf '%s' "$prose" | grep -E '—|–' | head -12)")"$'\n'
  case "$f" in
    *.rs)
      runs="$(awk '
        function flush() { if (run>=4) print start; run=0; start=0 }
        /^[[:space:]]*\/\/[\/!][[:space:]]*#/ { flush(); next }
        /^[[:space:]]*\/\/[\/!]/ {
          rest=$0; sub(/^[[:space:]]*\/\/[\/!]/,"",rest); gsub(/[[:space:]]/,"",rest)
          if (rest=="") { flush(); next }
          if (run==0) start=FNR
          run++; next
        }
        { flush() }
        END { flush() }
      ' "$f" 2>/dev/null)"
      blob_hits+="$(add x "$runs")"$'\n'
      ;;
  esac
done <<< "$changed"

trim() { printf '%s' "$1" | grep -v '^[[:space:]]*$'; }
leak_hits="$(trim "$leak_hits")"; tell_hits="$(trim "$tell_hits")"
dash_hits="$(trim "$dash_hits")"; blob_hits="$(trim "$blob_hits")"

harness=""
ignore_hits="$(git diff --cached -U0 2>/dev/null | grep -E '^\+.*#\[ignore' | grep -vE '#\[ignore[[:space:]]*=|reason' | head -10)"
[ -n "$ignore_hits" ] && harness+=$'\n\n[harness] a new #[ignore] carries no reason. Write `#[ignore = "..."]`:\n'"$ignore_hits"

probe_added="$(printf '%s\n' "$changed" | grep -E '^scripts/[^/]*-probe/' || true)"
probe_faults=""
while IFS= read -r f; do
  [ -n "$f" ] || continue
  d="$(printf '%s' "$f" | sed -E 's#^(scripts/[^/]*-probe)/.*#\1#')"
  [ "$d" = "$f" ] && continue
  [ -f "$d/README.md" ] || probe_faults+="  $d has no README.md"$'\n'
  if [ -f "$d/run.sh" ]; then
    if ! { grep -q 'mktemp' "$d/run.sh" && grep -q 'trap' "$d/run.sh"; }; then
      probe_faults+="  $d/run.sh has no mktemp + trap cleanup"$'\n'
    fi
  else
    probe_faults+="  $d has no run.sh"$'\n'
  fi
done <<< "$probe_added"
[ -n "$probe_faults" ] && harness+=$'\n\n[harness] a new probe ships a README and a run.sh that builds under `mktemp -d` with a trap cleanup:\n'"$probe_faults"

scan=""
[ -n "$blob_hits" ] && scan+=$'\n\n[rust-docs] paragraph-blob doc comments (4+ prose /// lines, no rustdoc section) - collapse to a noun phrase + sections, or delete:\n'"$blob_hits"
[ -n "$leak_hits" ] && scan+=$'\n\n[rust-docs] refactor history or prompt/session context in comments - delete unless it is a decision/constraint/footgun:\n'"$leak_hits"
[ -n "$dash_hits" ] && scan+=$'\n\n[human-voice] em/en dash, the loudest AI tell - replace with a period, comma, or colon:\n'"$dash_hits"
[ -n "$tell_hits" ] && scan+=$'\n\n[human-voice] AI-tell phrasing or placeholder leak - rewrite plainly or cut:\n'"$tell_hits"

session="$(printf '%s' "$raw" | jq -r '.session_id // empty' 2>/dev/null || true)"
[ -n "$session" ] || session="nosession"
state_dir="$HOME/.claude/state/commit-lint-gate"
mkdir -p "$state_dir" 2>/dev/null || true

rust_touched=false
while IFS= read -r f; do
  case "$f" in *.rs) rust_touched=true; break ;; esac
done <<< "$changed"

want_skills="no-slop human-voice commit-message"
$rust_touched && want_skills="$want_skills rust-docs rust-architecture rust-file-layout"

to_read=""
for s in $want_skills; do
  m="$state_dir/${session}-${s}"
  [ -f "$m" ] && continue
  to_read="$to_read$s "
  touch "$m" 2>/dev/null || true
done

[ -n "$scan" ] || [ -n "$to_read" ] || [ -n "$harness" ] || exit 0

skill_blurb() {
  case "$1" in
    no-slop) printf '%s' "no-slop (global skill)          - any text, any language or file type: fabrication, hollow statements, context-poisoning, session-leak, every AI-tell." ;;
    rust-docs) printf '%s' "rust-docs (global skill)        - any /// //! or // in the diff: the prune pass, comment voice, section order, mandatory public-API/panics docs." ;;
    rust-architecture) printf '%s' "rust-architecture (global skill) - any module, type, trait, or public API: narrowest visibility; behaviour on types, not free-fn soup." ;;
    rust-file-layout) printf '%s' "rust-file-layout (global skill)  - any new or restructured .rs file or top-level item." ;;
    human-voice) printf '%s' "human-voice (global skill)       - any prose (README, docs/*.md, comment bodies, the commit message): no AI tells, no em dashes." ;;
    commit-message) printf '%s' "commit-message (global skill)    - the message itself." ;;
  esac
}

skills_text=""; n=0
for s in $to_read; do n=$((n+1)); skills_text="$skills_text  $n. $(skill_blurb "$s")"$'\n'; done

if [ -n "$skills_text" ]; then
  reason="Commit gate. The rules are not in this message - they live in the skill files. Open and read IN FULL the ones named below (already covered this session are skipped), then enforce every rule on the changed files. Re-stage and re-run the same commit; it passes once this staged state is gated.
${skills_text}Read the file, apply it, fix in place, re-stage, commit again. Do not reconstruct a rule from memory - read it."
else
  reason="Commit gate. Lint skills already loaded this session, nothing new to read."
fi
reason="${reason}
The scan below is only proof the pass was skipped: a few offenders the skill would have caught. It is a heuristic spot-check, not the rulebook - judge each, fix every real one in the file, and apply the skill everywhere it does not reach.${scan}${harness}"

esc="$(printf '%s' "$reason" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' | sed ':a;N;$!ba;s/\n/\\n/g')"
printf '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"%s"}}\n' "$esc"
exit 0
