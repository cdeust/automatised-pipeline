# Stage 2 — `verify_finding` (clarification loop)

**Status:** spec. Consumes stage 1's artifact (`stages/stage-1.md` §4.4, §9). Extends the on-disk layout under `runs/<run_id>/findings/<finding_id>/`. Inherits every invariant in stage-1.md §5 unless explicitly overridden.

---

## 1. Purpose

Stage 2 takes one finalized stage-1 artifact and produces a **human-acknowledged, clarified** version of the finding ready for stage 3 (codebase analysis). It runs a multi-turn Q&A session between an LLM agent and the human user that terminates only on an explicit user signal. The Rust MCP tools own the session state on disk; the agent owns the intelligence; the user owns the stop condition.

---

## 2. Shannon framing

**Source.** The stage-1 refined artifact (`stage-1.refined.json`) plus a live human who can answer questions. The input channel is dual: machine-readable JSON and asynchronous human turns. The source distribution is unknown — it is whatever the human chooses to answer.

**Channel.** Three MCP tool calls (see §7) plus one append-only transcript file. The Rust tools write state; the agent reads state, asks the human, and writes the next turn. The human's answer flows back through the agent, which calls `append_clarification` to commit it. Stage 3 reads only `stage-2.verified.json` — never the transcript.

**Code.** Every turn in the session is numbered, typed, and persisted atomically so that a crash at any point leaves a valid prefix of the conversation on disk. The "code" is the transcript schema (§5) plus the state machine (§3) that prevents out-of-order appends.

**Information upper bound.** Stage 2 can only hand to stage 3 what it captures as machine-readable data in `stage-2.verified.json`. Conversation nuance that is not distilled into structured fields (§5.2) is lost to stage 3 even though it remains in the transcript.

---

## 3. State machine

A session has exactly one state at any time. Transitions are triggered by tool calls or, for `expired`, by wall-clock time. `finalized` and `aborted` are terminal.

```
                    start_verification
        (no session) ───────────────────▶ open
                                            │
                                            │  append_clarification(turn: "agent_question")
                                            ▼
                                     waiting_for_user
                                            │
                                            │  append_clarification(turn: "user_answer")
                                            ▼
                                     waiting_for_agent
                                            │
                           ┌────────────────┴────────────────┐
                           │                                 │
              append_clarification                  finalize_verification
              (turn: "agent_question")                       │
                           │                                 ▼
                           ▼                             finalized (terminal)
                    waiting_for_user
                           │
                           │  abort_verification (optional)
                           ▼
                      aborted (terminal)
```

| State               | Meaning                                                | Allowed next calls                                   |
|---------------------|--------------------------------------------------------|------------------------------------------------------|
| `open`              | Session created, no turns yet.                         | `append_clarification(agent_question)`, `finalize_verification`, `abort_verification` |
| `waiting_for_user`  | Agent just asked a question; human has not answered.   | `append_clarification(user_answer)`, `abort_verification` |
| `waiting_for_agent` | Human just answered; agent has not yet decided next.   | `append_clarification(agent_question)`, `finalize_verification`, `abort_verification` |
| `finalized`         | Terminal. User signalled ready.                        | none (further writes rejected)                        |
| `aborted`           | Terminal. Session killed before user ready.            | none                                                  |

**Invariant: alternation.** Two consecutive `agent_question` turns are rejected. Two consecutive `user_answer` turns are rejected. The `from_state → via_turn_type → to_state` table is the only legal path.

**Note on `open → finalized`:** legal. A finding may be self-evident; the agent may finalize without asking anything. The verified artifact will record `turns: []` and `verified_kind` containing only `schema` (see §4).

---

## 4. Q1 — What "verified" means here

Stage 2 uses a composite definition with three independent boolean flags, each backed by an operational procedure. All three must be `true` in `stage-2.verified.json` before stage 3 may consume it; a flag set to `false` means stage 2 was finalized early and stage 3 must decide whether to accept the downgrade.

| Flag                  | Operational procedure                                                                                                                                                                                                                                         | Who decides  |
|-----------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|--------------|
| `schema_ok`           | The stage-1 refined artifact exists on disk, parses as `RefinedArtifact` (stage-1.md §4.2), and the preconditions of stage-1.md §5.1 all hold. Checked by the Rust tool at `start_verification` time.                                                         | Rust tool    |
| `completeness_ok`     | A checklist of stage-2-required fields is satisfied. At v1 the checklist is: `extracted.relevance_category` non-empty AND `refined_prompt.text` non-empty AND (either the finding has `description` OR the session has at least one `user_answer` turn). Checked by the Rust tool at `finalize_verification` time against the transcript. | Rust tool    |
| `user_acknowledged`   | The user has explicitly signalled readiness via the `finalize_verification` tool call (§6). No LLM inference, no magic phrase in answers. The tool call itself is the signal.                                                                                 | Human (via agent) |

**Stage 3's contract with stage 2.** Given a finalized `stage-2.verified.json`:
1. The finding schema and stage-1 invariants hold (`schema_ok`).
2. The finding has enough data to anchor codebase analysis (`completeness_ok`).
3. A human has seen the finding and declared it ready (`user_acknowledged`).

Anything beyond this (semantic plausibility, cross-reference to prior findings, external fact-checking) is **not** what "verified" means in stage 2 and is deferred. No LLM-based plausibility check is defined here, because the old pipeline's only verification prompt (`ai-architect-feedback-loop/prompts/semantic_verification.md:1-60`) runs post-implementation on code diffs and has no overlap with pre-analysis finding clarification. *Rejected* as a model for stage 2 — different problem.

---

## 5. Q2 — Session and finalized artifact schemas

Two files live under `runs/<run_id>/findings/<finding_id>/`, alongside the existing stage-1 files. File layout extension of stage-1.md §4.4:

```
runs/<run_id>/findings/<finding_id>/
  stage-1.source.json         (stage 1, unchanged)
  stage-1.extracted.json      (stage 1, unchanged)
  stage-1.refined.json        (stage 1, unchanged)
  stage-2.session.jsonl       (stage 2, append-only; one turn per line)
  stage-2.session.head.json   (stage 2, overwritten atomically each turn)
  stage-2.verified.json       (stage 2, written once at finalize time)
```

**Why two session files.** `stage-2.session.jsonl` is the transcript (append-only, crash-safe, monotonic `seq`). `stage-2.session.head.json` is a single JSON object holding the *current* state, turn count, and pointer into the JSONL — cheap for the Rust tool to read and rewrite atomically without scanning the whole transcript. JSONL for history + head file for current state is a standard split; source: the same pattern used by Git's `packed-refs` + `HEAD`, and by SQLite's WAL + main DB. Not load-bearing on a single source — the split is justified by the operational need (cheap state check, append-only history).

### 5.1 `stage-2.session.head.json`

```json
{
  "type": "object",
  "required": ["run_id", "finding_id", "state", "turn_count", "started_at", "updated_at", "schema_ok"],
  "additionalProperties": false,
  "properties": {
    "run_id":       { "type": "string" },
    "finding_id":   { "type": "string" },
    "state":        { "enum": ["open", "waiting_for_user", "waiting_for_agent", "finalized", "aborted"] },
    "turn_count":   { "type": "integer", "minimum": 0 },
    "started_at":   { "type": "string", "format": "date-time" },
    "updated_at":   { "type": "string", "format": "date-time" },
    "schema_ok":    { "type": "boolean" },
    "transcript_path": { "type": "string", "description": "Relative path to stage-2.session.jsonl" }
  }
}
```

`schema_ok` is set at `start_verification` time based on the stage-1 artifact check (§4) and never mutates.

### 5.2 `stage-2.session.jsonl` — one JSON object per line

```json
{
  "type": "object",
  "required": ["seq", "kind", "timestamp", "content"],
  "additionalProperties": false,
  "properties": {
    "seq":       { "type": "integer", "minimum": 0, "description": "Monotonic, dense, zero-based." },
    "kind":      { "enum": ["agent_question", "user_answer"] },
    "timestamp": { "type": "string", "format": "date-time" },
    "content":   { "type": "string", "minLength": 1 },
    "meta":      { "type": "object", "description": "Free-form agent metadata. Ignored by the Rust tool." }
  }
}
```

**Invariants.** (a) `seq` values are the line number (zero-based) on write. (b) A new turn is appended iff its `kind` alternates from the previous turn (§3 alternation invariant). (c) Once `finalized` or `aborted`, no more lines may be appended.

### 5.3 `stage-2.verified.json` — the finalized artifact

```json
{
  "type": "object",
  "required": ["run_id", "finding_id", "verified", "verified_kind", "finalized_at", "stage1_refined_path", "transcript_path", "transcript_digest", "turn_count"],
  "additionalProperties": false,
  "properties": {
    "run_id":        { "type": "string" },
    "finding_id":    { "type": "string" },
    "verified":      { "type": "boolean", "description": "AND of verified_kind flags." },
    "verified_kind": {
      "type": "object",
      "required": ["schema_ok", "completeness_ok", "user_acknowledged"],
      "additionalProperties": false,
      "properties": {
        "schema_ok":         { "type": "boolean" },
        "completeness_ok":   { "type": "boolean" },
        "user_acknowledged": { "type": "boolean" }
      }
    },
    "finalized_at":        { "type": "string", "format": "date-time" },
    "stage1_refined_path": { "type": "string", "description": "Relative path to stage-1.refined.json inside the finding dir." },
    "transcript_path":     { "type": "string", "description": "Relative path to stage-2.session.jsonl." },
    "transcript_digest":   { "type": "string", "description": "sha256 of the transcript file at finalize time. Stage 3 can verify the transcript has not been tampered with." },
    "turn_count":          { "type": "integer", "minimum": 0 },
    "verifier_version":    { "type": "string", "description": "Compile-time constant in src/main.rs, like EXTRACTOR_VERSION in stage 1." },
    "completeness_checklist": {
      "type": "object",
      "description": "Which items of the §4 completeness checklist passed. Preserved for audit.",
      "additionalProperties": { "type": "boolean" }
    }
  }
}
```

The verified artifact deliberately does NOT embed the finding or the transcript. Both live next to it on disk (paths are recorded). Stage 3 opens them if it needs them. This keeps `stage-2.verified.json` small and its semantics clean: **it is a receipt, not a dossier.**

### 5.4 Index update

`runs/<run_id>/index.json` (stage-1.md §5.2) gains per-finding fields when stage 2 finalizes, in the existing `findings[<finding_id>]` entry:

```json
{
  "verified_at":     "<finalized_at>",
  "verified":        true,
  "stage2_path":     "findings/<finding_id>/stage-2.verified.json"
}
```

Stage 3 discovers its inputs by scanning `index.json` for `verified: true` entries. Atomic update rules: same as stage-1.md §5.2.3 (tempfile + rename).

---

## 6. Q3 — The termination signal

**The `finalize_verification` tool call is the signal.** The user, through the agent, says "I am ready." The agent responds by invoking `finalize_verification(run_id, finding_id, output_dir)`. The tool call itself IS the "ready" event; no heuristic, no phrase-matching, no LLM classification.

**Justification.**
1. **Symmetry with stage 1.** Stage 1 terminates the extract-then-refine flow by an explicit `refine_finding` call (stage-1.md §9.1). The same pattern here: the termination is a dedicated tool call, not an implicit signal inside data.
2. **Non-gameability.** A magic phrase in a `user_answer` turn ("ready", "done", "yes") is ambiguous — the user might say "I'm ready to continue asking questions." An LLM classifier on the answer adds a new failure mode and violates the Rust=data / agent=intelligence split. An explicit tool call is unambiguous and requires no intelligence inside the Rust tool.
3. **Auditability.** The transition from `waiting_for_agent` → `finalized` is a single event in the head file with a timestamp. One event, one cause. Trivial to debug.

**The agent's responsibility.** The agent MUST NOT call `finalize_verification` based on its own judgment that the finding "looks ready." It calls it **only** after the user has explicitly said so in a turn the agent committed via `append_clarification(user_answer)`. This is a rule for the agent prompt, not the Rust tool. The Rust tool cannot enforce it — the agent layer owns intelligence.

*Rejected alternatives.* (a) Magic phrase in an answer — brittle, LLM-classification-laden, violates the split. (b) Always-ask-the-user turn type — verbose, adds a new `kind` to the transcript, doubles turn count. (c) Timeout-based auto-finalize — dangerous; silently produces `user_acknowledged: true` without the user. All rejected.

---

## 7. Proposed tool set

**Tool count: three.** Justified below. Each tool is a pure persistence operation over `runs/<run_id>/findings/<finding_id>/`. None calls an LLM. All inherit stage-1.md §5.1.4 (safe-ID rule) and §5.2.3 (atomic writes).

### 7.1 `start_verification`

**Purpose.** Create the session. Verify the stage-1 artifact exists and parses; set `schema_ok`.

**Input schema.**
```json
{
  "type": "object",
  "required": ["run_id", "finding_id", "output_dir"],
  "additionalProperties": false,
  "properties": {
    "run_id":     { "type": "string" },
    "finding_id": { "type": "string" },
    "output_dir": { "type": "string", "pattern": "^/.+" }
  }
}
```

**Preconditions.** (i) `runs/<run_id>/findings/<finding_id>/stage-1.refined.json` exists and parses as `RefinedArtifact`. (ii) No existing `stage-2.session.head.json`, OR existing head is in state `aborted` (restart allowed). A head in `finalized` state is NOT restartable — fail with `already_finalized`.

**Postconditions.** Writes `stage-2.session.head.json` with `state: "open"`, `turn_count: 0`, `schema_ok: true`, creates empty `stage-2.session.jsonl`. Atomic.

**Output.** `{ stage: 2, status: "ok", state: "open", run_id, finding_id, started_at, head_path }`.

### 7.2 `append_clarification`

**Purpose.** Append one turn (agent question or user answer) to the transcript and advance the state machine.

**Input schema.**
```json
{
  "type": "object",
  "required": ["run_id", "finding_id", "output_dir", "kind", "content"],
  "additionalProperties": false,
  "properties": {
    "run_id":     { "type": "string" },
    "finding_id": { "type": "string" },
    "output_dir": { "type": "string", "pattern": "^/.+" },
    "kind":       { "enum": ["agent_question", "user_answer"] },
    "content":    { "type": "string", "minLength": 1 },
    "meta":       { "type": "object" }
  }
}
```

**Preconditions.** (i) Head exists and is in a non-terminal state. (ii) The transition in §3 is legal for the current state and requested `kind`. (iii) Alternation invariant holds.

**Postconditions.** Appends one line to `stage-2.session.jsonl` (append-mode open + write + flush + fsync), then rewrites `stage-2.session.head.json` atomically with the new state, `turn_count + 1`, and bumped `updated_at`. Order matters: transcript line MUST land before head is updated, so a crash between the two leaves a head that's one turn behind the transcript, which is recoverable. A crash before the transcript write leaves both files consistent at the old state.

**Output.** `{ stage: 2, status: "ok", state: "<new state>", seq: <n>, turn_count: <n+1> }`.

### 7.3 `finalize_verification`

**Purpose.** Consume the user-ready signal. Compute `completeness_ok`, compute `transcript_digest`, write `stage-2.verified.json`, update `index.json`.

**Input schema.**
```json
{
  "type": "object",
  "required": ["run_id", "finding_id", "output_dir"],
  "additionalProperties": false,
  "properties": {
    "run_id":     { "type": "string" },
    "finding_id": { "type": "string" },
    "output_dir": { "type": "string", "pattern": "^/.+" }
  }
}
```

**Preconditions.** (i) Head exists and state is `open` or `waiting_for_agent`. Finalizing from `waiting_for_user` is rejected — there is an unanswered question on the floor; the user cannot be ready. (ii) `schema_ok` on the head is `true` (else the stage-1 artifact is broken and stage 2 cannot finalize).

**Postconditions.** Runs the §4 completeness checklist against the stage-1 artifact and the transcript. Computes sha256 of the transcript file. Writes `stage-2.verified.json` atomically. Rewrites head with `state: "finalized"`. Updates `runs/<run_id>/index.json` atomically with the stage-2 fields (§5.4).

**Output.** `{ stage: 2, status: "ok", state: "finalized", verified: <bool>, verified_kind: {...}, verified_path, turn_count, transcript_digest }`.

### 7.4 Why three tools and not two or one

- **One-tool shape (`verify_finding(action, ...)`).** Rejected. A single dispatcher with an `action` field is the MCP anti-pattern: it hides the state machine behind a string and the input schema becomes a union of four disjoint shapes. The agent layer has to remember which fields are required for which action. The Rust tool has to re-dispatch internally. All the coupling returns.
- **Two-tool shape (step + finalize).** Rejected. Collapsing `start_verification` into `append_clarification` means the first call has a different contract (no prior state required, must create the head) than all subsequent calls (prior state required, must advance). That's a hidden mode. Having `start_verification` as its own tool makes the "session begins" moment observable and gives the agent a single place to handle the "stage 1 artifact is missing" failure.
- **Three-tool shape.** One tool per state-machine event: create, advance, terminate. Matches the stage-1 pattern (`extract_finding` + `refine_finding` = two tools, one per deterministic step). Each tool has exactly one job. Cleanest.

An `abort_verification` tool is deliberately not in v1 (see §10 open questions).

---

## 8. Invariants (stage-2 specific only)

Stage 2 inherits all of stage-1.md §5.1 (finding-level) and §5.2 (run-level) unchanged. Additional stage-2 invariants:

1. **Alternation.** §3. Two consecutive turns of the same `kind` are rejected at `append_clarification` time.
2. **Append-only transcript.** `stage-2.session.jsonl` is never rewritten in place. New turns are appended. A finalized or aborted session rejects further appends — the transcript is frozen.
3. **Write order.** `append_clarification` writes the JSONL line before updating the head. A crash between the two leaves the head one turn behind the transcript; `start_verification` on an existing head must reconcile (see §10 OQ3).
4. **Terminal means terminal.** Once head state is `finalized` or `aborted`, no tool writes. `start_verification` on a `finalized` head is an error.
5. **`verified_kind` is AND.** `verified = schema_ok AND completeness_ok AND user_acknowledged`. The Rust tool never sets `verified: true` when any sub-flag is false.
6. **Digest integrity.** `transcript_digest` is computed over the exact bytes of the transcript file at finalize time. If any external process modified the transcript between finalize and stage 3's read, stage 3 will detect it by recomputing the digest.
7. **No agent identity in the transcript.** The `meta` field is free-form but the Rust tool does not interpret it. "Which LLM asked the question" belongs in the agent's own logs, not in the stage-2 artifact.

---

## 9. What stage 2 must never lose

The information upper bound, same discipline as stage-1.md §6:

1. **The stage-1 path reference** (`stage1_refined_path` in §5.3). Without it, stage 3 cannot locate the underlying finding.
2. **The full transcript** (`stage-2.session.jsonl`). Every question, every answer, in order. The verified artifact summarizes, but the transcript is the ground truth. A future stage may want to mine it; stage 2 must not drop it.
3. **The three verified flags.** `schema_ok`, `completeness_ok`, `user_acknowledged` — stage 3 needs each independently because they answer different questions.
4. **`finalized_at` and `turn_count`.** Operational metrics. Without them, there is no way to audit "how long did clarification take" or "how many rounds was typical."
5. **`transcript_digest`.** Tamper detection.
6. **Completeness checklist breakdown** (`completeness_checklist`). Knowing `completeness_ok: false` is not useful; knowing **which item failed** is.

Stage 2 is **allowed** to lose:
- Agent internal reasoning traces (chain-of-thought, scratchpad). These belong in agent logs, not in the session artifact. If the agent wants to persist them it uses its own store.
- Inter-turn timing beyond wall-clock timestamps. "User took 4 minutes to answer" is not load-bearing for stage 3.
- The identity of the LLM model that produced each question. Stage 2 is model-agnostic.

---

## 10. Open questions

Only real blockers.

1. **Do we need `abort_verification` in v1?** A user might want to kill a session without finalizing (wrong finding, start over). v1 can live without it — the user deletes the head file by hand. v1 MUST reject a fresh `start_verification` on a non-aborted, non-finalized head (prevents silent clobbering), but that means without the abort tool, the only recovery is manual filesystem intervention. **Recommendation: defer `abort_verification` to v2 unless the user says otherwise.**
2. **Completeness checklist at v1.** §4 proposes a minimal checklist (category non-empty, refined prompt non-empty, description OR at least one user answer). Is this the right checklist? The stage-1 artifact already guarantees the first two (stage-1.md §5.1.5). So the only stage-2-added check is "description OR at least one user answer." **Is that enough? Needs user decision** — if the answer is "no, stage 2 must require at least one agent_question AND at least one user_answer regardless of description," the checklist tightens to a minimum round-trip.
3. **Crash recovery policy.** If `start_verification` finds a head whose `turn_count` disagrees with the JSONL line count (partial-write crash), does it (a) truncate the JSONL to match the head, (b) advance the head to match the JSONL, or (c) error out and require manual intervention? I propose **(b)** — the transcript is the ground truth because it's written first — but this is a Rust implementation decision that affects invariant §8.3. Needs confirmation.

Everything else (timeout semantics, multi-user sessions, session resume across `output_dir` moves) is deferred to v2 or later.

---

## 11. Source citations

| Decision | Source | Status |
|---|---|---|
| Artifact directory layout `runs/<run_id>/findings/<finding_id>/...` | stage-1.md §4.4 | **Inherited.** Stage 2 adds files; it does not move the layout. |
| Atomic write (tempfile + rename) | stage-1.md §5.2.3; POSIX `rename(2)` | **Inherited.** |
| Safe-ID rule on `run_id` and `finding_id` | stage-1.md §5.1.4, §9.3 Q4 | **Inherited.** `start_verification` re-validates. |
| Index update pattern | stage-1.md §5.2 | **Inherited, extended** per §5.4. |
| Version string as compile-time constant (`VERIFIER_VERSION`) | stage-1.md §9.3 Q6 (`EXTRACTOR_VERSION`, `ORCHESTRATOR_CONTRACT_VERSION`) | **Inherited pattern.** New constant `VERIFIER_VERSION: &str = "1.0.0"` in `src/main.rs`. |
| Tool-split pattern (one tool per deterministic step) | stage-1.md §9.1 | **Inherited, extended** to three tools for the three state-machine events. |
| Rust=data / agent=intelligence split | stage-1.md §9.1; user directive | **Inherited.** No LLM in the Rust tool. |
| JSONL append-only + head state file | Git `packed-refs` + `HEAD`; SQLite WAL + main DB | **Borrowed as pattern** (not a single-source citation). Motivation: cheap head read without scanning the transcript. |
| sha256 for `transcript_digest` | Standard practice; any modern Rust crate (e.g., `sha2`) | **Borrowed.** Adds one dep (`sha2`), justified by tamper-detection requirement. If the user wants zero new deps, the fallback is to omit the digest and accept weaker stage-3 contract — **needs confirmation**. |
| Old pipeline `semantic_verification.md` prompt (stage 11) | `ai-architect-feedback-loop/prompts/semantic_verification.md:1-60` | **Rejected as model for stage 2.** That prompt verifies post-implementation code diffs against a PRD. Stage 2 here clarifies pre-analysis findings. Different problem, different inputs, different outputs. Mentioned only to record that the rejection is deliberate. |
| Old pipeline `prd_review.md` (stage 6) | `ai-architect-feedback-loop/prompts/prd_review.md:1-40` | **Rejected as model for stage 2.** Reviews a generated PRD, not a finding. |
| `pipeline_policy.yaml` stage_models | `ai-architect/02_Areas/Pipeline/pipeline_policy.yaml:39-50` | **Not applicable.** Stage 2 here is not even the same stage as old stage 2 (old stage 2 = impact scoring). No model is pinned in this spec; the agent layer picks the model. |
| `thresholds.json` "verification" category | `ai-architect/02_Areas/Pipeline/thresholds.json:17` (the string "verification" in the list of relevance categories) | **Not applicable.** That's a routing tag on findings, not a pipeline stage of the same name. |
| Completeness checklist thresholds | None — chosen by judgment in §4 | **Flagged as open question §10.2.** No invented constants; the checklist is plain boolean logic, not weighted scoring. |

---

*The method (Rust implementation) is deferred until the shape above is accepted. The spec is shorter than stage 1's because stage 1 established the ground rules; stage 2 extends them.*

---

## 12. Locked decisions (supersede §5, §7, §10 on the items below)

User-approved, 2026-04-11. These overrule anything in §5.1, §5.2, §7, and §10 that conflicts. Rationale for each decision cited with the user's own words.

### 12.1 `abort_verification` — **included in v1**

> "We need to add now, going with a manual step to clean will not get us any user, we're in the partner program of Anthropic we cannot make half assled implementation."

Stage 2 ships **four** tools, not three: `start_verification`, `append_clarification`, `finalize_verification`, `abort_verification`. Manual `rm` recovery is rejected.

**`abort_verification` contract:**

Input schema:
```json
{
  "type": "object",
  "required": ["run_id", "finding_id", "output_dir"],
  "additionalProperties": false,
  "properties": {
    "run_id":     { "type": "string" },
    "finding_id": { "type": "string" },
    "output_dir": { "type": "string", "pattern": "^/.+" },
    "reason":     { "type": "string", "description": "Optional human-readable abort reason, recorded in the session file for audit." }
  }
}
```

**Preconditions.** (i) Session file exists and state is not already `finalized` or `aborted`. (ii) `run_id`/`finding_id` safe-ID per stage-1.md §5.1.4.

**Postconditions.** Atomically rewrites the session file with `state: "aborted"`, `aborted_at: <iso-8601 UTC>`, `abort_reason: <reason or null>`. No `stage-2.verified.json` is written. `runs/<run_id>/index.json` is NOT updated (an aborted session is invisible to stage 3 via the index).

**Output:** `{ stage: 2, status: "ok", state: "aborted", run_id, finding_id, turn_count, aborted_at }`.

After abort, `start_verification` may be called again to begin a fresh session — the aborted session file is overwritten. This is the recovery path.

### 12.2 Completeness checklist — **unconditional one-round-minimum**

> "Even with a description the finding will never be ready enough to be doable without clarifications, none LLM will way I've got all needed from a single description"

The §4 `completeness_ok` checklist is rewritten to:

> `completeness_ok = (transcript contains ≥1 turn with kind="agent_question") AND (transcript contains ≥1 turn with kind="user_answer")`

The clause `description OR at least one user_answer turn` is **dropped**. A finding's description is not a substitute for a clarification round-trip. Shannon's minimal checklist reflected a zero-round escape hatch that the user explicitly rejected: no LLM will ever declare a finding complete from description alone.

**Implication for `finalize_verification` preconditions.** In addition to §7.3, finalize rejects with `reason: "no_clarification_round"` if the session transcript has zero agent_questions or zero user_answers. `verified_kind.completeness_ok: false` is no longer a possible state at finalize time — finalize either succeeds with `completeness_ok: true` or rejects outright. This removes the "downgrade" path Shannon sketched in §4.

**Implication for §3 state machine.** `finalize_verification` from state `open` is now **illegal** (was "legal with empty transcript"). The minimum legal finalize path is `open → waiting_for_user → waiting_for_agent → finalized`, i.e., at least one full question-answer round trip must have occurred before finalize is accepted.

### 12.3 Session storage — **single-file atomic rewrite (supersedes §5.1 and §5.2)**

> "Advancing head to match jsonl, is running away from fixing the issue and making the process robust enough not to be able to crash"

Root-cause fix, per global CLAUDE.md: "If a fix requires violating a layer boundary, the design is wrong — fix the design." The two-file split in §5 creates a crash window between the JSONL append and the head rewrite. No reconcile policy can close that window without being a band-aid.

**The fix: collapse both files into one.** `stage-2.session.jsonl` and `stage-2.session.head.json` are **replaced** by a single file:

```
runs/<run_id>/findings/<finding_id>/
  stage-2.session.json         (single session file — head + transcript combined)
  stage-2.verified.json        (unchanged — written once at finalize)
```

**`stage-2.session.json` schema:**

```json
{
  "type": "object",
  "required": [
    "run_id", "finding_id", "state", "turn_count",
    "started_at", "updated_at", "schema_ok", "transcript"
  ],
  "additionalProperties": false,
  "properties": {
    "run_id":       { "type": "string" },
    "finding_id":   { "type": "string" },
    "state":        { "enum": ["open", "waiting_for_user", "waiting_for_agent", "finalized", "aborted"] },
    "turn_count":   { "type": "integer", "minimum": 0 },
    "started_at":   { "type": "string", "format": "date-time" },
    "updated_at":   { "type": "string", "format": "date-time" },
    "schema_ok":    { "type": "boolean" },
    "verifier_version": { "type": "string" },
    "transcript":   {
      "type": "array",
      "items": {
        "type": "object",
        "required": ["seq", "kind", "timestamp", "content"],
        "additionalProperties": false,
        "properties": {
          "seq":       { "type": "integer", "minimum": 0 },
          "kind":      { "enum": ["agent_question", "user_answer"] },
          "timestamp": { "type": "string", "format": "date-time" },
          "content":   { "type": "string", "minLength": 1 },
          "meta":      { "type": "object" }
        }
      }
    },
    "aborted_at":   { "type": ["string", "null"], "format": "date-time" },
    "abort_reason": { "type": ["string", "null"] }
  }
}
```

**Atomicity by construction.** Every tool that mutates the session follows exactly this algorithm:

1. Read `stage-2.session.json` (if exists).
2. Validate preconditions (state, alternation, safe-ID, terminal-check).
3. Construct the new session object in memory (never mutate the old one in place).
4. Serialize to bytes.
5. Write to `.stage-2.session.json.tmp.<pid>.<secs>.<nanos>.<rand>` in the **same parent directory**.
6. `fsync` the tempfile.
7. `rename(2)` the tempfile over `stage-2.session.json` — atomic per POSIX.

After step 7, readers see either the old session or the new session — never a partial. A crash between any two steps leaves the old file intact. No reconcile code exists, because no reconcile is possible or needed.

**Transcript semantics are still append-only at the LOGICAL level.** The `transcript` array only ever grows — no turn is ever removed or modified after it's committed to a written session. This is enforced in code: the tools refuse to rewrite or delete any existing element of `transcript`. An abort does not truncate the transcript; it only flips `state` and sets `aborted_at`.

**Cost.** For a bounded session (typical: ≤ 100 turns, ≤ 1 KB/turn), the full file is ≤ 100 KB. Rewriting 100 KB atomically per turn is microseconds on any modern SSD. For a pathologically long session (1000 turns at 10 KB each = 10 MB), it's still milliseconds. This is not a bottleneck.

**`transcript_digest` change.** The digest is now computed over the **canonicalized `transcript` array bytes** inside `stage-2.session.json` at finalize time, not over a separate JSONL file. Canonicalization = `serde_json::to_vec(&session.transcript)` using the same deterministic key ordering the rest of the codebase uses. Stage 3 can recompute the digest by reading `stage-2.session.json`, extracting `transcript`, serializing deterministically, and hashing. The verified artifact records both the digest and `transcript_bytes_at_finalize` (the length) so stage 3 has two tamper-detection channels.

**Section §8.3 (write order) is obsolete.** There is now only one write per turn. Delete.

**Section §10.3 (crash recovery policy) is obsolete.** There is no disagreement possible between head and transcript because there is no head file. Delete.

### 12.4 `sha2` crate — **added**

> "Adding sha2 crate is ok"

`Cargo.toml` gains a single dependency: `sha2 = "0.10"` (RustCrypto org, audited, primitive-only, no transitive framework creep). Used exclusively for `transcript_digest` per §12.3. Source: https://github.com/RustCrypto/hashes — project has been audited multiple times since 2018. This is the kind of dep the zetetic standard explicitly allows (single-purpose, well-known, sourced).

**Dep count: serde + serde_json + sha2 = 3 deps.** Still no framework. Still no async runtime. Still no HTTP/RPC/LLM client.

### 12.5 What the Rust implementer must do (for stage 2)

1. Add `sha2 = "0.10"` to `[dependencies]` in `Cargo.toml`.
2. Add four new tools to `tools_list()` and `handle_tool_call()`: `start_verification`, `append_clarification`, `finalize_verification`, `abort_verification`.
3. Define Rust structs for: `SessionFile`, `SessionTurn`, `SessionState` (enum), `VerifiedArtifact`, `VerifiedKind`, input-args structs per tool. All `#[derive(Serialize, Deserialize)]` with `#[serde(deny_unknown_fields)]` on input structs.
4. Implement the single atomic-rewrite helper (`read_session`, `write_session_atomic`) — reuse `atomic_write` from stage 1 where possible.
5. Implement the state machine check as a single function: `fn can_transition(from: SessionState, via_kind: TurnKind) -> Result<SessionState, StageErr>`.
6. `VERIFIER_VERSION: &str = "1.0.0"` compile-time constant at the top of the file, with a source comment citing §12.1 (first tool-set release including abort).
7. Index update: on finalize, write `verified_at`, `verified: true`, `stage2_path` to `runs/<run_id>/index.json` atomically — reuse stage 1's `upsert_index_entry`. Note: extend `IndexEntry` struct with optional stage-2 fields.
8. `cargo check` + `cargo build --release` must pass. A smoke test exercises: start → append question → append answer → finalize happy path; start → abort → start new; finalize-before-round-trip rejection; double-question alternation rejection; terminal-state rewrite rejection.
9. No function body over 40 LOC. The stage-1 function-size discipline (post S3 cleanup) is binding for stage 2 from day one.
10. Every new hardcoded constant (digest algorithm name, alphabet, default reason strings) must have a `// source:` comment or be a top-of-file named constant.
