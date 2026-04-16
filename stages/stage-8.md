# Stage 8 — `check_security_gates`

**Status:** spec (pre-implementation). Graph-aware security gate. Runs between the coding agent's implementation (stage 7) and the semantic-diff verification (stage 9). Operates purely on the graph + a list of changed symbols.

**Source:** NOTES.md stage 8 row; architect's scope constraint ("graph-aware only"); stages/stage-3b.md resolver `unresolved_refs`; stages/stage-3c.md community schema.

---

## 1. Shannon framing

**Question.** What security-relevant properties of a proposed change can be checked by graph queries alone — no subprocess to `cargo audit`, no LLM judge, no external scanner?

**The quantity.** For a set of changed symbols Δ = {s_1, ..., s_n}, define the *graph risk signature*:

```
risk(Δ, G) = ( touches_auth(Δ,G), touches_unsafe(Δ,G), introduces_unresolved(Δ,G),
              touches_public_api(Δ,G), coverage_gap(Δ,G) )
```

Each coordinate is binary or a count. The gate is a conjunction: pass iff all critical coordinates are clean.

**Operational definition.** `gates_passed = true` iff no `critical` flag is raised. Every flag is tied to one Cypher query whose output is interpreted by a fixed rule. Refined-with-the-graph version of a BOM-style pre-merge check.

**Layer decomposition:**
- **Source:** graph G + changed_symbols list Δ (from stage 4 impact or from a git diff → qualified_name mapping).
- **Channel:** LadybugDB Cypher.
- **Code:** the five check functions, each a query + rule.

**What we explicitly DON'T do** (non-redundancy constraint):
- We do **NOT** scan dependency versions (that's `cargo audit`/`npm audit`).
- We do **NOT** run AST taint analysis (that's `semgrep`/`bandit`).
- We do **NOT** run LLM-driven security review (that's the security-auditor agent or stage 12 CI).
- We do **NOT** evaluate license compliance.

---

## 2. Chosen checks — five axes

After filtering the architect's candidate list for *tractable* AND *non-redundant with CI tools*, **five** checks survive. Each has a one-line justification for *why the graph is the right place* (and not a standard scanner).

### 2.1 S1 — Auth-critical community touch

**Rule.** If any changed symbol is a member of a community whose member set includes at least one symbol whose name matches an auth/crypto pattern, flag `critical`.

**Pattern list** (case-insensitive substring match on symbol `name`):
```
auth, authn, authz, login, logout, session, token, jwt,
password, passwd, secret, credential, apikey, api_key,
permission, role, grant, acl, policy,
crypto, cipher, encrypt, decrypt, hash, sign, verify,
tls, ssl, certificate, keypair, keystore,
sanitize, escape, validate_input
```

**Cypher sketch.**
```cypher
// step 1: find communities containing auth-pattern symbols
MATCH (s)-[:MemberOf_Function_Community|:MemberOf_Method_Community|...]->(c:Community)
WHERE toLower(s.name) CONTAINS 'auth' OR toLower(s.name) CONTAINS 'token' OR ...
RETURN DISTINCT c.id AS auth_community

// step 2: check if any changed symbol is in those communities
MATCH (s)-[:MemberOf_*]->(c:Community)
WHERE s.qualified_name IN $changed AND c.id IN $auth_communities
RETURN s.qualified_name, c.id
```

**Why the graph.** A CI SCA scanner does not know which *community* a changed function lives in. `cargo audit` checks dependencies; it cannot tell you "this refactor sits in the same community as `verify_jwt`." The community is computed (stage 3c), not declared, so grep-for-auth-filename is a strict subset.

**Severity.** `critical` on match. Rationale: auth-community changes need human review, period.

### 2.2 S2 — Unsafe-symbol touch

**Rule.** If any changed symbol has `unsafe` in its node metadata (Rust `unsafe fn`, `unsafe impl`, `unsafe` blocks' enclosing function), flag `critical`.

For non-Rust languages, extend to:
- Python: symbols whose body contains `eval`, `exec`, `__import__`, `pickle.loads`, `subprocess.*shell=True`.
- TS/JS: symbols whose body contains `innerHTML`, `dangerouslySetInnerHTML`, `eval`, `new Function(`.

**Data requirement.** The parser (stage 3a) must record an `is_unsafe` or `risky_api_refs` property. If not currently recorded, degrade gracefully: emit `info` "unsafe detection unavailable: parser does not expose is_unsafe" and skip.

**Cypher sketch.**
```cypher
MATCH (s) WHERE s.qualified_name IN $changed AND s.is_unsafe = true
RETURN s.qualified_name, s.label
```

**Why the graph.** `semgrep` can find `unsafe` blocks in a file, but not "which symbol node does this block belong to?" The graph binds the unsafe marker to the symbol, which is the unit the PRD references.

**Severity.** `critical` for Rust `unsafe`, `warning` for the JS/Python risky-API set (less deterministic).

### 2.3 S3 — Public API surface change

**Rule.** If any changed symbol has `visibility = "pub"` and is at crate root or module-level, flag `warning`. If the change_kind is `remove` or the rename changes the name, escalate to `critical` (API break).

**Cypher sketch.**
```cypher
MATCH (s)-[:Contains]->(n)
WHERE n.qualified_name IN $changed
  AND n.visibility = 'pub'
  AND (s:Module OR s.name = 'crate' OR s:File)
RETURN n.qualified_name, n.visibility, s.label AS parent_label
```

**Why the graph.** Public-API detection in isolation is grep-able, but the *parent context* (is this a crate-public symbol or a module-internal `pub`?) requires traversing Contains edges. A flat scanner can't answer "is this symbol reachable from the crate root?"

**Severity.** `warning` on pub-modify; `critical` on pub-remove or pub-rename (breaks downstream consumers).

### 2.4 S4 — Unresolved-import introduction

**Rule.** If the changed set introduces new import edges whose target appears in the resolver's `unresolved_refs` list (stage 3b output), flag `warning`. If multiple unresolved imports are introduced, escalate to `critical`.

**Input.** Stage 3b persists `unresolved_refs` as a list of `(from_id, kind, target_text, reason)`. We check:
- For each changed symbol s, enumerate new Imports edges.
- Cross-reference against the resolver's unresolved list.

**Cypher sketch.** (after re-running stage 3b on the after-graph)
```cypher
// The unresolved-refs list is written to a JSON sidecar, not the graph proper.
// We load it from <output_dir>/runs/<run_id>/stage-3b.resolution.json
// and intersect with import edges whose from_id belongs to changed symbols.
```

**Why the graph.** A new unresolved import means either a missing crate/package (supply-chain risk: typosquat? abandoned?) or an internal API drift. A standalone resolver doesn't know which symbol *introduced* the unresolved — the graph does.

**Severity.** `warning` for 1 unresolved; `critical` for ≥2 OR for any unresolved that looks like a crate name rather than an in-project module (heuristic: no path separator and not in the project's workspace manifest).

### 2.5 S5 — Test-coverage gap

**Rule.** For each changed symbol, check whether it participates in any process whose entry point has kind `test` or `bench`. If not, flag `warning`.

**Cypher sketch.**
```cypher
MATCH (s)-[:ParticipatesIn_Function_Process|:ParticipatesIn_Method_Process]->(p:Process)
MATCH (entry)-[:EntryPointOf_Function_Process|:EntryPointOf_Method_Process]->(p)
WHERE s.qualified_name IN $changed
  AND p.entry_kind = 'test'
RETURN s.qualified_name,
       collect(DISTINCT p.name) AS test_processes

// Symbols in $changed that produce zero rows are uncovered.
```

**Why the graph.** Code-coverage tools measure execution coverage, not structural coverage. A file can have 90% line coverage yet leave an entire helper untested; the graph's ParticipatesIn from a test entry point is a structural, not dynamic, signal. It answers: **"is there any path from any test to this symbol?"** — which no static scanner does.

**Severity.** `warning` per uncovered symbol; `info` if the symbol is private (arguably internal-only).

---

## 3. What we rejected from the candidate list

| Candidate | Why rejected |
|---|---|
| "Unresolved external crates" (generic supply-chain flag) | Subsumed by S4 with better structure (we flag specific symbols, not just counts). |
| License compliance | Explicitly out of scope per architect's brief. |
| Hard-coded-secret detection (string literals like `AWS_SECRET=`) | Redundant with `trufflehog`, `gitleaks`, etc. Also not represented in the graph. |
| Dependency-version CVE check | Redundant with `cargo audit`, `npm audit`, `pip-audit`. |
| Taint analysis across functions | Requires dataflow, not just call-graph. Redundant with CodeQL/Semgrep. |

---

## 4. Tool contract

### 4.1 Input schema

```json
{
  "name": "check_security_gates",
  "inputSchema": {
    "type": "object",
    "required": ["run_id", "finding_id", "output_dir", "graph_path", "changed_symbols"],
    "properties": {
      "run_id":          { "type": "string" },
      "finding_id":      { "type": "string" },
      "output_dir":      { "type": "string", "pattern": "^/.+" },
      "graph_path":      { "type": "string", "pattern": "^/.+" },
      "changed_symbols": {
        "type": "array",
        "items": {
          "type": "object",
          "required": ["qualified_name", "change_kind"],
          "properties": {
            "qualified_name": { "type": "string" },
            "change_kind":    { "type": "string", "enum": ["add", "modify", "remove", "rename"] },
            "old_qualified_name": { "type": "string" }
          }
        }
      },
      "unresolved_refs_path": {
        "type": "string",
        "description": "Optional. Default: <output_dir>/runs/<run_id>/findings/<finding_id>/stage-3b.resolution.json"
      }
    }
  }
}
```

**Why accept a list rather than re-derive from diff.** Upstream (stage 4 impact bundle or a git-diff → qualified_name mapper) already knows the changed set. Passing it in makes this stage pure: no git subprocess, no diff parsing.

### 4.2 Output artifact — `stage-8.security_gates.json`

```json
{
  "run_id": "...",
  "finding_id": "...",
  "graph_path": "...",
  "checked_at": "2026-04-11T...",
  "changed_symbol_count": 12,
  "flags": [
    {
      "check": "S1",
      "severity": "critical",
      "symbol": "src/auth.rs::verify_jwt",
      "message": "Changed symbol is a member of an auth-critical community",
      "details": { "community_id": "community::leiden::1.0::5", "auth_pattern_match": "verify" }
    },
    ...
  ],
  "summary": {
    "S1_auth_community": { "critical": 1, "warning": 0, "info": 0 },
    "S2_unsafe":         { "critical": 0, "warning": 0, "info": 0 },
    "S3_public_api":     { "critical": 0, "warning": 2, "info": 0 },
    "S4_unresolved":     { "critical": 0, "warning": 1, "info": 0 },
    "S5_coverage_gap":   { "critical": 0, "warning": 3, "info": 1 }
  },
  "gates_passed": false,
  "critical_count": 1,
  "warning_count": 6
}
```

### 4.3 Gate rule

- `gates_passed = (critical_count == 0)`.
- `warning_count` is informational; does not fail the gate.
- Callers decide whether warnings block. Default: warnings do not block, but are surfaced in the PR description.

---

## 5. Invariants

| # | Invariant | Verification |
|---|---|---|
| I1 | Stage 8 is LLM-free. | No network, no subprocess. Enforced. |
| I2 | Stage 8 is read-only w.r.t. the graph. | No mutating Cypher. |
| I3 | Every flag has a severity in `{info, warning, critical}`. | Schema validation. |
| I4 | `gates_passed = true` ⟺ zero critical flags. | Post-run assertion. |
| I5 | Deterministic on same (graph, changed_symbols). | No timestamps in rule logic. |
| I6 | If a check's data source is unavailable, emit `info` and skip — never crash. | S2, S4, S5 all have graceful-degradation paths. |
| I7 | No duplication with standard scanners. | Every check must satisfy: "a non-graph tool cannot answer this question as formulated." |

---

## 6. Implementation steps

| Step | Description | Smoke test | LOC |
|---|---|---|---|
| 1 | **Schema wiring.** Add `check_security_gates_schema()` to tool_schemas.rs. Add `do_check_security_gates` match arm. | `tools/list` returns the tool. | ~30 |
| 2 | **Changed-symbols loader.** Validate input; resolve optional `unresolved_refs_path`. | Empty changed list → empty flags, gates pass. | ~30 |
| 3 | **S1 auth-community check.** Precompute auth-community set; intersect with changed symbols. | Symbol in community with `verify_token` → critical. | ~50 |
| 4 | **S2 unsafe check.** Query `is_unsafe` property; degrade if absent. | Unsafe Rust fn in Δ → critical; missing property → info skip. | ~30 |
| 5 | **S3 public-API check.** Visibility + module-level detection. | Crate-root `pub fn` in remove set → critical. | ~40 |
| 6 | **S4 unresolved-import check.** Load stage-3b JSON; intersect. | Δ introduces new unresolved → warning; ≥2 → critical. | ~40 |
| 7 | **S5 coverage-gap check.** Per-symbol ParticipatesIn query against test entry points. | Symbol with no test process → warning. | ~30 |
| 8 | **Report assembler + artifact writer.** Apply §4.3 gate rule. Write `stage-8.security_gates.json`. | End-to-end run yields valid JSON. | ~40 |
| 9 | **Index update.** Append `stage8_path`, `stage8_checked_at`, `stage8_gates_passed`. | Index contains the three keys. | ~20 |

Total estimate: **~310 LOC** in new `src/security_gates.rs`.

---

## 7. Open questions

1. **Does the parser (stage 3a) currently record `is_unsafe` on function/method nodes?** If not, S2 is partial. **Resolution:** inspect `src/rust_parser.rs` before implementing step 4. File a parser enhancement if missing; ship S2 in info-skip mode until then.
2. **Where do `changed_symbols` come from in practice?** Options: (a) stage 4 impact bundle, (b) a new `git_diff_to_symbols` helper that maps a unified diff back to qualified names via line ranges in the graph. **Resolution:** out of scope for this stage's spec — we accept the list as input. Document the provenance expectation in the tool description.
3. **Auth-pattern list completeness.** The substring list in S1 is inevitably incomplete. **Deferred:** ship with the list in §2.1; add a config file `stage-8.auth_patterns.json` in a follow-up. Not v1.
4. **Test-entry detection granularity.** Stage 3c defines `entry_kind = 'test'`. Does it catch integration tests in `tests/` dir? **Resolution:** already handled per stage-3c.md §3.1 ("Function whose qualified_name is under a `#[cfg(test)]` module"). Integration tests in `tests/*.rs` are separate files; they may need `lib_entry` classification. Revisit if S5 produces false warnings on integration-tested code.

---

## 8. Source citations

| Claim | Source |
|---|---|
| Community schema for S1 | stages/stage-3c.md §4.1 |
| Process / entry_kind for S5 | stages/stage-3c.md §3.1, §4.1 |
| Unresolved-refs schema for S4 | src/resolver.rs `UnresolvedRef` struct + stages/stage-3b.md §9 |
| Rust `unsafe` semantics | Rust Reference §Unsafety |
| Rust `pub` visibility rules | Rust Reference §Visibility and privacy |
| Why community-based auth detection beats filename grep | Leiden / Louvain produce functional groupings not visible in filename structure (Traag 2019) |
| Exclusion of license/dep-version checks | NOTES.md stage 8 row — "Legal/license gates owned elsewhere" |
| Graph-aware scope restriction | NOTES.md stage 8 row — "Graph-aware security gate" |
