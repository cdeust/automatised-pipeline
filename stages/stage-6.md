# Stage 6 — `validate_prd_against_graph`

**Status:** spec (pre-implementation). Consumes the PRD produced by `prd-spec-generator` (stage 5) and the resolved+clustered graph (stages 3a-3c). Emits a validation report.

**Source:** NOTES.md stage 6 row; architect's open question on value-add vs multi-judge verification; stages/stage-3c.md community schema; src/prd_input.rs matched-symbol contract.

---

## 1. Shannon framing

**Question.** What defects can a *graph-aware* validator catch in a PRD that a *multi-judge LLM panel* (which prd-spec-generator already runs) cannot?

**The quantity.** Let a PRD P claim a set of changes C = {c_i}. Each claim c_i has a *truth condition* T(c_i) defined against the graph G. Define:

```
validation_error_rate(P, G) = |{c ∈ C : T(c) is FALSE against G}| / |C|
```

A multi-judge panel of LLMs *approximates* T(c) by pattern-matching against training data. We *compute* T(c) directly against G. The multi-judge panel has no ground-truth access to G; we do. **This is the value-add, and it is quantifiable.**

**Operational definition.** T(c) is a Cypher query against G that returns true or false. The validator runs N such queries (one per extractable claim) and reports the failures.

**Layer decomposition:**
- **Source:** PRD markdown/JSON + resolved graph G.
- **Channel:** symbol-claim extractor (PRD → structured claims).
- **Code:** the validator's claim→Cypher mapping.

---

## 2. What the graph catches that multi-judge misses

Three **concrete counter-examples** where a graph-aware check beats a multi-judge LLM panel. These are the load-bearing justifications; if none of these hold, this stage is redundant.

### 2.1 Counter-example A — Symbol hallucination

**Scenario.** PRD says: *"Modify `handle_tool_call` in `src/main.rs` to add retry logic around the `do_cluster_graph` branch."*

**Multi-judge LLM panel behavior.** Judges see `handle_tool_call` and `do_cluster_graph`, recognize them as plausible Rust identifiers following the project's convention (from context they may have seen), and rate the PRD as internally consistent. They cannot check whether either symbol actually exists in the current HEAD of the repo.

**Graph behavior.**
```cypher
MATCH (n) WHERE n.qualified_name = 'src/main.rs::handle_tool_call' RETURN n
```
If this returns zero rows, the PRD is referencing a symbol that does not exist. **Fail.** If the symbol was renamed in a recent commit (e.g., to `handle_tool_invocation`), the graph knows; the panel does not.

**Why this is non-redundant.** LLMs hallucinate plausible-sounding symbol names routinely. The multi-judge setup catches contradictions between judges but not shared hallucinations (all judges have the same priors).

### 2.2 Counter-example B — Community-consistency violation

**Scenario.** PRD claims: *"Changes are scoped to the graph-indexing subsystem."*

**Multi-judge LLM panel behavior.** Judges read the claim, read the change list, and rate it plausible if the filenames look consistent. They have no formal notion of "subsystem."

**Graph behavior.** The graph has computed communities (stage 3c, Leiden/Louvain). For each claimed-changed symbol s_i, look up its MemberOf_*_Community edge. If |distinct communities touched| > threshold (e.g., 2), the scope claim is false.

```cypher
MATCH (s)-[:MemberOf_Function_Community|:MemberOf_Method_Community|...]->(c:Community)
WHERE s.qualified_name IN [$changed_symbols]
RETURN DISTINCT c.id, count(s)
```

**Concrete defect this catches.** A PRD that says "small refactor to the search module" but actually touches the graph_store, resolver, clustering, and search modules is scope-creeping. The panel's "plausibility" heuristic passes this; the community count fails it hard.

### 2.3 Counter-example C — Process-impact contradiction

**Scenario.** PRD says: *"This change does not affect the `main` entry-point execution path."*

**Multi-judge LLM panel behavior.** Judges cannot trace call chains. They take the claim at face value unless the code names are blatantly contradictory.

**Graph behavior.** Stage 3c built Process nodes with ParticipatesIn edges from every entry point. Query: does any changed symbol participate in `process::src/main.rs::main`?

```cypher
MATCH (s)-[:ParticipatesIn_Function_Process|:ParticipatesIn_Method_Process]->(p:Process)
WHERE s.qualified_name IN [$changed_symbols] AND p.name = 'process::src/main.rs::main'
RETURN s.qualified_name, p.name
```

If any row returns, the PRD's claim is contradicted by the call graph. **This is structural ground truth** — no amount of LLM reasoning recovers it without the graph.

---

## 3. What we do NOT validate

Explicitly out of scope (owned by prd-spec-generator's multi-judge):
- Prose quality, tone, formatting, completeness of the problem statement.
- Business value justification, stakeholder rationale, KPI selection.
- Feasibility of non-code claims (timelines, team readiness, rollout plan).
- Semantic correctness of the *proposed fix* — we don't know if the PR is right, only whether its claimed surface matches the graph.

**Zetetic guardrail.** If a defect axis cannot be expressed as a Cypher query over G, it is NOT in this stage's scope. Route it back to the multi-judge panel.

---

## 4. The symbol-extraction challenge

The PRD is free-form markdown. To run graph queries we need a list of symbol claims. This is the non-obvious design decision.

### 4.1 Three options considered

| Option | Mechanism | Recall | Precision | Contract impact |
|---|---|---|---|---|
| A. Regex-only | Extract backticked identifiers, qualified names, file paths from PRD text | Medium | Medium (false positives from prose backticks like `TODO`) | None — PRD stays free-form |
| B. Structured section | Require PRD to contain a machine-parseable "Affected Symbols" block | High | High | PRD generator must emit the section |
| C. LLM re-extraction | Call an LLM to extract symbol claims | High | High but costly + non-deterministic | None, but violates stage's "no LLM" rule |

### 4.2 Decision — B + A fallback

**Primary:** require the PRD generator to produce a structured section. **Fallback:** regex extraction when the section is missing or empty.

**Rationale.** Option B gives high precision/recall and keeps this stage LLM-free (axiom). Option A alone has noisy false positives (e.g., `` `JSON` `` in prose). Option C violates the stage's purity constraint.

**Contract with prd-spec-generator.** The PRD (or a companion JSON sidecar the generator already produces as part of its 9-file export) must contain:

```yaml
affected_symbols:
  - qualified_name: "src/main.rs::handle_tool_call"
    change_kind: "modify"        # add | modify | remove | rename
    rationale: "add retry logic"
  - qualified_name: "src/clustering.rs::cluster_graph"
    change_kind: "modify"
    rationale: "invoke retry wrapper"
scope_claims:
  - kind: "community_scope"
    assertion: "graph-indexing"   # a human-readable label; we match against Community.name or fall back to "no assertion"
  - kind: "process_exclusion"
    processes: ["process::src/main.rs::main"]  # processes the PRD claims NOT to affect
```

**Contract location.** The prd-spec-generator already outputs multiple JSON files; we require one named `stage-5.affected_symbols.json` alongside the main PRD. If absent, stage 6 degrades to regex-only mode with a `contract_missing: true` warning at the top of the report (informational, not fail).

### 4.3 Regex fallback rules

Only triggered when `stage-5.affected_symbols.json` is missing.

| Pattern | Regex | Example match |
|---|---|---|
| Backticked qualified name | `` `([A-Za-z_][A-Za-z0-9_]*::[A-Za-z_:0-9]+)` `` | `` `graph_store::GraphStore::insert_node` `` |
| Backticked identifier (snake_case or CamelCase) | `` `([A-Za-z_][A-Za-z0-9_]{2,})` `` | `` `handle_tool_call` ``, `` `GraphStore` `` |
| File path with extension | `([A-Za-z0-9_/\-.]+\.(rs\|ts\|py\|go))` | `src/main.rs` |

For each extracted token, attempt graph lookup by qualified_name (exact) → by name (fuzzy, case-sensitive). Tokens with zero matches are either (a) external references or (b) hallucinations; both are reported as `unresolved_claims` with severity `info`.

**Precision-recall stance.** Regex fallback is deliberately **low-precision, high-recall**. False positives land as `info`-level findings the reviewer can dismiss; false negatives (missed hallucinations) are the catastrophic failure mode we must avoid.

---

## 5. Validation axes — tractable subset

Five axes. Each produces a `finding` with a severity.

| # | Axis | Query | Severity on failure | Notes |
|---|---|---|---|---|
| V1 | **Symbol existence** | `MATCH (n) WHERE n.qualified_name = $qn RETURN n` per affected symbol | `fail` if `change_kind = modify\|remove\|rename` and row count = 0; `info` if `change_kind = add` and row count > 0 (symbol already exists) | Highest-value axis. Catches hallucinations directly. |
| V2 | **Community scope** | Count distinct communities touched by affected symbols | `warning` if count > 2 and PRD asserts community_scope; `info` otherwise | Requires communities to exist (stage 3c ran). Skip with `info` if communities absent. |
| V3 | **Process-exclusion contradiction** | For each claimed-excluded process, check ParticipatesIn edges from affected symbols | `fail` if any excluded process has participants; `info` per participant | Structural ground truth. |
| V4 | **Dependency plausibility** | For each `add` symbol, check that at least one proposed Call/Uses target exists in the graph | `warning` if a claimed target does not exist | Catches "depends on non-existent helper" claims. |
| V5 | **Scope-size sanity** | Compare `len(affected_symbols)` vs PRD's self-description keywords (`small fix`, `minor`, `refactor`, `rewrite`) | `warning` if size/claim mismatch (e.g., "small fix" with >10 affected symbols) | Purely heuristic; `info` unless combined with V2 warning. |

**Why these five and not more.** Each axis maps to a single-query Cypher pattern with a clear failure signal. Additional axes (e.g., "does the claimed fix preserve the trait hierarchy?") require semantic understanding of the change diff, which is stage 9's job (verify_semantic_diff, post-implementation). Stage 6 is pre-implementation.

---

## 6. Tool contract

### 6.1 Input schema

```json
{
  "name": "validate_prd_against_graph",
  "inputSchema": {
    "type": "object",
    "required": ["run_id", "finding_id", "output_dir", "graph_path"],
    "properties": {
      "run_id":       { "type": "string" },
      "finding_id":   { "type": "string" },
      "output_dir":   { "type": "string", "pattern": "^/.+" },
      "graph_path":   { "type": "string", "pattern": "^/.+" },
      "prd_path":     { "type": "string", "description": "Optional. Default: <output_dir>/runs/<run_id>/findings/<finding_id>/stage-5.prd.md" },
      "affected_symbols_path": { "type": "string", "description": "Optional. Default: <output_dir>/runs/<run_id>/findings/<finding_id>/stage-5.affected_symbols.json" }
    }
  }
}
```

### 6.2 Output artifact — `stage-6.validation.json`

```json
{
  "run_id": "...",
  "finding_id": "...",
  "prd_path": "...",
  "graph_path": "...",
  "validated_at": "2026-04-11T...",
  "contract_missing": false,
  "extraction_mode": "structured",  // "structured" | "regex_fallback"
  "affected_symbol_count": 7,
  "scope_claim_count": 2,
  "findings": [
    {
      "axis": "V1",
      "severity": "fail",
      "qualified_name": "src/main.rs::handle_tool_call",
      "message": "Symbol claimed in affected_symbols does not exist in graph",
      "details": { "change_kind": "modify", "matches_by_name": 0 }
    },
    ...
  ],
  "validation_status": "fail",  // "ok" | "warning" | "fail"
  "summary_counts": { "fail": 1, "warning": 2, "info": 3 }
}
```

### 6.3 Status rule

- Any `fail` → `validation_status = "fail"`.
- No `fail`, ≥1 `warning` → `validation_status = "warning"`.
- Otherwise → `validation_status = "ok"`.

`info` findings never escalate.

---

## 7. Invariants

| # | Invariant | Verification |
|---|---|---|
| I1 | Stage 6 is pre-implementation. It does not read any diff, PR, or post-change state. | No filesystem reads outside `stage-5.*` and the graph. |
| I2 | Stage 6 is LLM-free. | No network calls, no subprocesses to anthropic/openai/etc. Enforced by construction. |
| I3 | Stage 6 is read-only w.r.t. the graph. | No CREATE/MERGE/DELETE/SET in any query. |
| I4 | Every finding has a severity in `{info, warning, fail}`. | Schema validation. |
| I5 | Validation is deterministic given the same PRD + same graph. | No timestamps in query logic; extraction is pure. |
| I6 | `validation_status = "ok"` implies zero fail-severity findings. | Post-run assertion. |

---

## 8. Implementation steps

| Step | Description | Smoke test | LOC |
|---|---|---|---|
| 1 | **Schema wiring.** Add `validate_prd_against_graph_schema()` to tool_schemas.rs. Add `do_validate_prd_against_graph` match arm. | `tools/list` returns the tool. | ~30 |
| 2 | **Structured-section loader.** Parse `stage-5.affected_symbols.json`. Degrade to regex when missing. | Load a sample file; extract 3 symbols. | ~40 |
| 3 | **Regex fallback extractor.** Implement §4.3 patterns. | Test: "modify `handle_tool_call` in `src/main.rs`" yields 2 tokens. | ~40 |
| 4 | **V1 symbol existence.** Per-symbol existence query, change_kind-aware. | Known-missing symbol → fail; known-existing → ok. | ~40 |
| 5 | **V2 community scope.** Distinct-community query + scope claim comparison. | Symbols across 3 communities + claim "single subsystem" → warning. | ~40 |
| 6 | **V3 process-exclusion.** Per-excluded-process participation query. | Symbol participates in excluded process → fail. | ~30 |
| 7 | **V4 dependency plausibility.** Per-`add` symbol Calls/Uses target check. | `add` symbol referencing non-existent target → warning. | ~30 |
| 8 | **V5 scope-size sanity.** Keyword match vs symbol count. | `"small fix"` + 20 symbols → warning. | ~20 |
| 9 | **Report assembler + artifact writer.** Apply §6.3 status rule, write `stage-6.validation.json`. | End-to-end run produces valid JSON matching schema. | ~40 |
| 10 | **Index update.** Append `stage6_path`, `stage6_validated_at`, `stage6_status` to index.json. | Index contains the three keys after run. | ~20 |

Total estimate: **~330 LOC** in new `src/prd_validator.rs`. Extract to module from day one; this is heavier than a single `handle_tool_call` arm.

---

## 9. Open questions

1. **Does prd-spec-generator already emit a structured affected-symbols file?** If yes, use its exact schema. If no, §4.2's contract requires an agreement with that skill's owner. **Resolution: check prd-spec-generator's 9-file export list before implementing step 2. If absent, file an enhancement request there AND ship regex fallback first.**
2. **Threshold for V2 community-count warning.** Proposed: warn if distinct communities > 2. **Defer tuning:** ship with 2; revisit after first 10 real PRDs.
3. **Scope keywords for V5.** Initial list: `small`, `minor`, `refactor`, `quick`, `trivial`, `one-liner`. **Deferred:** make configurable via an optional `scope_keywords` parameter; ship with a hardcoded list.
4. **Degrade when stage 3c hasn't run?** V2 requires communities. **Resolution:** detect absence, emit `info` finding "community scope validation skipped: no Community nodes", continue other axes.

---

## 10. Source citations

| Claim | Source |
|---|---|
| Multi-judge panels can't check ground truth | prd-spec-generator skill description; architect's brief. |
| Community schema | stages/stage-3c.md §4.1 |
| Process schema | stages/stage-3c.md §4.1, §5.2 |
| MemberOf edge tables | stages/stage-3c.md §4.2 |
| ParticipatesIn edge tables | stages/stage-3c.md §4.2 |
| PRD generator output format | ai-prd-generator:generate-prd skill; "9-file export" |
| Determinism invariant | stages/stage-3.md §13 invariant 1 |
| Regex precision/recall tradeoff reasoning | Jurafsky & Martin, Speech and Language Processing 3e, §2.1 (pattern matching, false-positive analysis) |
