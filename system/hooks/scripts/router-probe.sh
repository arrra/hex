#!/usr/bin/env bash
# router-probe.sh — acceptance probe for the PreToolUse router (deliverable 5).
#
# Feeds one positive fixture and one near-miss fixture per rule id in
# router-rules.json through the REAL router implementation (a subprocess per
# fixture, never sourced/mocked), using a single temp ledger dir for the
# whole run. Prints one line per case:
#   <rule_id> expected=<decision|abstain> got=<decision|...> PASS|FAIL
# and one final ledger-count line. Exits 1 if any case FAILs or if the final
# ledger line count does not equal the number of positive fixtures (every
# near-miss must abstain and write nothing). Exit status comes from this
# script's own tally — never from a pipeline's last stage.
#
# ROUTER_IMPL selects which implementation is driven:
#   ROUTER_IMPL=python (default) — the reference pretooluse-router.py script.
#   ROUTER_IMPL=rust             — the `hex hook router` Rust port. Requires
#     HEX_ROUTER_BIN to point at a built `hex` binary (falls back to
#     ${CARGO_TARGET_DIR:-target}/release/hex under the repo root if unset).
# Same fixtures, same PASS/FAIL line format, same exit-code contract in both
# modes.
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROUTER="$SCRIPT_DIR/pretooluse-router.py"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
ROUTER_IMPL="${ROUTER_IMPL:-python}"
HEX_ROUTER_BIN="${HEX_ROUTER_BIN:-${CARGO_TARGET_DIR:-$REPO_ROOT/target}/release/hex}"
LEDGER_DIR="$(mktemp -d)"
trap 'rm -rf "$LEDGER_DIR"' EXIT

python3 - "$ROUTER" "$LEDGER_DIR" "$ROUTER_IMPL" "$HEX_ROUTER_BIN" "$REPO_ROOT" <<'PYEOF'
import json
import os
import subprocess
import sys
from pathlib import Path

router = sys.argv[1]
ledger_dir = Path(sys.argv[2])
router_impl = sys.argv[3]
hex_router_bin = sys.argv[4]
repo_root = sys.argv[5]

if router_impl not in ("python", "rust"):
    print(
        f"[router-probe] unknown ROUTER_IMPL={router_impl!r}; expected 'python' or 'rust'",
        file=sys.stderr,
    )
    sys.exit(2)

if router_impl == "rust":
    if not hex_router_bin or not os.path.isfile(hex_router_bin) or not os.access(
        hex_router_bin, os.X_OK
    ):
        print(
            f"[router-probe] HEX_ROUTER_BIN={hex_router_bin!r} does not exist or is not "
            "executable — build it with `cargo build --release --locked` "
            "(from .hex/harness) first",
            file=sys.stderr,
        )
        sys.exit(2)

HOME_DIR = os.environ.get("HOME", "/tmp")
DEFAULT_CWD = os.path.join(HOME_DIR, "hex")


def make_payload(tool_name, tool_input, cwd=DEFAULT_CWD, session_id="probe-1"):
    return {
        "session_id": session_id,
        "transcript_path": "/tmp/transcript.jsonl",
        "cwd": cwd,
        "permission_mode": "default",
        "hook_event_name": "PreToolUse",
        "tool_name": tool_name,
        "tool_input": tool_input,
    }


# One positive + one near-miss fixture per seed rule id (deliverable 4).
# Mirrors .hex/hooks/tests/test_router.py::RULE_FIXTURES; kept in sync
# manually since this probe must run standalone against the real scripts,
# not import test machinery.
RULE_FIXTURES = [
    dict(
        id="gh-pr-merge-ci-green",
        decision="prior",
        tool_name="Bash",
        positive={"command": "gh pr merge 123 --squash"},
        near_miss={"command": "gh pr checks 123 --watch"},
    ),
    dict(
        id="vitest-spawnsync",
        decision="prior",
        tool_name="Edit",
        positive={
            "file_path": "src/components/foo.test.ts",
            "old_string": "x",
            "new_string": "const r = spawnSync('ls', []);",
        },
        near_miss={
            "file_path": "src/components/foo.ts",
            "old_string": "x",
            "new_string": "const r = spawnSync('ls', []);",
        },
    ),
    dict(
        id="git-stash-shared-checkout",
        decision="deny",
        tool_name="Bash",
        positive={"command": "git stash"},
        near_miss={"command": "git stash list"},
    ),
    dict(
        id="git-push-force",
        decision="ask",
        tool_name="Bash",
        positive={"command": "git push origin main --force"},
        near_miss={"command": "git push origin main"},
    ),
    dict(
        id="git-push-force-with-lease",
        decision="ask",
        tool_name="Bash",
        positive={"command": "git push origin main --force-with-lease"},
        near_miss={"command": "git push origin main"},
    ),
    dict(
        id="git-destructive-ask",
        decision="ask",
        tool_name="Bash",
        positive={"command": "git reset --hard HEAD~1"},
        near_miss={"command": "git reset --soft HEAD~1"},
    ),
    dict(
        id="boi-dispatch-spec-priors",
        decision="prior",
        tool_name="Bash",
        positive={"command": "boi dispatch spec.toml"},
        near_miss={"command": "boi dashboard"},
    ),
    dict(
        id="gh-fast-polling",
        decision="ask",
        tool_name="Bash",
        positive={"command": "while true; do gh pr checks 123; sleep 5; done"},
        near_miss={"command": "while true; do gh pr checks 123; sleep 90; done"},
    ),
    dict(
        id="pipe-tail-masks-exit",
        decision="prior",
        tool_name="Bash",
        positive={"command": "pytest -q | tail -20"},
        near_miss={"command": "pytest -q"},
    ),
    dict(
        id="hex-events-flat-policy",
        decision="prior",
        tool_name="Write",
        positive={
            "file_path": os.path.join(HOME_DIR, ".hex-events/policies/my-policy.yaml"),
            "content": "name: my-policy\ntrigger:\n  event: foo\naction:\n  type: shell\n  command: echo hi\n",
        },
        near_miss={
            "file_path": os.path.join(HOME_DIR, ".hex-events/policies/my-policy.yaml"),
            "content": "name: my-policy\nrules:\n  - name: r1\n    trigger:\n      event: foo\n",
        },
    ),
    dict(
        id="hex-memory-index-full",
        decision="ask",
        tool_name="Bash",
        positive={"command": "hex memory index --full"},
        near_miss={"command": "hex memory index"},
    ),
    dict(
        id="builtin-scheduler-tools",
        decision="ask",
        tool_name="CronCreate",
        positive={"schedule": "* * * * *", "command": "echo hi"},
        near_miss_tool_name="CronList",
        near_miss_input={"filter": "*"},
    ),
    dict(
        id="backticks-in-unquoted-heredoc",
        decision="prior",
        tool_name="Bash",
        positive={"command": "python3 - <<PYEOF\nprint('run `boi start` now')\nPYEOF"},
        near_miss={"command": "python3 - <<'PYEOF'\nprint('run `boi start` now')\nPYEOF"},
    ),
    dict(
        id="builtin-websearch",
        decision="ask",
        tool_name="WebSearch",
        positive={"query": "test"},
        near_miss_tool_name="WebFetch",
        near_miss_input={"url": "https://example.com"},
    ),
]

assert len(RULE_FIXTURES) == 14, "expected exactly 14 seed rule fixtures"

# Extra positive/near-miss pairs beyond the one-per-rule seed set above,
# covering PR #5 round-1 findings F4/F5/F6/F11. Reuse the owning rule's own
# `id` (the ledger fire is genuinely tagged with that rule) so the
# ledger-count tally below still adds up: one new ledger line per positive
# fixture here, zero per near miss, same as the seed set.
WORKTREE_CWD = os.path.join(HOME_DIR, ".boi/v2/worktrees/Scnfz8k1f/T7w2t3bzf")

EXTRA_FIXTURES = [
    # F4: unless_cwd must track the EFFECTIVE checkout of the invocation
    # (via -C / a preceding cd), not just the hook's own payload cwd.
    dict(
        id="git-stash-shared-checkout",
        decision="deny",
        tool_name="Bash",
        positive={"command": "git -C /shared/checkout stash"},
        cwd=WORKTREE_CWD,
        near_miss={"command": f"git -C {WORKTREE_CWD} stash"},
        near_miss_cwd=DEFAULT_CWD,
    ),
    dict(
        id="git-stash-shared-checkout",
        decision="deny",
        tool_name="Bash",
        positive={"command": "cd /shared/checkout && git stash"},
        cwd=WORKTREE_CWD,
        near_miss={"command": f"cd {WORKTREE_CWD} && git stash"},
        near_miss_cwd=DEFAULT_CWD,
    ),
    # F1 (review round 1 redo): `_CD_RE`/`_DASH_C_RE` captured `(\S+)`,
    # which swallowed a trailing `;` so a chained `cd x; cd y && ...` lost
    # its second `cd` (no leading separator left to anchor on) and the
    # stale first `cd` wrongly exempted the stash. Capture must stop at
    # `;`/`&`/`|`/`)` so every `cd` in the chain is seen.
    dict(
        id="git-stash-shared-checkout",
        decision="deny",
        tool_name="Bash",
        positive={"command": "cd /worktrees/x; cd /shared/checkout && git stash"},
        cwd=WORKTREE_CWD,
        near_miss={"command": f"cd /worktrees/x; cd {WORKTREE_CWD} && git stash"},
        near_miss_cwd=DEFAULT_CWD,
    ),
    # F4 (review round 1 redo): a relative `cd sub` from a /worktrees/
    # payload cwd is resolvable against that cwd (not "uncertain") and
    # stays inside the same worktree checkout -- must abstain.
    dict(
        id="git-stash-shared-checkout",
        decision="deny",
        tool_name="Bash",
        positive={"command": "git -C /other/shared stash"},
        cwd=WORKTREE_CWD,
        near_miss={"command": "cd sub && git stash"},
        near_miss_cwd=WORKTREE_CWD,
    ),
    # F5: a leading `+` on a push refspec forces the update, same as
    # --force/-f, but neither original rule alternative matched it.
    dict(
        id="git-push-force",
        decision="ask",
        tool_name="Bash",
        positive={"command": "git push origin +HEAD:main"},
        near_miss={"command": "git push origin HEAD:main"},
    ),
    # F6: destructive git arguments in equivalent spellings/positions must
    # still ask (long options, reordered options, checkout-with-tree).
    dict(
        id="git-destructive-ask",
        decision="ask",
        tool_name="Bash",
        positive={"command": "git clean --force -d"},
        near_miss={"command": "git clean -n"},
    ),
    dict(
        id="git-destructive-ask",
        decision="ask",
        tool_name="Bash",
        positive={"command": "git reset HEAD~1 --hard"},
        near_miss={"command": "git reset --soft HEAD~1"},
    ),
    dict(
        id="git-destructive-ask",
        decision="ask",
        tool_name="Bash",
        positive={"command": "git checkout HEAD -- tracked-file"},
        near_miss={"command": "git checkout -b newbranch"},
    ),
    # F2 (review round 1 redo): `(?:\S+\s+)*` treated `;`/`&&`/`|` as
    # ordinary whitespace-separated tokens, so a non-destructive command
    # followed by an unrelated command containing a destructive-looking
    # flag (e.g. `rm -rf`) was scanned as one option run and asked. The
    # option-skip must stop at `;`, `&`, and `|`.
    dict(
        id="git-destructive-ask",
        decision="ask",
        tool_name="Bash",
        positive={"command": "git reset HEAD~1 --hard"},
        near_miss={"command": "git clean -n; rm -rf build"},
    ),
    # F11: a normal multiline while/until loop must ask the same as its
    # one-line equivalent (rules compile MULTILINE, not DOTALL); an
    # out-of-loop gh call before an unrelated loop must stay abstain.
    dict(
        id="gh-fast-polling",
        decision="ask",
        tool_name="Bash",
        positive={"command": "while true; do\n  gh pr checks 123\n  sleep 5\ndone"},
        near_miss={"command": "gh pr checks 123\nwhile true; do\n  sleep 5\ndone"},
    ),
    dict(
        id="gh-fast-polling",
        decision="ask",
        tool_name="Bash",
        positive={"command": "until false; do\n  gh pr checks 123\n  sleep 5\ndone"},
        near_miss={"command": "for n in 293 294 295; do gh pr checks $n; sleep 2; done"},
    ),
    # F3 (review round 1 redo): the loop-body gaps are lazy but not bounded
    # to their OWN `done` -- they can skip past an earlier, unrelated
    # loop's closing `done` while hunting for a gh+sleep pair that belongs
    # to a later loop. A candidate spanning more than one `done` must be
    # rejected (see `_polling_loop_bounded` in pretooluse-router.py).
    dict(
        id="gh-fast-polling",
        decision="ask",
        tool_name="Bash",
        positive={"command": "while true; do\n  gh pr checks 123\n  sleep 5\ndone"},
        near_miss={
            "command": (
                "while read x; do echo $x; done < f\n"
                "gh pr checks 123\n"
                "sleep 5\n"
                "for p in 1 2; do echo; done"
            )
        },
    ),
    # F8 (review round 2 redo): `executable_mask` blanks quoted argument
    # content to spaces before `_effective_checkout` ever sees it, so a
    # quoted `cd` target found no resolvable path, fell back to the hook's
    # own /worktrees/ payload cwd, and wrongly exempted. A single-quoted
    # literal must resolve to its real (masked-away) content and still
    # deny; the genuine quoted worktree-local case must still abstain.
    dict(
        id="git-stash-shared-checkout",
        decision="deny",
        tool_name="Bash",
        positive={"command": "cd '/shared/checkout' && git stash"},
        cwd=WORKTREE_CWD,
        near_miss={"command": "cd '/worktrees/x/sub' && git stash"},
        near_miss_cwd=DEFAULT_CWD,
    ),
    # F8: a double-quoted variable still expands at runtime, so it can't be
    # resolved to a literal path -- uncertain must keep protection (deny),
    # same as the unquoted $VAR case.
    dict(
        id="git-stash-shared-checkout",
        decision="deny",
        tool_name="Bash",
        positive={"command": 'cd "$DIR" && git stash'},
        cwd=WORKTREE_CWD,
        near_miss={"command": 'cd "/worktrees/x/sub" && git stash'},
        near_miss_cwd=DEFAULT_CWD,
    ),
    # F7 (review round 2 redo): `finditer` never revisits text inside an
    # already-yielded span, even a REJECTED one -- a real polling loop
    # placed after an earlier, unrelated loop's own `done` was never tried
    # as a match start once the first (unbounded, two-`done`) candidate got
    # rejected. Must still ask; the out-of-loop gh call near miss stays
    # abstain.
    dict(
        id="gh-fast-polling",
        decision="ask",
        tool_name="Bash",
        positive={
            "command": "while read x; do echo $x; done < f\nwhile true; do gh pr checks 1; sleep 5; done"
        },
        near_miss={
            "command": (
                "while read x; do echo $x; done < f\n"
                "gh pr checks 123\n"
                "sleep 5\n"
                "for p in 1 2; do echo; done"
            )
        },
    ),
    # G1 (review_b round 1): `_effective_checkout` used to pick up the LAST
    # `cd` anywhere earlier in the text even when its effect never actually
    # reaches the later invocation -- a subshell-local `cd` (closed by its
    # own `)` before the stash runs) or a `cd` guarded by `||` (only the
    # stash runs, which means the `cd` FAILED) must not leak into the
    # effective checkout. The near miss: `cd` and the stash both inside the
    # SAME subshell -- that subshell-local checkout genuinely governs.
    dict(
        id="git-stash-shared-checkout",
        decision="deny",
        tool_name="Bash",
        positive={"command": "(cd /worktrees/x); git stash"},
        near_miss={"command": "(cd /worktrees/x; git stash)"},
    ),
    dict(
        id="git-stash-shared-checkout",
        decision="deny",
        tool_name="Bash",
        positive={"command": "cd /worktrees/x || git stash"},
        near_miss={"command": "(cd /worktrees/x; git stash)"},
    ),
    # G2 (review_b round 1): `executable_mask` used to blank an entire
    # quoted span to spaces, erasing the literal argument content along
    # with it -- a quoted leading `+` refspec or a quoted destructive flag
    # abstained even though the shell passes that exact text through
    # unquoted. Near miss: a quoted but non-forcing refspec / a
    # destructive-looking flag spelled out as literal TEXT to an unrelated
    # subcommand must keep abstaining.
    dict(
        id="git-push-force",
        decision="ask",
        tool_name="Bash",
        positive={"command": "git push origin '+HEAD:main'"},
        near_miss={"command": "git push origin 'main'"},
    ),
    dict(
        id="git-destructive-ask",
        decision="ask",
        tool_name="Bash",
        positive={"command": "git reset HEAD~1 '--hard'"},
        near_miss={"command": "git commit -m 'reset --hard would be bad here'"},
    ),
    dict(
        id="git-destructive-ask",
        decision="ask",
        tool_name="Bash",
        positive={"command": "git clean '--force' -d"},
        near_miss={"command": "git commit -m 'reset --hard would be bad here'"},
    ),
    # G3 (review_b round 1): `_polling_loop_bounded` rejected any candidate
    # with more than one `done` token, which also rejected a genuine OUTER
    # polling loop merely CONTAINING a fully-closed nested loop (two
    # `done`s, both legitimate). The near miss (two UNRELATED sibling
    # loops, F3/F7's actual concern) stays abstain.
    dict(
        id="gh-fast-polling",
        decision="ask",
        tool_name="Bash",
        positive={
            "command": "while true; do\n  for i in 1 2; do\n    echo $i\n  done\n  gh pr checks 123\n  sleep 5\ndone"
        },
        near_miss={
            "command": (
                "while read x; do echo $x; done < f\n"
                "gh pr checks 123\n"
                "sleep 5\n"
                "for p in 1 2; do echo; done"
            )
        },
    ),
    # Review R4 (regression from G2): `_mask_literal_span` excluded a REAL
    # newline from blanking, so a multi-line quoted literal's second line
    # sat at a fresh line-start and `_CMD_PREFIX`'s `\n\s*` alternative
    # anchored it as if it were a brand new command -- a multi-line `-m`
    # message merely mentioning `git stash` denied, and a multi-line quoted
    # `echo` argument mentioning `git push --force` asked. Both must stay
    # inert, same as their single-line equivalents.
    dict(
        id="git-stash-shared-checkout",
        decision="deny",
        tool_name="Bash",
        positive={"command": "git stash"},
        near_miss={"command": "git commit -m 'fix\n\ngit stash was wrong'"},
    ),
    dict(
        id="git-push-force",
        decision="ask",
        tool_name="Bash",
        positive={"command": "git push origin +HEAD:main"},
        near_miss={"command": "echo 'x\ngit push --force'"},
    ),
    # G4 (review_b round 2): `_mask_double_quoted` had its own inline
    # masking loop and was missed by the R4 fix above -- a REAL newline
    # inside a DOUBLE-quoted span stayed visible, so a fake `cd
    # /worktrees/x` mentioned in a quoted `echo` argument got picked up by
    # `_effective_checkout` as a genuine `cd` and wrongly exempted a real
    # `git stash` that runs from an actual shared (non-worktree) checkout.
    # Near miss: the double-quoted equivalent of the R4 multi-line mention
    # above must stay inert too.
    dict(
        id="git-stash-shared-checkout",
        decision="deny",
        tool_name="Bash",
        cwd="/shared/checkout",
        positive={"command": 'echo "x\ncd /worktrees/x"; git stash'},
        near_miss={"command": 'git commit -m "fix\n\ngit stash was wrong"'},
        near_miss_cwd="/shared/checkout",
    ),
    dict(
        id="git-push-force",
        decision="ask",
        tool_name="Bash",
        positive={"command": "git push origin +HEAD:main"},
        near_miss={"command": 'echo "x\ngit push --force"'},
    ),
    # G4 (review_b round 2, re-opened): the backslash-escape branch inside
    # `_mask_double_quoted` special-cased `\<newline>` (a real shell line
    # continuation -- the shell deletes both characters, joining the two
    # source lines into one) by leaving BOTH characters unmasked, so the
    # embedded real newline never reached the `_SEPARATOR_CHARS` branch
    # that the fixture above already covers for a BARE embedded newline.
    # `echo "x\` + newline + `cd /worktrees/x"` is exactly one quoted
    # string with no executable `cd` at all -- same bug, different escape
    # path into it. Near miss: the same backslash-newline join in a commit
    # message mentioning `git stash` must stay inert too.
    dict(
        id="git-stash-shared-checkout",
        decision="deny",
        tool_name="Bash",
        cwd="/shared/checkout",
        positive={"command": 'echo "x\\\ncd /worktrees/x"; git stash'},
        near_miss={"command": 'git commit -m "fix\\\ngit stash was wrong"'},
        near_miss_cwd="/shared/checkout",
    ),
    # G5 (review_b round 2): a nested bounded loop placed AFTER the
    # `gh`+sleep pair (rather than before, G3's case) left the lazy
    # gh-fast-polling candidate UNCLOSED at the nested loop's own `done`
    # instead of reaching the outer loop's real terminator further out --
    # `_polling_loop_extent` must be extended to the next `done`, not
    # discarded. Near miss: the same unrelated-sibling-loops case G3 uses,
    # which must keep abstaining regardless of nesting placement.
    dict(
        id="gh-fast-polling",
        decision="ask",
        tool_name="Bash",
        positive={
            "command": "while true; do\n  gh pr checks 123\n  sleep 5\n  for i in 1 2; do\n    echo $i\n  done\ndone"
        },
        near_miss={
            "command": (
                "while read x; do echo $x; done < f\n"
                "gh pr checks 123\n"
                "sleep 5\n"
                "for p in 1 2; do echo; done"
            )
        },
    ),
]


def run(payload):
    env = dict(os.environ)
    env["HEX_LEDGER_DIR"] = str(ledger_dir)
    if router_impl == "rust":
        env["HEX_DIR"] = repo_root
        cmd = [hex_router_bin, "hook", "router"]
    else:
        cmd = [sys.executable, router]
    return subprocess.run(
        cmd,
        input=json.dumps(payload),
        capture_output=True,
        text=True,
        env=env,
        timeout=5,
    )


def classify(proc):
    """Return the winning decision string, or 'abstain'/'error:<detail>'."""
    if proc.returncode != 0:
        return f"error:exit={proc.returncode}"
    out = proc.stdout.strip()
    if not out:
        return "abstain"
    try:
        doc = json.loads(out)
        hso = doc["hookSpecificOutput"]
    except Exception as exc:
        return f"error:parse:{exc}"
    pd = hso.get("permissionDecision")
    if pd in ("deny", "ask"):
        return pd
    if hso.get("additionalContext"):
        return "prior"
    return "error:unknown-shape"


failures = 0
ledger_path = ledger_dir / "router-fires.jsonl"


def read_ledger():
    if not ledger_path.exists():
        return []
    return [l for l in ledger_path.read_text().splitlines() if l.strip()]


ALL_FIXTURES = [(fx["id"], fx) for fx in RULE_FIXTURES]
extra_seen = {}
for fx in EXTRA_FIXTURES:
    extra_seen[fx["id"]] = extra_seen.get(fx["id"], 0) + 1
    ALL_FIXTURES.append((f"{fx['id']}-extra{extra_seen[fx['id']]}", fx))

for label, fx in ALL_FIXTURES:
    # Positive fixture: must fire with the rule's declared decision AND must
    # append exactly one ledger line stamped with THIS rule's own id — not
    # merely produce the right decision (which another rule could also
    # produce) and not merely bump a global counter another fire could pad.
    before = read_ledger()
    proc = run(make_payload(fx["tool_name"], fx["positive"], cwd=fx.get("cwd", DEFAULT_CWD)))
    got = classify(proc)
    decision_ok = got == fx["decision"]

    after = read_ledger()
    new_lines = after[len(before):]
    ledger_ok = False
    if len(new_lines) == 1:
        try:
            rec = json.loads(new_lines[0])
            ledger_ok = (
                rec.get("rule_id") == fx["id"] and rec.get("decision") == fx["decision"]
            )
        except Exception:
            ledger_ok = False

    ok = decision_ok and ledger_ok
    detail = "" if ok else f" (decision_ok={decision_ok} ledger_ok={ledger_ok} new_lines={len(new_lines)})"
    print(f"{label} expected={fx['decision']} got={got} {'PASS' if ok else 'FAIL'}{detail}")
    if not ok:
        failures += 1

    # Near-miss fixture: must abstain (empty stdout, exit 0) and write
    # nothing to the ledger.
    before = read_ledger()
    nm_tool = fx.get("near_miss_tool_name", fx["tool_name"])
    nm_input = fx.get("near_miss_input", fx.get("near_miss"))
    nm_cwd = fx.get("near_miss_cwd", DEFAULT_CWD)
    proc = run(make_payload(nm_tool, nm_input, cwd=nm_cwd))
    got = classify(proc)
    after = read_ledger()
    ok = got == "abstain" and len(after) == len(before)
    print(f"{label}-near-miss expected=abstain got={got} {'PASS' if ok else 'FAIL'}")
    if not ok:
        failures += 1

lines = read_ledger()
expected_count = len(ALL_FIXTURES)
count_ok = len(lines) == expected_count
print(
    f"ledger-count expected={expected_count} got={len(lines)} "
    f"{'PASS' if count_ok else 'FAIL'}"
)
if not count_ok:
    failures += 1

sys.exit(1 if failures else 0)
PYEOF
status=$?
exit "$status"
