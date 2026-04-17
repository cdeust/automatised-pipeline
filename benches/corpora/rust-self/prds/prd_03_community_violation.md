# PRD-03: Parser-only refactor (falsely scoped)

## Summary

This PRD claims to be a parser-only refactor. It asserts that the scope
is limited to the parser community. In reality, it touches symbols
spread across three different communities: parser, search, and
clustering. The validator should flag a community_consistency violation.

## Scope

- Parser-only refactor. Only `src/parser/*` is affected.

## Affected symbols

- `parse_rust_file` (change_kind: modify) — lives in parser community.
- `search_graph` (change_kind: modify) — lives in search community.
- `cluster_graph` (change_kind: modify) — lives in clustering community.

Because the affected symbols span >= 2 distinct Leiden communities while
the scope assertion claims only the parser community, the validator
should emit a community_consistency finding with severity warning or
critical (depending on the span threshold).
