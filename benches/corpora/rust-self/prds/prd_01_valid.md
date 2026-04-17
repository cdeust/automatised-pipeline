# PRD-01: Add structured logging to `handle_tool_call`

## Summary

Add structured logging statements inside `handle_tool_call` to emit a JSON
log line per tool invocation. This is a localized change scoped to the
MCP dispatch entry point in `src/main.rs`.

## Scope

- Modify `handle_tool_call` to write a log line for each tool dispatch.
- No other symbols in the codebase need to be touched.

## Affected symbols

- `handle_tool_call` (change_kind: modify)

All affected symbols are in the `src/main.rs` dispatch community. The
clustering layer, parser layer, and graph-storage layer are out of scope
and must not be touched by this change.
