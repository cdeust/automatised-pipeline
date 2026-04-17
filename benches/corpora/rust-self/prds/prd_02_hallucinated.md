# PRD-02: Rename `totally_fake_symbol_99` to `new_name_for_fake_symbol`

## Summary

Rename the symbol `totally_fake_symbol_99` to `new_name_for_fake_symbol`
across the codebase. This symbol is believed to live in `src/main.rs`.

## Scope

- Rename `totally_fake_symbol_99` everywhere it is referenced.

## Affected symbols

- `totally_fake_symbol_99` (change_kind: rename)

This PRD intentionally names a symbol that does not exist in the graph —
the validator should flag this as a symbol_hallucination with critical
severity, because `change_kind = rename` against a non-existent symbol
is an incoherent claim.
