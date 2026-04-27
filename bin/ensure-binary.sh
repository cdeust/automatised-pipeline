#!/usr/bin/env bash
# ensure-binary.sh — guarantee target/release/automatised-pipeline exists.
#
# Used by:
#   - session-start.sh hook (runs synchronously, BEFORE MCP servers
#     attempt to connect). Builds once on first install.
#   - .mcp.json launcher (re-invokes if binary disappeared between
#     sessions; fails fast with a clear stderr message rather than
#     silently calling `cargo run` which exceeds MCP startup timeout).
#
# Behaviour
# ---------
# 1. If `target/release/automatised-pipeline` exists AND is newer than every
#    file under `src/` and `Cargo.{toml,lock}`, exit 0 immediately.
# 2. Otherwise, run `cargo build --release --quiet`. Prints progress to
#    stderr only — stdout stays clean for the MCP launcher pipeline.
# 3. On build failure, exit 1 with an actionable message.
#
# Determinism: zero side effects beyond `cargo build`. Idempotent. No
# global state. Safe to invoke from any hook or launcher.

set -euo pipefail

ROOT="${CLAUDE_PLUGIN_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
BIN="$ROOT/target/release/automatised-pipeline"
MANIFEST="$ROOT/Cargo.toml"
SRC_DIR="$ROOT/src"

# Quiet mode: caller is the MCP launcher; suppress progress noise.
# Verbose mode: caller is a session-start hook or human; show progress.
MODE="${1:-quiet}"

log() {
    if [ "$MODE" = "verbose" ]; then
        echo "$@" >&2
    fi
}

err() {
    echo "automatised-pipeline: $*" >&2
}

# Need cargo available in PATH.
if ! command -v cargo >/dev/null 2>&1; then
    err "FATAL: cargo not found in PATH. Install Rust (https://rustup.rs)"
    err "       and re-run this command. Plugin cannot start without"
    err "       a built binary."
    exit 127
fi

# Manifest sanity.
if [ ! -f "$MANIFEST" ]; then
    err "FATAL: Cargo.toml missing at $MANIFEST"
    exit 1
fi

# Fast path: binary exists and is fresh relative to all sources.
needs_build="no"
if [ ! -x "$BIN" ]; then
    needs_build="missing"
else
    # Compare mtimes: rebuild if any source file is newer than the binary.
    # `find -newer` returns the first match; head -1 to short-circuit.
    if [ -d "$SRC_DIR" ]; then
        newer=$(find "$SRC_DIR" "$MANIFEST" "$ROOT/Cargo.lock" \
                    -newer "$BIN" -print -quit 2>/dev/null || true)
        if [ -n "$newer" ]; then
            needs_build="stale"
        fi
    fi
fi

if [ "$needs_build" = "no" ]; then
    log "automatised-pipeline: binary up-to-date at $BIN"
    exit 0
fi

# Build path. Cargo writes its own progress to stderr; we add a
# bracketing message so the user knows what's happening.
err "Building ai-architect-mcp (reason: $needs_build)…"
err "  manifest: $MANIFEST"
err "  output:   $BIN"
err "  this can take 2–3 minutes on first install."

start_ts=$(date +%s)
if cargo build --release --quiet --manifest-path "$MANIFEST" >&2; then
    end_ts=$(date +%s)
    elapsed=$((end_ts - start_ts))
    err "automatised-pipeline: build OK in ${elapsed}s"
    if [ ! -x "$BIN" ]; then
        err "FATAL: cargo build succeeded but $BIN is not executable."
        err "       Check Cargo.toml [[bin]] target name == 'automatised-pipeline'."
        exit 1
    fi
    exit 0
else
    err "FATAL: cargo build failed. Run manually for full output:"
    err "       cargo build --release --manifest-path '$MANIFEST'"
    exit 1
fi
