#!/usr/bin/env bash
# Basic test for spawn-agent.sh.
#
# Verifies, without hitting the Anthropic API:
#   1. frontmatter stripping produces a non-empty body that excludes YAML keys
#   2. unknown agent name fails with exit 1
#   3. missing argument fails with exit 2
#   4. worktree + branch are created under a throwaway target repo
#   5. the final `claude …` invocation is assembled with the expected flags
#
# Strategy: shim `claude` on PATH with a recorder that dumps its argv to a file
# and exits 0. The real CLI is never called.

set -euo pipefail

REPO="$(git -C "$(dirname "$0")/.." rev-parse --show-toplevel)"
SPAWN="$REPO/scripts/spawn-agent.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"; [[ -n "${TARGET:-}" ]] && git -C "$TARGET" worktree list --porcelain 2>/dev/null | awk "/^worktree/ {print \$2}" | grep -v "^$TARGET\$" | xargs -I{} git -C "$TARGET" worktree remove --force {} 2>/dev/null || true; rm -rf "${TARGET:-}" "${TARGET:-}"-*' EXIT

pass() { printf "  \033[32mok\033[0m   %s\n" "$1"; }
fail() { printf "  \033[31mFAIL\033[0m %s\n" "$1"; exit 1; }

# ---- Test 1: missing arg -----------------------------------------------------
echo "test: missing agent name exits 2"
set +e
"$SPAWN" >/dev/null 2>&1
rc=$?
set -e
[[ $rc -eq 2 ]] && pass "exit 2" || fail "expected 2, got $rc"

# ---- Test 2: unknown agent ---------------------------------------------------
echo "test: unknown agent exits 1"
set +e
"$SPAWN" no-such-agent-xyz "task" >/dev/null 2>&1
rc=$?
set -e
[[ $rc -eq 1 ]] && pass "exit 1" || fail "expected 1, got $rc"

# ---- Test 3: frontmatter stripping -------------------------------------------
echo "test: frontmatter stripping"
BODY="$(awk 'BEGIN{f=0} /^---$/{f++; next} f>=2{print}' "$REPO/agents/engineer.md")"
[[ -n "$BODY" ]]                                    || fail "body empty"
grep -q "^name: engineer" <<<"$BODY"                && fail "body still contains YAML 'name:'"
grep -q "senior software engineer" <<<"$BODY"       || fail "body missing identity text"
pass "frontmatter removed, identity preserved ($(wc -l <<<"$BODY" | tr -d ' ') lines)"

# ---- Test 4: end-to-end with a claude shim -----------------------------------
echo "test: worktree creation + claude invocation (shimmed)"
TARGET="$TMP/target-repo"
mkdir -p "$TARGET"
git -C "$TARGET" init -q -b main
git -C "$TARGET" commit -q --allow-empty -m init

# Shim claude: record argv, exit 0.
SHIM_DIR="$TMP/bin"
mkdir -p "$SHIM_DIR"
RECORD="$TMP/claude-argv.txt"
cat >"$SHIM_DIR/claude" <<EOF
#!/usr/bin/env bash
printf '%s\n' "\$@" > "$RECORD"
exit 0
EOF
chmod +x "$SHIM_DIR/claude"

# Run spawn script from inside the target repo with shimmed PATH.
(
  cd "$TARGET"
  PATH="$SHIM_DIR:$PATH" "$SPAWN" engineer "hello task" >/dev/null
)

[[ -f "$RECORD" ]] || fail "claude shim was not invoked"

# Check the recorded args.
grep -qx -- "--permission-mode"    "$RECORD" || fail "missing --permission-mode"
grep -qx -- "bypassPermissions"    "$RECORD" || fail "missing bypassPermissions value"
grep -qx -- "--append-system-prompt" "$RECORD" || fail "missing --append-system-prompt"
grep -qx -- "-p"                   "$RECORD" || fail "missing -p"
grep -qx -- "hello task"           "$RECORD" || fail "missing task string"
pass "claude invoked with correct flags"

# Verify worktree + branch were created.
WT="$(git -C "$TARGET" worktree list --porcelain | awk '/^worktree/ {print $2}' | grep -v "^$TARGET\$" | head -1)"
[[ -n "$WT" && -d "$WT" ]] || fail "worktree not created"
BR="$(git -C "$TARGET" branch --list 'agent/engineer/*' | head -1 | tr -d ' *')"
[[ -n "$BR" ]] || fail "agent branch not created"
pass "worktree=$WT branch=$BR"

echo
echo "all tests passed"
