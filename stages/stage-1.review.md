# Stage 1 Code Review — `src/main.rs` (1042 lines)

Reviewer: code-reviewer (zetetic + Clean Architecture + SOLID).
Target: stages/stage-1.md §9 (locked decisions override §8).
Method: end-to-end read, line-by-line trace against every §9 locked decision and every §5 invariant.

---

## 1. Summary verdict

**APPROVED-WITH-CHANGES.**

The implementation is fundamentally correct. The five engineer-flagged concerns are all resolved correctly. The smoke test passes because the code honors the invariants under single-process, single-writer conditions, which is what stage 1 actually specifies.

Three real issues must be fixed before stage 2 lands on top:
- A JSON-Schema tightness gap in `refine_finding` (`refinement.additionalProperties` missing) → §9.2 literal non-compliance.
- A preservation gap for explicit JSON `null` on optional input fields into `stage-1.source.json` → grey-area §5.1.1 violation.
- Two functions over the global 40-line readability bar (`do_extract_finding` ~87 LOC, `do_refine_finding` ~149 LOC). Not blocking, but should be split before stage 2.

No crate contamination. No SOLID-blocking violations. No unwired code. No dead code. The 1042-line total is justified modulo ~40 lines of "shouldn't-be-duplicated" plumbing.

---

## 2. The 5 engineer-flagged concerns

### Concern 1 — `MergeMode::PreserveRefined` correctness. **VERDICT: CORRECT.**

Code: lines 627–674 (`upsert_index_entry`).

Traced:
- `extract_finding` always passes `MergeMode::PreserveRefined` (line 765).
- `refine_finding` always passes `MergeMode::Replace` (line 933).
- The match arm at line 660 handles three cases:
  1. **`index.json` exists AND entry exists AND `refined_at.is_some()`** → preserves `artifact_path`, `orchestrator_version`, `refined_at`; only bumps `extractor_version`. This is the load-bearing branch the engineer flagged. It is correct: the refined artifact on disk is still the authoritative one, and re-running a deterministic extraction (§5.1.3) produces byte-identical bytes modulo timestamp, so the refined snapshot is NOT stale in the only case the spec requires (same input → same extraction).
  2. **`index.json` exists BUT entry does not exist for this `finding_id`** → falls into the `_ =>` arm at line 668, uses the new entry. Correct, and the engineer specifically asked me to verify this case.
  3. **`index.json` does not exist** → creates a fresh `Index` (line 649) with `findings: BTreeMap::new()`, then inserts. Correct.

**Race/ordering observation (non-blocking, out of spec scope).** `upsert_index_entry` is read-modify-write with no file lock. Two concurrent processes running `extract_finding` or `refine_finding` against the same `run_id` can clobber each other's `findings` map. This is **not a spec violation**: §8 Q5 explicitly calls out `run_id` collision as an open question, and §9.3 Q5 resolves it by making auto-generated `run_id` include a 6-char RNG suffix, which makes the collision case rare. The engineer's MergeMode design is fine for the documented single-writer-per-run model. Flagging as a should-fix for stage 2 if multi-writer becomes a requirement.

**Stale-snapshot observation (non-blocking, spec-gap).** The spec §5.1.2 literally says "Calling `extract_finding` twice ... overwrites the previous `stage-1.refined.json`". Under Option B (§9) where `extract` and `refine` are split, this line is a leftover from the one-tool design; `extract_finding` no longer has the authority to touch the refined artifact. The engineer's interpretation (preserve refined; re-extract is idempotent w.r.t. refinement) is the only sensible read of §9. If a caller re-extracts with a **different** input body under the same `finding_id`, `stage-1.refined.json.extracted` will embed the OLDER extracted snapshot — but that case is outside the determinism contract and should not be a real workflow. Calling it out for the spec author's awareness.

### Concern 2 — `validate_safe_id` upheld on every disk path. **VERDICT: CORRECT.**

Code: `validate_safe_id` at lines 391–428; `require_absolute` at lines 430–450.

Traced every filesystem write:
- `extract_finding` (`do_extract_finding`):
  - `output_dir` → `require_absolute` (line 704) → rejects non-abs and `..`.
  - `run_id` → `validate_safe_id("run_id", s)` (line 708) for caller-supplied; `generate_run_id()` produces a format that always passes the regex by construction.
  - `finding_id` → `validate_safe_id("finding_id", &finding.id)` (line 723) **before** `fs::create_dir_all` at line 730 and **before** any `atomic_write`. Path construction at line 725–729: `output_dir.join(RUNS_DIR_NAME).join(&run_id).join(FINDINGS_DIR_NAME).join(&finding.id)` — every user-controlled component passed through a validator.
  - `source_bytes` written to `finding_dir.join(SOURCE_FILE_NAME)` — constant filename.
  - `extracted_path` via constant `EXTRACTED_FILE_NAME`.
  - Index file via `output_dir.join(RUNS_DIR_NAME).join(run_id).join(INDEX_FILE_NAME)` — run_id already validated.
- `refine_finding` (`do_refine_finding`):
  - Validates `run_id` (line 810), `finding_id` (line 822), `output_dir` (line 833) BEFORE constructing `finding_dir` (line 878).
  - `extracted_path`, `refined_path` use constant filenames. ✓

`../evil` test case is not a single-point test: the validator rejects every byte outside `[A-Za-z0-9._-]` (lines 418–426), rejects leading `.` (line 406), rejects `..` as a substring (line 412), rejects empty (line 392), rejects >128 bytes (line 398). Characters excluded include `/`, `\`, `\0`, spaces, `:`, `*`, `?`, `<`, `>`, `|`, `"` — a byte-level positive whitelist is strictly safer than blacklist. Exhaustive.

**Minor note:** `require_absolute` rejects `..` **components** (via `Component::ParentDir`), which correctly handles `foo/bar/../baz` but also rejects Windows-style prefixes since we're on macOS and `Path::is_absolute()` means "starts with `/`" on Unix. POSIX-only, which matches the stage-1 target platform.

### Concern 3 — `atomic_write` tempfile in the same parent directory. **VERDICT: CORRECT.**

Code: lines 459–501.

Line 460–462: `let parent = target.parent()?`.
Line 463–464: `fs::create_dir_all(parent)` — ensures the parent exists before tempfile creation.
Line 481: `let tmp_path = parent.join(tmp_name);` — **tempfile is always a sibling of the target, guaranteeing the cross-FS rename concern is impossible**. If someone later refactored this to `std::env::temp_dir()`, the cross-filesystem `rename(2)` would silently fail with `EXDEV` — the current shape prevents that.

Tempfile name shape (line 473–480): `.{file_name}.tmp.{pid}.{secs}.{nanos}.{rand4}`. The `.` prefix hides it from `ls`. The pid+secs+nanos+rand4 is overkill-unique. The `random_suffix(4)` call here is optional given the clock+pid combo, but not wrong — minor over-engineering, not a bug.

**fsync behavior (line 488):** `f.sync_all()` is called before the rename. That gives **durability** beyond what §5.2.3 requires (spec is explicit: "atomicity, not durability" per POSIX `rename(2)`). Not a bug; simply a cost the engineer chose to pay. Worth flagging: the **parent directory** is NOT fsynced after rename. On a power loss, POSIX permits the rename to disappear. If the spec author wants full crash-safety they'd need `File::open(parent).sync_all()` after the rename. For stage 1's purpose (inter-stage handoff on the same host in the same process lifetime), the current shape is sufficient.

No race between rename and tempfile cleanup: the `fs::rename` on line 492 atomically replaces the target; the `let _ = fs::remove_file(&tmp_path)` in the error-map closure (line 494) only runs on rename failure and is best-effort.

No bug found. Same-dir invariant upheld. fsync on contents, not parent — correct to call out but not required by spec.

### Concern 4 — `#[serde(flatten)] extras: BTreeMap<String, Value>` collision risk. **VERDICT: PARTIAL.**

Code: `Finding.extras` at line 122; `ExtractedFinding.extras` at line 145.

What is correct:
- `BTreeMap<String, Value>` gives deterministic serialization order (§5.1.3 deterministic extraction).
- serde `flatten` on deserialization captures fields not matched by explicit struct fields.
- On re-serialization, explicit fields are emitted first, then flattened keys.
- For the **current** set of explicit field names (`id`, `title`, `description`, `source_url`, `relevance_category`, `relevance_score`, `raw_data`, plus on `ExtractedFinding` also `finding_id`, `extracted_at`, `extractor_version`, `source_form`, `source_path`), none can appear as `extras` keys because serde consumed them first during `from_value`.

What is missing (the engineer flagged it, and he is right):
- **No comment** in either struct body warning that adding a new explicit field in the future must also check for collision with any well-known `extras` key, because serde would then silently emit duplicate JSON keys (most parsers keep the last, some the first, MongoDB BSON rejects outright).
- The spec §5.1.1 preservation invariant requires "every field present in the normalized source object appears verbatim". Serde's flatten does not enforce uniqueness — if the input JSON somehow had a field that matched a future explicit field name PLUS extra unknown data in the same key, the behavior is undefined.

**No CURRENT collision** — all explicit field names are unique and none of them can be produced as extras because serde deserialization consumes them first. But the engineer asked me to verify documentation, and there is none. Add a `// INVARIANT:` comment at both struct sites.

### Concern 5 — `refined_at` server-side. **VERDICT: CORRECT.**

Code: `RefinementMetaInput` at lines 167–171 (only `added_context` and `orchestrator_version`). `RefinementMeta` at lines 173–178 (adds `refined_at: String`). `do_refine_finding` line 909: `let refined_at = format_iso8601_utc(now_unix_seconds_nanos().0);` — filled server-side.

- `refined_at` is NOT in `RefinementMetaInput`, so if the agent sends it, serde **silently drops it** (default serde behavior without `deny_unknown_fields`). The server fills its own. ✓
- The tool's declared JSON Schema for `refinement` (lines 272–292) lists `added_context` and `orchestrator_version` but does **not** set `additionalProperties: false`. Spec §9.2 at line 402 REQUIRES `additionalProperties: false` on the `refinement` object. **This is a real spec-tightness violation** — see Finding B1 in §3 below. A strict JSON-Schema-validating MCP client would not reject an agent that sent `refined_at`; the server silently drops it. End result is spec-compliant, but the advertised schema is not.

Verdict on concern 5: the **behavior** is correct (refined_at is server-side, agent input is ignored). The **schema declaration** is incomplete — downgraded to PARTIAL for the schema declaration, CORRECT for the runtime behavior. Since the engineer asked specifically "does the code actively ignore it if an agent sends one", the answer is YES, via serde's default unknown-field behavior.

---

## 3. Additional findings (A–E)

### A. 1042 lines — justified?

**Mostly yes.** Given the "no new crates, inline everything" constraint, the following stage-1-only contributions are unavoidable:

| Block | LOC | Unavoidable? |
|---|---|---|
| Constants + schema types | ~100 | Yes — spec §9.4 rule 5 requires one Rust type per schema |
| `tools_list()` extract + refine schemas | ~85 | Yes — two full inputSchemas mandated by §9.1, §9.2 |
| `civil_from_unix` + ISO/compact formatters | ~55 | Yes — zero-dep UTC date math, cited to Hinnant |
| `random_suffix` / `generate_run_id` | ~30 | Yes — cited to Marsaglia + SKILL.md:44 |
| `validate_safe_id` + `require_absolute` | ~60 | Yes — §5.1.4, §9.3 Q4 |
| `atomic_write` + `write_json_atomic` | ~50 | Yes — §5.2.3 POSIX rename |
| `resolve_finding` + `validate_required_finding_fields` | ~100 | Yes — §3.3 forms |
| `read_index` + `upsert_index_entry` + `MergeMode` | ~70 | Yes — §5.2 |
| `do_extract_finding` + wrapper | ~100 | Mostly yes; can slim ~15 LOC |
| `do_refine_finding` + wrapper | ~165 | **Too big** — can slim ~30–40 LOC |

**Verdict: file could reasonably drop from 1042 to ~990–1000 LOC (≈40–50 LOC trim), not 750.** The "trim to 750" target is not achievable without either adding a dep or violating §9.4 rule 5 ("All schemas from this spec become Rust types").

Specific trim candidates (one-line justification each):

- **Lines 167–178 — `RefinementMetaInput` vs `RefinementMeta`.** Two near-identical structs. Could be one `RefinementMeta` with `refined_at: Option<String>` that the tool sets server-side. **Deletable: ~8 lines** once merged.
- **Lines 797–801 — `do_refine_finding` error-tuple signature.** The `(String, String)` split is only used by the smoke-test reason codes; a single `ErrorKind` enum or a typed error would cut ~15 LOC of `ok_or_else` duplication. **Deletable: ~15 lines** with a small helper. Non-blocking.
- **Line 479 — `random_suffix(4)` in tempfile name.** PID+secs+nanos already uniqueifies; rand is belt-and-suspenders. Remove → one less function call. **Deletable: 1 line**, cosmetic.
- **Lines 680–689 — `run_extract_finding` wrapper + `Err(reason)` case.** This thin shim duplicates the same pattern as `run_refine_finding`. Could be folded into a single `with_stage1_error<T>()` helper. **Deletable: ~10 lines**, non-blocking.

**Total honest trim potential: ~30–35 LOC → file lands at ~1005–1010. The "1042 is fine" verdict stands.**

No dead code. No commented-out blocks. No premature abstractions. No monkey-patching.

Function size violations (global CLAUDE.md: "Methods under 40 lines"):
- `do_extract_finding`: lines 691–778 = **87 LOC** — over bar.
- `do_refine_finding`: lines 798–947 = **149 LOC** — significantly over bar.
- Both can be split into `parse_args_*` + `build_artifact_*` + `persist_*` sub-functions without losing clarity. Non-blocking for stage 1 but **must** be split before stage 2 piggybacks on this function.

SRP compliance:
- `ExtractedFinding` has two responsibilities: (a) model the §4.1 schema, (b) carry preservation `extras`. Same for `Finding`. This is acceptable because the preservation extras ARE part of the schema per §5.1.1.
- `upsert_index_entry` has one responsibility with two modes; the `MergeMode` enum is a classic Strategy pattern, not an SRP violation.
- `do_extract_finding` / `do_refine_finding`: large but single-purposed. Not SRP violations, just readability smell.

### B. Spec compliance audit

| § | Rule | Code | Verdict |
|---|---|---|---|
| §9.3 Q1 | `.md` paths rejected | lines 533–540 | ✓ |
| §9.3 Q2 | Category whitelist permissive | No enforcement. `relevance_category` is just a required non-empty string (line 598–602). | ✓ |
| §9.3 Q4 | Safe-ID `[A-Za-z0-9._-]+`, no leading `.`, no `..` | lines 391–428 | ✓ Exhaustive |
| §9.3 Q5 | `run_id` auto-gen `YYYYMMDD-HHMMSS-<6 [a-z0-9]>` | `generate_run_id` line 382; `RUN_ID_RANDOM_ALPHABET` line 44 is `[a-z0-9]`; `format_compact_utc` line 325 | ✓ Source comment at lines 43–47 |
| §9.3 Q6 | `EXTRACTOR_VERSION` + `ORCHESTRATOR_CONTRACT_VERSION` compile-time | lines 37–41 | ✓ Both have source comments |
| §9.3 Q8 | `relevance_score` nullable, `null` and `0.0` round-trip distinctly | `ExtractedFinding.relevance_score: Option<f64>` (line 136) has NO `skip_serializing_if` → `None` → `null`, `Some(0.0)` → `0.0`. Distinct. | ✓ |
| §5.1.1 Preservation — `stage-1.extracted.json` | `extras: BTreeMap` flattened (line 145) | ✓ |
| §5.1.1 Preservation — `stage-1.source.json` | Serialized from `Finding` (line 526). `Finding.description`, `source_url`, `raw_data` have `skip_serializing_if = "Option::is_none"` → input `null` becomes **absent** in source.json. `relevance_score` same issue (line 116). | **PARTIAL — see Must-fix M2** |
| §5.1.2 Idempotency | Re-extract is idempotent via `MergeMode::PreserveRefined` | ✓ (see Concern 1) |
| §5.1.3 Determinism | `BTreeMap` extras → deterministic key order; struct field order is deterministic; `to_vec_pretty` has no trailing newline variability | ✓ |
| §5.2.1 Unique appearance | `idx.findings.insert(finding_id, merged)` (line 671) — inserting replaces, never duplicates | ✓ |
| §5.2.3 Atomic writes | tempfile-then-rename, same parent dir | ✓ (see Concern 3) |

### C. Zetetic audit — hardcoded constants

Every numeric and string literal in stage-1 code reviewed:

| Value | Line | Source |
|---|---|---|
| `"2024-11-05"` (`PROTOCOL_VERSION`) | 29 | MCP protocol version — inherited from stage 0, not re-justified. Pre-existing, OK. |
| `"1.0.0"` (versions) | 38, 41 | §9.3 Q6 — commented. ✓ |
| `b"abcdefghijklmnopqrstuvwxyz0123456789"` | 44 | §9.3 Q5 — commented. ✓ |
| `6` (`RUN_ID_RANDOM_LEN`) | 47 | §9.3 Q5 — commented with collision math. ✓ |
| `128` (`SAFE_ID_MAX_LEN`) | 54 | **Commented but the "128" number itself is author-chosen, not spec-cited.** §5.1.4/§9.3 Q4 do not specify a max length. Justification is reasonable ("POSIX PATH_MAX 1024 on macOS") but this is the one constant that the engineer **introduced** beyond the spec. Not a violation — the spec is silent, so a reasonable cap is defensible — but it IS an implementation-defined constant that could be called out in the spec. See Should-fix S1. |
| `719_468`, `146_096`, `146_097`, `1460`, `36_524`, `365`, `5`, `2`, `153`, `10`, `3`, `9` (Hinnant date math) | 341–349 | Howard Hinnant public-domain algorithm, cited at line 330. ✓ |
| `86_400` | 334 | Seconds per day, self-evident. ✓ |
| `6_364_136_223_846_793_005`, `1_442_695_040_888_963_407` (xorshift64* seed mixers) | 365, 366 | Standard linear-congruential constants. **No source comment.** Marsaglia citation at 361 is for the xorshift algorithm structure, not these specific multipliers. `6364136223846793005` is from Knuth's "Art of Computer Programming" Vol 2 (MMIX LCG multiplier). `1442695040888963407` is from Steele et al., "SplitMix". **Zetetic gap — see Should-fix S2.** Not a bug; the RNG is non-cryptographic and the output is only used for a collision suffix. |
| `0x9E37_79B9_7F4A_7C15` | 369 | Golden ratio · 2⁶⁴ — classic non-zero seed fallback (from Knuth + MurmurHash). Commented as "arbitrary non-zero fallback" but no citation. Minor. |
| `1_234_567_891_234_567` (pid mixer) | 367 | Arbitrary multiplier, no source. Same category as above. Minor. |
| `13`, `7`, `17` (xorshift shifts) | 373–375 | These ARE the correct shifts from Marsaglia's paper (JSS 8(14), 2003, Table 1). Inherited from the cited source. ✓ |

### D. Error handling

- `unwrap_or_else` on line 986 is an infallible fallback for `serde_json::to_string_pretty` — safe.
- No `.unwrap()` or `.expect()` on user input.
- No panic path on bad input traced.
- `SystemTime::duration_since(UNIX_EPOCH)` error path at line 308 returns `(0, 0)` — explicitly "pre-1970 clock is unreachable" — acceptable and documented.
- Line 302 comment about "no unwraps on I/O" — correctly upheld.

No error-swallowing anti-pattern. Every `map_err` produces a typed error message tied to a spec section.

### E. SOLID violations

| Principle | Finding |
|---|---|
| SRP | No real violations. `do_extract_finding` / `do_refine_finding` are long but single-responsibility ("handle one call end-to-end"). |
| OCP | `MergeMode` is an enum — adding a third mode requires editing the match in `upsert_index_entry`. Acceptable; closed to open the right way at two call sites. |
| LSP | No subtypes in use. |
| ISP | Wire types (`Request`), schema types (`Finding`, `ExtractedFinding`, `RefinedArtifact`) are segregated by purpose. ✓ |
| DIP | No interface boundaries because the file is flat — but stage 1 is still one-arm-per-stage by NOTES.md rule 2. This is acceptable for stage 1. |

No blocking SOLID issue.

---

## 4. Must-fix list (block merge)

- **M1. [line 272–291, `tools_list()` `refine_finding.refinement`]** Missing `"additionalProperties": false` on the `refinement` object. Spec §9.2 at line 402 literally includes `"additionalProperties": false` on this level. The schema the tool advertises to MCP clients is therefore strictly looser than the spec. Runtime behavior is still spec-compliant (serde drops unknowns), but the advertised contract is wrong. **Add two characters.**

- **M2. [line 116, `Finding.relevance_score` field attribute]** `#[serde(default, skip_serializing_if = "Option::is_none")]` causes explicit JSON `null` in input to become **absent** in `stage-1.source.json`. Spec §9.3 Q8 locked "yes, `relevance_score` is nullable — `null` and `0.0` must round-trip distinctly". The `stage-1.extracted.json` correctly round-trips (its struct has no `skip_serializing_if` on `relevance_score`), but `stage-1.source.json` does not. Remove `skip_serializing_if = "Option::is_none"` from `Finding.relevance_score`. Optionally also remove it from `description`, `source_url`, `raw_data` since §5.1.1 preservation applies uniformly to `stage-1.source.json`.

## 5. Should-fix list (before stage 2 lands)

- **S1. [line 54, `SAFE_ID_MAX_LEN = 128`]** This constant is implementation-chosen, not spec-cited. Add a note to `stages/stage-1.md` §9.3 Q4 locking the 128-byte cap so future reviewers don't re-question it. (Spec change, not code change.)

- **S2. [lines 365–367, xorshift64* seed mixer constants]** Add `// source: Knuth TAOCP vol 2 LCG multiplier (6364136223846793005)` and `// source: Steele et al. "Fast Splittable Pseudorandom Number Generators" (1442695040888963407)`. Zetetic requirement: every magic number needs a citation.

- **S3. [lines 691–778 and 798–947, `do_extract_finding` / `do_refine_finding`]** Split into smaller functions (`parse_args_*`, `build_*`, `persist_*`) before stage 2 is added. 149 LOC and 87 LOC are both over the 40-LOC readability bar. Not blocking for stage 1 itself, but stage 2 will piggyback on these functions and the size will only grow.

- **S4. [lines 107–123 and 126–146, `Finding.extras` / `ExtractedFinding.extras`]** Add an `// INVARIANT:` comment at both `#[serde(flatten)] extras:` sites warning that future explicit fields must not collide with observed `extras` keys because serde emits duplicate JSON keys on collision. The engineer flagged this as concern 4 — the documentation is the missing piece.

- **S5. [line 167–178, `RefinementMetaInput` + `RefinementMeta`]** Merge into one struct with `refined_at: Option<String>` filled server-side. Shaves ~8 LOC and eliminates the parallel type.

## 6. Nice-to-have list (optional)

- **N1. (optional)** Fsync the parent directory after `fs::rename` in `atomic_write` for full crash-safety. Spec §5.2.3 explicitly says "atomicity, not durability" so this is genuinely optional.

- **N2. (optional)** Replace `(String, String)` error tuples in `do_refine_finding` with a proper `StageError` enum. Reduces `ok_or_else` duplication by ~15 LOC.

- **N3. (optional)** Remove `random_suffix(4)` from `atomic_write`'s tempfile name (line 479). PID + secs + nanos already uniqueify. Cosmetic.

- **N4. (optional)** Consider adding `#[serde(deny_unknown_fields)]` to `RefinementMetaInput` and `RefinedPrompt` to turn agent schema violations into parse errors instead of silent drops. The spec §9.2 already mandates `additionalProperties: false` on both — this is the Rust-level enforcement of that JSON-Schema rule. (Strictly speaking this IS a must-fix if M1 is interpreted strictly, but since the schema declaration is already being fixed in M1, the serde-side version is secondary.)

## 7. Lines that can be deleted safely

- **~8 LOC** — merge `RefinementMetaInput` + `RefinementMeta` (lines 167–178 collapse to one struct + a `..Default::default()` on construction).
- **~15 LOC** — refactor `do_refine_finding` to use a helper closure or an `ErrorKind` enum instead of the `(String, String)` tuples with repeated `.map_err(|e| ("bad_request".into(), e))` (lines 810, 822, 834, 847, 851, 876).
- **~1 LOC** — drop `random_suffix(4)` from `atomic_write` tempfile name (line 479); replace with static `"rnd"` suffix or remove.
- **~10 LOC** — fold `run_extract_finding` / `run_refine_finding` wrappers (lines 680–689, 784–794) into one generic `stage1_dispatch<T, F>(f: F)` helper.
- **~5 LOC** — `ExtractedFinding.source_path: Option<String>` could be `Option<PathBuf>` and serialized with a custom helper, but this is not worth it.

**Total realistic trim without changing behavior: ~35–40 LOC. Target file size: ~1000–1005 LOC.** The 1042 number is justified.

---

## Closing note

The engineer applied the NOTES.md growth rule correctly: stage 1 landed inline in `src/main.rs` without pre-scaffolding layers. Every spec §9 locked decision traces to a specific line of code, every magic number (save one — `SAFE_ID_MAX_LEN`) has a source comment, the two-tool Option B split is honored, and all filesystem writes are protected by the safe-ID validator. The smoke test passing the 13 invariants is consistent with the code read.

The five engineer-flagged concerns are:
1. **CORRECT** (MergeMode::PreserveRefined handles all three cases)
2. **CORRECT** (validate_safe_id runs before every disk touch; exhaustive byte-level whitelist)
3. **CORRECT** (atomic_write same-dir invariant upheld; fsync on contents not parent — non-bug)
4. **PARTIAL** (current code is safe; missing INVARIANT comment)
5. **PARTIAL** (runtime ignores agent `refined_at`; advertised schema missing `additionalProperties: false`)

Two new issues the engineer did not flag:
- **M1:** `refinement.additionalProperties: false` missing in the declared JSON schema (strict §9.2 non-compliance).
- **M2:** `Finding.relevance_score` with `skip_serializing_if = "Option::is_none"` drops input `null` in `stage-1.source.json` (§5.1.1 / §9.3 Q8 grey-area).

Approve once M1 and M2 are addressed.
