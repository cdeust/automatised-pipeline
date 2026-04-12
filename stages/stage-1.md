# Stage 1 — `extract_finding`

**Status:** spec (pre-implementation). No Rust yet. Zetetic: every constant and shape below traces to a file and line, or is explicitly flagged as deferred.

This document defines the *quantities* stage 1 must produce before any code is written. The growth rule from `NOTES.md` applies: stage 1 lands as one arm in `handle_tool_call()` in `src/main.rs`, and stays inline until it can't. This spec describes schemas, invariants, and the on-disk artifact layout — not an implementation.

---

## 1. Purpose

Stage 1 takes **one incoming finding** and produces **one refined-prompt artifact on disk**, so that stage 2 can load it without re-parsing the original source. The stage has two sub-steps fused into one MCP call: (a) deterministic extraction of structured information from the finding, (b) orchestrator-agent refinement that adds downstream-usable context to the extraction. The orchestrator refinement is the only non-deterministic layer; the extraction is a pure function of the input.

Stage 1 does **not** filter, prioritize, or score findings. That is a separate concern and is deferred to a later stage.

---

## 2. Shannon-shape framing (the measure before the method)

Before defining schemas, name the layers. Keeping them separate is the whole point.

- **Source** — a single raw finding, in one of the accepted input forms (§3). The source has a known finite set of shapes; the tool must accept all of them.
- **Channel** — the MCP tool boundary (`tools/call` with `name: "extract_finding"`) plus the filesystem artifact written under `output_dir`. The channel has two halves: the in-process MCP response, and the on-disk artifact. Stage 2 reads the on-disk artifact, **never** the MCP response. The MCP response is for the calling agent; the disk artifact is for the next stage.
- **Code** — the encoding that makes stage 2's decoding trivial. The refined prompt artifact is the code: it must contain everything stage 2 needs, in a format stage 2 can parse without calling any external service, without re-reading the source, and without depending on state outside `output_dir`.

**Information upper bound (the limit that blocks the method):** stage 1 can only pass to stage 2 what it captures in the artifact. Anything that the source contains but the artifact does not contain is **lost forever for this run**. §6 lists what stage 1 must never drop.

---

## 3. Inputs

### 3.1 MCP tool arguments

The tool's `inputSchema` currently declared in `src/main.rs:77-90` already takes `finding` and `output_dir`. This spec tightens that schema:

```json
{
  "type": "object",
  "required": ["finding", "output_dir"],
  "additionalProperties": false,
  "properties": {
    "finding": {
      "description": "One of: (a) an inline finding object matching §3.2, (b) an absolute path to a .json file containing a finding object or a {findings: [...]} wrapper from which the first element is taken, (c) an absolute path to a Technical Veil markdown file (one actor file or one pre-extracted single-finding file).",
      "oneOf": [
        { "type": "object" },
        { "type": "string", "pattern": "^/.+\\.(json|md)$" }
      ]
    },
    "output_dir": {
      "type": "string",
      "pattern": "^/.+",
      "description": "Absolute directory where the artifact will be staged. Created if it does not exist. Must be writable."
    },
    "run_id": {
      "type": "string",
      "description": "Optional opaque run identifier. If omitted, stage 1 generates one (§5.3). Used by stage 2 to group findings from the same pipeline run."
    }
  }
}
```

*Cited/borrowed:* the `finding` + `output_dir` pair carries forward from `src/main.rs:77-90`. The `run_id` idea is borrowed from the feedback-loop run directory convention (`runs/<RUN_TS>/`) at `ai-architect-feedback-loop/skills/run-pipeline/SKILL.md:44` (`RUN_TS=$(date +%Y%m%d-%H%M%S)`).

*Rejected:* the feedback-loop shell driver accepts `--builder-dir`, `--tv-dir`, `--tv-date`, `--tv-input` as four separate priority-ordered flags (`ai-architect-feedback-loop/scripts/stage1-parse-findings.sh:41-79`). Rejected because the MCP tool processes **one** finding per call; the multi-source resolution happens in the caller (the orchestrator agent), not inside the tool. Separation of concerns: discovery ≠ extraction.

### 3.2 Finding schema (canonical)

The canonical in-memory shape. When a finding is passed inline as a JSON object, it must match this schema. When a finding is loaded from a file, it must either already match or be reducible to it (see §3.3).

```json
{
  "type": "object",
  "required": ["id", "title", "relevance_category"],
  "additionalProperties": true,
  "properties": {
    "id":                  { "type": "string", "minLength": 1 },
    "title":               { "type": "string", "minLength": 1 },
    "description":         { "type": "string" },
    "source_url":          { "type": "string" },
    "relevance_category":  { "type": "string" },
    "relevance_score":     { "type": "number", "minimum": 0, "maximum": 1 },
    "raw_data":            { "type": "object" }
  }
}
```

*Cited:* this is the exact shape emitted by `ai-architect-feedback-loop/scripts/tv_markdown_to_json.py:191-206` — the current "ground truth" for what a finding looks like coming out of the TV ingestion side. Every field name is used verbatim.

*Required vs optional:* only `id`, `title`, and `relevance_category` are required, because those are the fields stage 1 cannot do its job without (identity, human-readable anchor, and the category that selects the downstream routing). `description`, `source_url`, `relevance_score`, and `raw_data` are optional and MUST be preserved when present (§6) but are not mandatory on input.

*Rejected the old "score floor":* the feedback-loop stage 1 hard-rejects findings with `relevance_score < 0.5` (`parse_findings.py:95-117`, threshold from `config/thresholds.json:3`). This spec does **not** port that filter into stage 1 of the MCP. Reason: the score 0.5 comes from `thresholds.json` but is itself computed by a hack in `tv_markdown_to_json.py:181-188` — importance string divided by 10 with a `post_worthy → 0.9` clamp. That is not a calibrated quantity; it is a placeholder. Propagating it into the new stage 1 would bake the placeholder into the protocol. Filtering by score, if it is kept at all, belongs to a later, explicitly-scoring stage whose axioms match the question being asked. **`relevance_score` is preserved but not acted upon at stage 1.**

### 3.3 Accepted source forms (how the tool resolves `finding` into §3.2)

Three forms, in priority order when a path is given:

| # | Form | How stage 1 reads it |
|---|------|----------------------|
| 1 | Inline object | Validate against §3.2 directly. Required fields → error on missing. |
| 2 | Absolute path to a `.json` file | Read file, JSON-parse. If the root is an object with a `findings` array, take `findings[0]` and fail if the array has more than one element (one MCP call = one finding; orchestrator must iterate). If the root matches §3.2, use it directly. Otherwise error. |
| 3 | Absolute path to a `.md` file | Treated as a **pre-extracted single-finding** Technical Veil markdown file. Parse using the section/table rules already implemented by `tv_markdown_to_json.py:117-211`. If the file contains multiple `### N.M` findings, return an error naming how many were found — the caller must split first. |

*Rejected the markdown-directory walk:* `tv_markdown_to_json.py:68-91` walks a TV date directory and parses every actor file. This is ingestion, not extraction. In the MCP rewrite, ingestion is the caller's job; the tool operates on one finding. The orchestrator agent is free to call `extract_finding` N times over a directory — stage 1 is not responsible for the iteration.

*Open question (§8):* whether form 3 (markdown) should be supported in the first cut at all, or whether the caller should always convert to JSON first.

---

## 4. Outputs

Two outputs, in two different channels:

- **MCP response** — a small JSON object confirming the write and echoing the artifact path. For the calling agent.
- **On-disk artifact** — the refined-prompt artifact at a deterministic path under `output_dir`. For stage 2.

### 4.1 Extracted-information schema (the deterministic half)

Pure function of the input. Produced before the orchestrator is consulted, and written as its own field inside the artifact so stage 2 can audit the refinement.

```json
{
  "type": "object",
  "required": ["finding_id", "title", "relevance_category", "extracted_at"],
  "properties": {
    "finding_id":         { "type": "string" },
    "title":              { "type": "string" },
    "description":        { "type": "string" },
    "source_url":         { "type": "string" },
    "relevance_category": { "type": "string" },
    "relevance_score":    { "type": ["number", "null"] },
    "raw_data":           { "type": "object" },
    "extracted_at":       { "type": "string", "format": "date-time" },
    "extractor_version":  { "type": "string" },
    "source_form":        { "type": "string", "enum": ["inline", "json_file", "markdown_file"] },
    "source_path":        { "type": ["string", "null"] }
  },
  "additionalProperties": false
}
```

*Rationale for every field:*
- `finding_id` → identity. Required because it is the key stage 2 uses to reference back.
- `title` → human-readable anchor for logs, dashboards, PR titles.
- `description`, `source_url`, `relevance_category`, `relevance_score`, `raw_data` → verbatim pass-through from §3.2. Preservation is an invariant (§5).
- `extracted_at` → ISO-8601 UTC timestamp. Cited: same timestamp format used by `parse_findings.py:127` (`datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")`). Reuse the format so dashboards parse identically.
- `extractor_version` → string that pins the extractor's semantic version. When the extraction rules change, this bumps, and stage 2 can detect mixed-version runs.
- `source_form` + `source_path` → traceability. If the same finding is re-ingested from a different source, the provenance is in the artifact.

### 4.2 Refined-prompt schema (the orchestrator half)

The orchestrator agent reads the extracted object plus any stage-1-available context and produces a refined prompt. "Refinement" must be defined as data, not as prose. The refined prompt adds exactly these fields on top of §4.1:

```json
{
  "type": "object",
  "required": ["extracted", "refined_prompt", "refinement"],
  "properties": {
    "extracted":     { "$ref": "#/§4.1" },
    "refined_prompt": {
      "type": "object",
      "required": ["text", "role_hint"],
      "properties": {
        "text":       { "type": "string", "minLength": 1 },
        "role_hint":  { "type": "string", "description": "Which downstream stage consumes this prompt. Free-form in first cut; constrained later." },
        "token_estimate": { "type": ["integer", "null"] }
      }
    },
    "refinement": {
      "type": "object",
      "required": ["added_context", "orchestrator_version", "refined_at"],
      "properties": {
        "added_context": {
          "type": "array",
          "description": "List of context snippets the orchestrator added beyond the extraction. Each entry is self-describing.",
          "items": {
            "type": "object",
            "required": ["kind", "content"],
            "properties": {
              "kind":    { "type": "string", "description": "e.g. 'category_description', 'historical_precedent', 'related_finding_id'" },
              "content": { "type": "string" },
              "provenance": { "type": "string", "description": "Where this snippet came from. Required if non-trivial." }
            }
          }
        },
        "orchestrator_version": { "type": "string" },
        "refined_at":           { "type": "string", "format": "date-time" }
      }
    }
  }
}
```

**What makes a prompt "refined" (the operational definition):** the refined object equals the extracted object plus a non-empty `refined_prompt.text`, plus a (possibly empty) `added_context` array. "Refinement" is defined as the *difference* between the refined object and the extracted object. If `added_context` is empty and `refined_prompt.text` is just a reformat of `title + description`, that is a **valid** refinement (the minimal one) — but it is honest about its triviality. This prevents the orchestrator from silently producing "refinement" that contains nothing.

### 4.3 MCP response shape

```json
{
  "type": "object",
  "required": ["stage", "status", "finding_id", "artifact_path"],
  "properties": {
    "stage":          { "const": 1 },
    "status":         { "enum": ["ok", "error"] },
    "finding_id":     { "type": "string" },
    "artifact_path":  { "type": "string", "description": "Absolute path to the refined-prompt artifact written to disk." },
    "run_id":         { "type": "string" },
    "bytes_written":  { "type": "integer" },
    "extractor_version":   { "type": "string" },
    "orchestrator_version": { "type": "string" }
  }
}
```

Small on purpose. The artifact on disk is the source of truth; the MCP response is a receipt.

### 4.4 On-disk layout

Given `output_dir = /abs/path` and a `run_id` (provided or generated per §5.3):

```
/abs/path/
  runs/
    <run_id>/
      findings/
        <finding_id>/
          stage-1.extracted.json   # §4.1, deterministic
          stage-1.refined.json     # §4.2, full artifact (contains §4.1 as `extracted`)
          stage-1.source.json      # verbatim copy of the source after normalization to §3.2
      index.json                   # §5.2
```

*Rationale:*
- `runs/<run_id>/` — one level of grouping per pipeline run. Cited: `ai-architect-feedback-loop/skills/run-pipeline/SKILL.md:44` (`RUN="$REPO/runs/$RUN_TS"`).
- `findings/<finding_id>/` — one directory per finding, not one flat file. Lets stages 2+ add their own files (e.g. `stage-2.impact.json`) next to the stage-1 output without a naming collision. Cited: this is the same discipline used by the `<RUN>/findings.json` + `<RUN>/prioritized_findings.json` convention at `SKILL.md:147-156`, generalized to per-finding directories.
- `stage-1.extracted.json` separate from `stage-1.refined.json` — so an agent that only needs the deterministic extraction can read it without parsing a large refinement object. Also lets a future "re-refine without re-extract" path exist.
- `stage-1.source.json` — the canonical copy of the input after form resolution. If the original source disappears (e.g. the TV markdown directory is deleted), stage 1's output is still self-contained.
- `index.json` — see §5.2.

*Rejected:* writing everything into one monolithic `<run>/findings.json` the way `parse_findings.py:124-137` does. Rejected because that file is built by *batching* many findings at once; the MCP tool is per-finding and must be idempotent per-finding. One file per finding is the right unit.

---

## 5. Invariants

Things that must hold, or stage 1 has failed.

### 5.1 Finding-level invariants

1. **Preservation.** Every field present in the normalized §3.2 source object appears verbatim in `stage-1.extracted.json` under the corresponding key. No field is dropped. No field is renamed. Unknown fields (`additionalProperties: true` in §3.2) are preserved inside `raw_data` or at top level and round-tripped into `stage-1.source.json`.
2. **Idempotency.** Calling `extract_finding` twice with the same `finding` and the same `output_dir` + `run_id` produces byte-identical `stage-1.extracted.json` (modulo `extracted_at` timestamp) and overwrites the previous `stage-1.refined.json`. The second call must not crash on existing files.
3. **Determinism of extraction.** `stage-1.extracted.json` minus the `extracted_at` timestamp is a pure function of the input. Same input, same extractor version → identical bytes.
4. **Finding ID sanity.** `finding_id` must be filesystem-safe (no `/`, no `..`, no leading `.`). If the incoming `id` is not safe, stage 1 fails loudly; it does not silently rewrite it. *Open question (§8):* do we allow a sanitization map, or hard-fail?
5. **Non-empty refinement contract.** `refined_prompt.text` must not be the empty string. If the orchestrator produces an empty prompt, stage 1 returns `status: "error"` and writes no `stage-1.refined.json`.

### 5.2 Run-level invariants

1. **Unique appearance.** Every `finding_id` processed in a run appears exactly once in `runs/<run_id>/index.json`. Re-processing a `finding_id` within the same run updates the same entry — it does not add a duplicate.
2. **Index shape.** `index.json` is a JSON object:
   ```json
   {
     "run_id": "<run_id>",
     "started_at": "<first-call timestamp>",
     "last_updated_at": "<most-recent-call timestamp>",
     "findings": {
       "<finding_id>": {
         "artifact_path": "findings/<finding_id>/stage-1.refined.json",
         "extractor_version": "<semver>",
         "orchestrator_version": "<semver>",
         "refined_at": "<timestamp>"
       }
     }
   }
   ```
   Stage 2 discovers its inputs by reading `index.json` — it does not have to glob the filesystem.
3. **Atomicity of writes.** Writing `stage-1.refined.json` is atomic from stage 2's perspective: write to a tempfile and rename. Partial artifacts must never be visible. *Source:* standard POSIX `rename(2)` semantics; no citation needed beyond POSIX.

### 5.3 Run ID generation

If `run_id` is not passed, stage 1 generates one: `YYYYMMDD-HHMMSS` UTC. Cited: same format as `ai-architect-feedback-loop/skills/run-pipeline/SKILL.md:44`. Matching the old format means old tools (grep, ls) that already understand this layout keep working.

---

## 6. What stage 1 must never lose

The information upper bound. If stage 1 captures these, stage 2 can recover everything; if stage 1 drops any of these, they are gone for this run regardless of how clever stage 2 is.

1. **`id`** — without it there is no way to reference the finding downstream.
2. **`title`** — without it logs/PRs/dashboards lose their only human anchor.
3. **`relevance_category`** — the routing key. The old pipeline uses it to select which engine/module is affected via `ai-architect/02_Areas/Pipeline/category_engine_map.json:1-55`. Losing the category means losing the routing table.
4. **The full original source bytes, normalized** — written to `stage-1.source.json`. Every field on the input, including unknown ones, round-trips. Future stages may discover they need a field the current extractor didn't model; the raw copy is the escape hatch.
5. **`relevance_score` when present** — even though stage 1 does not filter on it (§3.2 rejection note), it must be preserved, because a future scoring stage will want the untouched number to recalibrate.
6. **Provenance** — `source_form` + `source_path` + `extracted_at`. Without provenance, debugging a bad run a week later is impossible.
7. **Refinement diff** — `added_context` array. Stage 2 must be able to tell *what the orchestrator added*, so a bad refinement is attributable to the orchestrator, not to the source.

What stage 1 is **allowed** to lose:
- Free-form markdown formatting of the source (once structured into §3.2, the original whitespace is not load-bearing).
- Transient IDs the orchestrator generates for its own temp state.
- Everything the source explicitly marks as "skip" (e.g. the `SKIP_FILES` set at `tv_markdown_to_json.py:61` — `INDEX.md`, `_daily_digest.md`, `_linkedin_angles.md` — are already filtered out at ingestion time before a finding is formed).

---

## 7. Source citations

Every borrowed shape and every reject:

| Decision | Source | Borrowed / rejected |
|---|---|---|
| Finding schema fields (`id`, `title`, `description`, `source_url`, `relevance_category`, `relevance_score`, `raw_data`) | `/Users/cdeust/Developments/ai-architect-feedback-loop/scripts/tv_markdown_to_json.py:191-206` | Borrowed verbatim |
| Timestamp format `YYYY-MM-DDTHH:MM:SSZ` | `/Users/cdeust/Developments/ai-architect-feedback-loop/scripts/parse_findings.py:127` | Borrowed |
| Run ID format `YYYYMMDD-HHMMSS` | `/Users/cdeust/Developments/ai-architect-feedback-loop/skills/run-pipeline/SKILL.md:44` | Borrowed |
| `runs/<RUN_TS>/` grouping | `SKILL.md:44`, same file line 147 | Borrowed, generalized to per-finding subdirs |
| `relevance_score < 0.5` filter | `parse_findings.py:95-117`; threshold from `ai-architect-feedback-loop/config/thresholds.json:3` and `/Users/cdeust/Developments/ai-architect/02_Areas/Pipeline/thresholds.json:3` | **Rejected at stage 1**; preserved as a pass-through field. Reason: the score is computed by an uncalibrated hack at `tv_markdown_to_json.py:181-188` (`importance/10` with a `post_worthy → 0.9` clamp), not a real quantity. Filtering on it at stage 1 would bake an unvalidated measure into the protocol boundary. |
| Default category list | `ai-architect-feedback-loop/scripts/parse_findings.py:25-40` (14 categories) vs `ai-architect/02_Areas/Pipeline/thresholds.json:4-23` (18 categories, with `agentic`, `safety`, `infrastructure`, `optimization` added) | **Neither borrowed as a whitelist.** Stage 1 accepts any string for `relevance_category` and preserves it. Whitelisting is a filtering/routing concern; see §8. |
| Monolithic `findings.json` output | `parse_findings.py:124-137` | **Rejected.** One file per finding, per §4.4. |
| Multi-source CLI resolution (`--tv-dir` / `--tv-input` / `--builder-dir`) | `ai-architect-feedback-loop/scripts/stage1-parse-findings.sh:41-79` | **Rejected.** Ingestion is not extraction; the MCP tool operates on one finding per call. |
| TV markdown directory walk | `tv_markdown_to_json.py:68-91`, `214-232` | **Rejected.** Caller iterates, tool extracts. |
| SKIP_FILES set | `tv_markdown_to_json.py:61` | Noted only; these are filtered at ingestion, not at stage 1. |
| `category_engine_map.json` routing | `/Users/cdeust/Developments/ai-architect/02_Areas/Pipeline/category_engine_map.json:1-55` | Not borrowed. Stage 1 preserves the category string; the mapping is a stage-2+ concern. |
| `ReviewRequest.swift` domain type | `/Users/cdeust/Developments/ai-architect/ai-architect/ai-architect/Domain/ReviewRequest.swift:4-14` | **Rejected as shape for stage 1.** `ReviewRequest` models a PR-review-loop concern (a pending `@claude` comment on an open PR), which is a later stage. Noted only so a future stage does not re-invent it. |
| `VisualizationNode` / `Edge` / `Graph` | `/Users/cdeust/Developments/ai-architect/ai-architect/ai-architect/Domain/` | **Rejected as shape for stage 1.** These model codebase graph structure, which is a later-stage concern (analysis of the target repo), not finding extraction. |
| MCP tool input schema (`finding` + `output_dir`) | `/Users/cdeust/Developments/anthropic/ai-automatised-pipeline/src/main.rs:77-90` | Borrowed (this spec tightens it per §3.1). |
| Growth rule: one stage = one tool, no pre-scaffolding | `/Users/cdeust/Developments/anthropic/ai-automatised-pipeline/NOTES.md:11-14` | Binding constraint on the spec. |

---

## 8. Open questions — things I cannot decide without more data or user input

1. **Markdown source form.** Should `extract_finding` accept `.md` paths directly (§3.3 form 3), or must the caller always convert to JSON first? Accepting markdown means porting the 200-line parser from `tv_markdown_to_json.py:117-211` into the Rust tool. Not accepting it means every caller needs a converter. Default recommendation: **defer** — ship with JSON-only in the first cut, revisit after the first real run shows whether callers actually pre-convert.
2. **Finding ID sanitization (invariant §5.1.4).** Hard-fail on unsafe IDs, or apply a deterministic sanitization map (e.g., replace `/` with `_`)? Sanitization is friendlier but creates a slippage between the `id` in the artifact and the `id` on disk, which breaks §5.1.1 (preservation). Default recommendation: **hard-fail**, document the rule, make the caller responsible.
3. **Category whitelist.** Old pipeline has two different whitelists: 14 categories in `parse_findings.py:25-40`, 18 categories in `ai-architect/02_Areas/Pipeline/thresholds.json:4-23` (adds `agentic`, `safety`, `infrastructure`, `optimization`). **Neither is authoritative.** At stage 1, I propose accepting any non-empty string and letting a later stage enforce routing. The user may want stage 1 to warn (not error) on unknown categories — needs decision.
4. **Orchestrator boundary.** This spec defines *what* the refined prompt artifact must contain but not *which agent* produces the refinement or *which model* it uses. The old `pipeline_policy.yaml:39-50` pins `stage_1: {primary: haiku, secondary: haiku}`. Is the orchestrator for the new stage 1 an in-process call from the MCP tool (blocking), or does the MCP return the extracted object and the orchestrator runs out-of-band? This is an architecture decision, not a schema decision, so I am not making it here. It affects whether `stage-1.refined.json` is written synchronously or appears later.
5. **`run_id` ownership.** If multiple orchestrator agents run in parallel against the same `output_dir`, they might collide on `run_id` (same second, different runs). Do we want a random suffix, a monotonic counter, or is "one orchestrator per output_dir per second" a safe assumption? Needs real-world load input.
6. **`extractor_version` and `orchestrator_version` registry.** Where do these strings live? In `Cargo.toml` (compile-time constant), in a separate `versions.json`, or hashed from the extractor source? Default recommendation: **compile-time constants in `src/main.rs`**, bumped by hand at first, automated later.
7. **What to do with `.md` files that contain multiple findings.** Today's answer: error out (§3.3 form 3). Alternative: silently extract only the first. Hard-fail is zetetically safer; the caller has to make the split explicit.
8. **Is `relevance_score` nullable?** §4.1 says yes. The old schema treats `0.0` as equivalent to absent (`parse_findings.py:106`). But `0.0` and "unknown" are different; preserving the distinction matters if a later stage will recalibrate. Needs confirmation that stage 2 will treat `null` correctly.

---

*This spec is the measure. The method (Rust implementation in `src/main.rs`) is deferred until the quantities above are accepted. No code until the shape is right.*

---

## 9. Locked decisions (supersede §8 open questions)

User-approved, 2026-04-11. These overrule anything ambiguous above.

### 9.1 Orchestrator boundary — **Option B: two MCP tools**

Stage 1 ships as **two** Rust tools, not one. The "one stage = one tool" rule is extended to accept sub-steps when one half is deterministic and the other is non-deterministic.

| Tool | Produces | Deterministic? | Orchestrator-aware? |
|---|---|---|---|
| `extract_finding` | `stage-1.extracted.json` + `stage-1.source.json` + initial `index.json` entry | Yes, pure function | No |
| `refine_finding` | `stage-1.refined.json` + updated `index.json` entry | Yes (pure persistence) | Consumes orchestrator output |

**Neither tool calls an LLM.** Both are pure Rust. The agent layer (the orchestrator in `.claude/agents/`) is responsible for:
1. Calling `extract_finding(finding, output_dir, run_id?)` → receives extracted object + artifact path.
2. Running the LLM refinement itself, producing the `refined_prompt` + `added_context` payload per §4.2.
3. Calling `refine_finding(run_id, finding_id, output_dir, refined_prompt, refinement_meta)` → persists and returns a receipt.

This keeps the Rust MCP LLM-free and provider-agnostic. Stage 1's contract is "safe data movement with invariants checked"; intelligence stays in the agent layer.

### 9.2 `refine_finding` tool schema

```json
{
  "type": "object",
  "required": ["run_id", "finding_id", "output_dir", "refined_prompt", "refinement"],
  "additionalProperties": false,
  "properties": {
    "run_id":      { "type": "string" },
    "finding_id":  { "type": "string" },
    "output_dir":  { "type": "string", "pattern": "^/.+" },
    "refined_prompt": {
      "type": "object",
      "required": ["text", "role_hint"],
      "properties": {
        "text":           { "type": "string", "minLength": 1 },
        "role_hint":      { "type": "string" },
        "token_estimate": { "type": ["integer", "null"] }
      },
      "additionalProperties": false
    },
    "refinement": {
      "type": "object",
      "required": ["added_context", "orchestrator_version"],
      "properties": {
        "added_context": {
          "type": "array",
          "items": {
            "type": "object",
            "required": ["kind", "content"],
            "properties": {
              "kind":       { "type": "string" },
              "content":    { "type": "string" },
              "provenance": { "type": "string" }
            },
            "additionalProperties": false
          }
        },
        "orchestrator_version": { "type": "string" }
      },
      "additionalProperties": false
    }
  }
}
```

**Preconditions** `refine_finding` must check before writing:
1. `runs/<run_id>/findings/<finding_id>/stage-1.extracted.json` exists. If not → `status: "error"`, `reason: "no_extraction"`.
2. `refined_prompt.text` is non-empty (invariant §5.1.5).
3. `run_id` and `finding_id` are filesystem-safe (invariant §5.1.4).

On success: writes `stage-1.refined.json` atomically (tempfile + rename), updates `index.json` atomically, returns a receipt matching §4.3 extended with `refined: true`.

### 9.3 Decisions on the other 7 open questions

| # | Question | Locked answer |
|---|---|---|
| 1 | Accept `.md` files directly? | **No — JSON-only in v1.** Form 3 in §3.3 is deferred. `extract_finding` accepts inline objects or absolute paths to `.json` files. `.md` path is an error. |
| 2 | Category whitelist? | **Permissive.** Any non-empty string in `relevance_category` is accepted and preserved verbatim. No warning. No enforcement. Routing is a later-stage concern. |
| 4 | Finding ID sanitization | **Hard-fail on unsafe IDs.** If `finding_id` contains `/`, `..`, a leading `.`, or any byte outside `[A-Za-z0-9._-]`, the tool returns `status: "error"` and writes nothing. Caller's responsibility. **Maximum length**: 128 bytes. Source: chosen to leave ample headroom under POSIX `NAME_MAX` (255 bytes on macOS HFS+/APFS and Linux ext4 per `<limits.h>`), while staying well under `PATH_MAX` (1024 on macOS) when composed into `runs/<run_id>/findings/<finding_id>/`. 128 bytes is an implementation-defined constant in `src/main.rs` (`SAFE_ID_MAX_LEN`) — bump it here if a use case demands more, never in the code alone. |
| 5 | `run_id` collision | **Auto-generated `run_id` format: `YYYYMMDD-HHMMSS-<6 lowercase alphanumeric>`.** The 6-char suffix is a uniform-random draw from `[a-z0-9]`, generated per-call at auto-gen time. Source: extension of the feedback-loop convention at `SKILL.md:44`; the 6-char random suffix follows the same structure as git short-hashes (7 chars) and Cargo's fingerprint directories, trimmed to 6 for readability. Collision probability over N runs ≈ N²/(2·36⁶) — negligible for any realistic run rate. |
| 6 | Version strings location | **Compile-time constants in `src/main.rs`.** Two constants: `EXTRACTOR_VERSION: &str = "1.0.0"` and `ORCHESTRATOR_CONTRACT_VERSION: &str = "1.0.0"`. The second is the version of the refinement schema the Rust tool accepts from the agent layer — bumped when §9.2 schema changes. Both bumped by hand. |
| 7 | Multi-finding `.md` files | Moot — §9.3.1 defers `.md` support entirely. |
| 8 | Is `relevance_score` nullable? | **Yes.** Preserved as JSON `null` when absent from input. `0.0` and `null` are distinct and round-trip distinctly. |

### 9.4 What the Rust implementer must do

1. Update `tools_list()` in `src/main.rs` to declare **two** tools: `extract_finding` and `refine_finding`, with the schemas in §3.1 (tightened) and §9.2 respectively.
2. Implement `extract_finding` per §4.1, §4.4, §5.1, §5.2. Writes `stage-1.extracted.json`, `stage-1.source.json`, and creates/updates `index.json` with a preliminary entry (no `refined_at`, no `orchestrator_version` yet).
3. Implement `refine_finding` per §9.2. Checks preconditions, writes `stage-1.refined.json` atomically, updates the existing `index.json` entry with `refined_at`, `orchestrator_version`, `artifact_path = findings/<finding_id>/stage-1.refined.json`.
4. No new crates beyond `serde` + `serde_json` unless absolutely necessary. If a new dep is added, it must be justified in a code comment with a source.
5. All schemas from this spec become Rust types with `serde` derives. No untyped `Value` except at the MCP wire boundary.
6. Every hardcoded constant (version strings, the `run_id` alphabet, the safe-ID regex) must be a named constant at the top of the file with a source comment.
7. `cargo check` + `cargo build --release` must pass. A smoke test must exercise both tools end-to-end: extract → refine → read the artifact back.
