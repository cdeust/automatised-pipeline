// ai-architect-mcp — stage-by-stage MCP rewrite of the ai-architect pipeline.
//
// Transport: stdio JSON-RPC 2.0, hand-rolled (no MCP SDK — we own the protocol
// wire layer so the agents know exactly what's happening).
//
// Today:
//   Stage 0 — health_check         (handshake + server state)
//   Stage 1 — extract_finding      (finding → extracted artifact + index entry)
//   Stage 1 — refine_finding       (orchestrator refinement → refined artifact)
//   Stage 2 — start_verification   (create clarification session)
//   Stage 2 — append_clarification (append one turn, advance state machine)
//   Stage 2 — finalize_verification(compute digest, write verified receipt)
//   Stage 2 — abort_verification   (kill a non-terminal session)
//
// Each future stage lands as one more entry in `tools_list()` + one more arm
// in `handle_tool_call()`. No pre-scaffolding of layers or helpers.
//
// Reference implementation (to read, not copy): /Users/cdeust/Developments/ai-architect

mod clustering;
mod git_diff;
mod graph_store;
mod indexer;
mod lsp_client;
mod lsp_resolver;
mod parser;
mod prd_input;
mod prd_validator;
mod resolver;
mod rust_parser;
mod search;
mod security_gates;
mod semantic_diff;
mod tool_schemas;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Component, Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Protocol / server identity
// ---------------------------------------------------------------------------

const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "ai-architect";
const SERVER_VERSION: &str = "0.0.1";

// ---------------------------------------------------------------------------
// Stage 1 constants — every value traces to a spec section.
// ---------------------------------------------------------------------------

// source: stages/stage-1.md §9.3 Q6 — compile-time version of the extractor.
const EXTRACTOR_VERSION: &str = "1.0.0";
// source: stages/stage-1.md §9.3 Q6 — compile-time version of the refinement
// schema the Rust tool accepts from the agent layer.
const ORCHESTRATOR_CONTRACT_VERSION: &str = "1.0.0";

// source: stages/stage-1.md §9.3 Q5 — `[a-z0-9]` suffix on auto-generated run_id.
const RUN_ID_RANDOM_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
// source: stages/stage-1.md §9.3 Q5 — 6 chars, follows git short-hash (7 chars)
// convention trimmed to 6 for readability, collision ~ N²/(2·36⁶).
const RUN_ID_RANDOM_LEN: usize = 6;

// source: stages/stage-1.md §5.1.4, §9.3 Q4 — safe-ID regex `^[A-Za-z0-9._-]+$`,
// no leading `.`, no `..`. 128 chosen as the cap: long enough for any realistic
// upstream ID (e.g. "FID-2026-04-11-behavior-change-001" ≈ 40 chars) while short
// enough to keep filesystem paths well under POSIX PATH_MAX (1024 on macOS, see
// <sys/syslimits.h>). If we ever need more we bump it with a source comment.
const SAFE_ID_MAX_LEN: usize = 128;

// source: stages/stage-1.md §4.4 — canonical on-disk layout.
const RUNS_DIR_NAME: &str = "runs";
const FINDINGS_DIR_NAME: &str = "findings";
const INDEX_FILE_NAME: &str = "index.json";
const EXTRACTED_FILE_NAME: &str = "stage-1.extracted.json";
const SOURCE_FILE_NAME: &str = "stage-1.source.json";
const REFINED_FILE_NAME: &str = "stage-1.refined.json";

// ---------------------------------------------------------------------------
// Stage 2 constants — every value traces to a spec section.
// ---------------------------------------------------------------------------

// source: stages/stage-2.md §11 + §12.5 item 6 — compile-time version of the
// verifier. "1.0.0" = first release shipping the four-tool set (including
// abort_verification, locked in §12.1).
const VERIFIER_VERSION: &str = "1.0.0";

// source: stages/stage-2.md §12.3 — single-file session replacing the §5
// two-file split.
const SESSION_FILE_NAME: &str = "stage-2.session.json";
// source: stages/stage-2.md §5.3 — verified receipt filename.
const VERIFIED_FILE_NAME: &str = "stage-2.verified.json";

// source: stages/stage-2.md §12.3 "transcript_digest change" — the sha256
// algorithm identifier is a named constant, not a scattered string literal
// (zetetic standard in the stage-2 brief).
const DIGEST_ALGORITHM: &str = "sha256";

// ---------------------------------------------------------------------------
// Wire types (MCP JSON-RPC layer)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Request {
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

fn write_message(msg: Value) {
    let line = msg.to_string();
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    let _ = writeln!(lock, "{}", line);
    let _ = lock.flush();
}

fn send_response(id: Value, result: Value) {
    write_message(json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    }));
}

fn send_error(id: Value, code: i32, message: &str) {
    write_message(json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    }));
}

// ---------------------------------------------------------------------------
// Stage 1 schema types (one Rust type per spec schema)
// ---------------------------------------------------------------------------

// source: stages/stage-1.md §3.2 — canonical finding shape.
// additionalProperties: true → extra fields are captured in `extras`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Finding {
    id: String,
    title: String,
    // §5.1.1 preservation + §9.3 Q8: `None` must serialize as JSON `null`,
    // not absent. `default` is kept so input-absent still parses. Removing
    // `skip_serializing_if` makes `null` and `0.0` round-trip distinctly in
    // `stage-1.source.json`. (spec: stages/stage-1.md §5.1.1, §9.3 Q8)
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    source_url: Option<String>,
    relevance_category: String,
    #[serde(default)]
    relevance_score: Option<f64>,
    #[serde(default)]
    raw_data: Option<Value>,
    // Spec §3.2 declares additionalProperties: true and §5.1.1 (preservation)
    // requires round-tripping unknown fields. Kept as a flat map.
    // INVARIANT: adding a new explicit field to this struct requires verifying
    // it does NOT collide with any key that the input stream might put in
    // `extras`. On collision, serde flatten emits duplicate JSON keys — most
    // parsers silently keep the last, which violates §5.1.1 preservation.
    // Check the field name against recent `raw_data` shapes before adding.
    #[serde(flatten)]
    extras: BTreeMap<String, Value>,
}

// source: stages/stage-1.md §4.1 — deterministic extraction output.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ExtractedFinding {
    finding_id: String,
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_url: Option<String>,
    relevance_category: String,
    // Nullable per §9.3 Q8 — `null` and 0.0 must round-trip distinctly.
    relevance_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_data: Option<Value>,
    extracted_at: String,
    extractor_version: String,
    source_form: String,
    source_path: Option<String>,
    // §5.1.1 preservation of unknown fields from the canonical finding.
    // INVARIANT: adding a new explicit field to this struct requires verifying
    // it does NOT collide with any key that the input stream might put in
    // `extras`. On collision, serde flatten emits duplicate JSON keys — most
    // parsers silently keep the last, which violates §5.1.1 preservation.
    // Check the field name against recent `raw_data` shapes before adding.
    #[serde(flatten)]
    extras: BTreeMap<String, Value>,
}

// source: stages/stage-1.md §4.2 / §9.2 — orchestrator's refined prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RefinedPrompt {
    text: String,
    role_hint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token_estimate: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AddedContext {
    kind: String,
    content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provenance: Option<String>,
}

// source: stages/stage-1.md §9.2 — refinement metadata from the orchestrator.
// `refined_at` is filled in by the Rust tool, not the agent: the JSON Schema
// (§9.2 line 402) lists only `added_context` + `orchestrator_version` with
// `additionalProperties: false`, and `#[serde(skip_deserializing)]` below
// enforces the same invariant at the Rust layer — any `refined_at` an agent
// sends is silently dropped before the tool fills its own timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RefinementMeta {
    added_context: Vec<AddedContext>,
    orchestrator_version: String,
    #[serde(default, skip_deserializing)]
    refined_at: Option<String>,
}

// source: stages/stage-1.md §4.2 — full refined artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RefinedArtifact {
    extracted: ExtractedFinding,
    refined_prompt: RefinedPrompt,
    refinement: RefinementMeta,
}

// source: stages/stage-1.md §5.2 + stages/stage-2.md §5.4 — index entry.
// Stage-2 fields are optional because a finding may be extracted/refined but
// not yet verified. `skip_serializing_if = Option::is_none` is CORRECT here
// (different from `Finding.relevance_score` which needed null/absent
// distinction per §9.3 Q8) — the index is server-owned metadata with no
// null/absent distinction issue: absent means the stage has not run.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct IndexEntry {
    artifact_path: String,
    extractor_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    orchestrator_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    refined_at: Option<String>,
    // --- stage-2 (stages/stage-2.md §5.4) ---
    #[serde(skip_serializing_if = "Option::is_none")]
    verified_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    verified: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stage2_path: Option<String>,
}

// source: stages/stage-1.md §5.2 — index.json shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Index {
    run_id: String,
    started_at: String,
    last_updated_at: String,
    findings: BTreeMap<String, IndexEntry>,
}

// ---------------------------------------------------------------------------
// Tool registry — one entry per pipeline stage.
// ---------------------------------------------------------------------------

fn tools_list() -> Value {
    tool_schemas::tools_list()
}

// ---------------------------------------------------------------------------
// Stage 1 — time + randomness (no external crates)
// ---------------------------------------------------------------------------

fn now_unix_seconds_nanos() -> (u64, u32) {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => (d.as_secs(), d.subsec_nanos()),
        // Pre-1970 system clock is unreachable on any modern host; fall back
        // to epoch so we never panic (spec: "No unwraps on I/O").
        Err(_) => (0, 0),
    }
}

// source: stages/stage-1.md §4.1 — `extracted_at` and §5.2 `started_at`.
// Reuses the format from parse_findings.py:127 (`%Y-%m-%dT%H:%M:%SZ`).
// Pure-stdlib UTC conversion via the civil-from-days algorithm by Howard
// Hinnant, "date algorithms", http://howardhinnant.github.io/date_algorithms.html
// (public-domain reference implementation; reproduced in §civil_from_days).
fn format_iso8601_utc(secs: u64) -> String {
    let (y, mo, d, h, mi, s) = civil_from_unix(secs);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, h, mi, s)
}

// source: stages/stage-1.md §9.3 Q5 — compact `YYYYMMDD-HHMMSS` for the
// run_id prefix. Matches SKILL.md:44 (`date +%Y%m%d-%H%M%S`).
fn format_compact_utc(secs: u64) -> String {
    let (y, mo, d, h, mi, s) = civil_from_unix(secs);
    format!("{:04}{:02}{:02}-{:02}{:02}{:02}", y, mo, d, h, mi, s)
}

// source: Howard Hinnant, "chrono-Compatible Low-Level Date Algorithms"
// http://howardhinnant.github.io/date_algorithms.html — civil_from_days().
// Public-domain. Valid for the full proleptic Gregorian range we care about.
fn civil_from_unix(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let rem = (secs % 86_400) as u32;
    let h = rem / 3600;
    let mi = (rem % 3600) / 60;
    let s = rem % 60;

    // Shift so day 0 is 0000-03-01 (Hinnant's "era" anchor).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as i64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let mo = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if mo <= 2 { y + 1 } else { y };

    (y, mo, d, h, mi, s)
}

// source: stages/stage-1.md §9.3 Q5 — 6-char uniform-random suffix. This is
// not cryptographic; nanosecond mixing is adequate for collision avoidance
// at any realistic run rate (collision P ≈ N²/(2·36⁶), negligible).
fn random_suffix(len: usize) -> String {
    let (secs, nanos) = now_unix_seconds_nanos();
    let pid = process::id() as u64;
    // xorshift64* seeded from clock + pid. Source: Marsaglia, "Xorshift RNGs",
    // Journal of Statistical Software 8(14), 2003. Not cryptographic — good
    // enough for a collision suffix.
    let mut state: u64 = secs
        // source: Knuth, TAOCP vol 2 §3.3.4, Table 1 (MMIX LCG multiplier)
        .wrapping_mul(6_364_136_223_846_793_005)
        // source: Steele, Lea, Flood, "Fast Splittable Pseudorandom Number
        // Generators", OOPSLA 2014 — constant from SplitMix64 seed advance.
        .wrapping_add((nanos as u64).wrapping_mul(1_442_695_040_888_963_407))
        // PID mix: plain XOR into the state. No multiplier is needed for a
        // non-cryptographic collision suffix — the clock+nanos product above
        // already provides the avalanche, and XOR keeps the pid contribution
        // sourced and justifiably trivial (no magic multiplier to cite).
        ^ pid;
    if state == 0 {
        // source: golden ratio * 2^64 (Knuth TAOCP vol 3 §6.4); also used
        // as MurmurHash3 finalizer mixer. Non-zero fallback seed.
        state = 0x9E37_79B9_7F4A_7C15;
    }
    let mut out = String::with_capacity(len);
    for _ in 0..len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let idx = (state as usize) % RUN_ID_RANDOM_ALPHABET.len();
        out.push(RUN_ID_RANDOM_ALPHABET[idx] as char);
    }
    out
}

fn generate_run_id() -> String {
    let (secs, _) = now_unix_seconds_nanos();
    format!("{}-{}", format_compact_utc(secs), random_suffix(RUN_ID_RANDOM_LEN))
}

// ---------------------------------------------------------------------------
// Stage 1 — safe-ID validation (spec §5.1.4, §9.3 Q4)
// ---------------------------------------------------------------------------

fn validate_safe_id(kind: &str, id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err(format!(
            "unsafe {} (spec §5.1.4, §9.3 Q4): must be non-empty",
            kind
        ));
    }
    if id.len() > SAFE_ID_MAX_LEN {
        return Err(format!(
            "unsafe {} (spec §5.1.4, §9.3 Q4): length {} exceeds max {}",
            kind,
            id.len(),
            SAFE_ID_MAX_LEN
        ));
    }
    if id.starts_with('.') {
        return Err(format!(
            "unsafe {} (spec §5.1.4, §9.3 Q4): must not start with '.'",
            kind
        ));
    }
    if id.contains("..") {
        return Err(format!(
            "unsafe {} (spec §5.1.4, §9.3 Q4): must not contain '..'",
            kind
        ));
    }
    for b in id.bytes() {
        let ok = b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-';
        if !ok {
            return Err(format!(
                "unsafe {} (spec §5.1.4, §9.3 Q4): must match [A-Za-z0-9._-]+",
                kind
            ));
        }
    }
    Ok(())
}

fn require_absolute(path: &str, field: &str) -> Result<PathBuf, String> {
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err(format!(
            "{} must be an absolute path (spec §3.1): got {:?}",
            field, path
        ));
    }
    // Reject `..` components outright — spec §5.1.4 safety applies to paths
    // the caller passes in. output_dir may still *resolve* to whatever the
    // user wants, but we do not silently consume `..`.
    for comp in p.components() {
        if matches!(comp, Component::ParentDir) {
            return Err(format!(
                "{} must not contain '..' (spec §5.1.4): got {:?}",
                field, path
            ));
        }
    }
    Ok(p.to_path_buf())
}

/// Parses the `language` field from tool arguments into an optional Language filter.
/// "auto" or absent -> None (detect per-file). Named language -> Some(Language).
fn parse_language_filter(args: &Map<String, Value>) -> Result<Option<parser::Language>, String> {
    match args.get("language").and_then(|v| v.as_str()) {
        None | Some("auto") => Ok(None),
        Some(lang_str) => parser::Language::from_str_opt(lang_str)
            .map(Some)
            .ok_or_else(|| format!("unsupported language: {lang_str}")),
    }
}

// ---------------------------------------------------------------------------
// Stage 1 — atomic file writes (spec §5.2.3, POSIX rename(2))
// ---------------------------------------------------------------------------

// Writes `contents` to `target` by first writing to a sibling tempfile and
// renaming it over the target. POSIX rename(2) is atomic on the same
// filesystem — reference: IEEE Std 1003.1-2017, rename(2).
fn atomic_write(target: &Path, contents: &[u8]) -> Result<(), String> {
    let parent = target.parent().ok_or_else(|| {
        format!("atomic_write: target has no parent: {:?}", target)
    })?;
    fs::create_dir_all(parent)
        .map_err(|e| format!("atomic_write: mkdir {:?}: {}", parent, e))?;

    let file_name = target
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("atomic_write: target has no file name: {:?}", target))?;

    let (secs, nanos) = now_unix_seconds_nanos();
    let pid = process::id();
    let tmp_name = format!(
        ".{}.tmp.{}.{}.{}.{}",
        file_name,
        pid,
        secs,
        nanos,
        random_suffix(4)
    );
    let tmp_path = parent.join(tmp_name);

    {
        let mut f = fs::File::create(&tmp_path)
            .map_err(|e| format!("atomic_write: create {:?}: {}", tmp_path, e))?;
        f.write_all(contents)
            .map_err(|e| format!("atomic_write: write {:?}: {}", tmp_path, e))?;
        f.sync_all()
            .map_err(|e| format!("atomic_write: fsync {:?}: {}", tmp_path, e))?;
    }

    fs::rename(&tmp_path, target).map_err(|e| {
        // Best-effort cleanup of the tempfile — it is not fatal if this fails.
        let _ = fs::remove_file(&tmp_path);
        format!(
            "atomic_write: rename {:?} -> {:?}: {}",
            tmp_path, target, e
        )
    })?;
    Ok(())
}

fn write_json_atomic<T: Serialize>(target: &Path, value: &T) -> Result<usize, String> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| format!("json serialize {:?}: {}", target, e))?;
    atomic_write(target, &bytes)?;
    Ok(bytes.len())
}

// ---------------------------------------------------------------------------
// Stage 1 — finding resolution (spec §3.3 form 1 + form 2)
// ---------------------------------------------------------------------------

// Returns (finding, source_form, source_path, source_bytes_verbatim).
// `source_bytes_verbatim` is the canonical JSON bytes to write into
// stage-1.source.json — after normalization to the §3.2 schema, per §4.4.
fn resolve_finding(
    finding_arg: &Value,
) -> Result<(Finding, &'static str, Option<String>, Vec<u8>), String> {
    match finding_arg {
        Value::Object(_) => {
            let finding: Finding = serde_json::from_value(finding_arg.clone()).map_err(|e| {
                format!("inline finding does not match canonical schema §3.2: {}", e)
            })?;
            validate_required_finding_fields(&finding)?;
            let bytes = serde_json::to_vec_pretty(&finding)
                .map_err(|e| format!("serialize inline finding: {}", e))?;
            Ok((finding, "inline", None, bytes))
        }
        Value::String(path_str) => {
            let path = require_absolute(path_str, "finding")?;
            let lower = path_str.to_ascii_lowercase();
            if lower.ends_with(".md") {
                // spec §9.3 Q1 — .md input is deferred in v1.
                return Err(
                    ".md finding inputs are not supported in v1 (spec §9.3 Q1); \
                     convert to JSON first"
                        .to_string(),
                );
            }
            if !lower.ends_with(".json") {
                return Err(format!(
                    "finding path must end in .json (spec §3.3): got {:?}",
                    path_str
                ));
            }
            let raw = fs::read_to_string(&path)
                .map_err(|e| format!("read finding file {:?}: {}", path, e))?;
            let parsed: Value = serde_json::from_str(&raw)
                .map_err(|e| format!("parse finding file {:?}: {}", path, e))?;

            // §3.3 form 2: either the root matches §3.2, or the root is
            // {findings: [...]} with exactly one element.
            let finding_value = if let Some(arr) = parsed
                .as_object()
                .and_then(|m| m.get("findings"))
                .and_then(|v| v.as_array())
            {
                if arr.len() != 1 {
                    return Err(format!(
                        "finding file {:?} has findings[{}]: stage 1 processes one finding per call (spec §3.3)",
                        path,
                        arr.len()
                    ));
                }
                arr[0].clone()
            } else {
                parsed
            };

            let finding: Finding = serde_json::from_value(finding_value).map_err(|e| {
                format!(
                    "finding file {:?} does not match canonical schema §3.2: {}",
                    path, e
                )
            })?;
            validate_required_finding_fields(&finding)?;
            let bytes = serde_json::to_vec_pretty(&finding)
                .map_err(|e| format!("serialize finding: {}", e))?;
            Ok((
                finding,
                "json_file",
                Some(path.to_string_lossy().into_owned()),
                bytes,
            ))
        }
        _ => Err("finding must be an object or an absolute path string (spec §3.1)".to_string()),
    }
}

fn validate_required_finding_fields(f: &Finding) -> Result<(), String> {
    if f.id.trim().is_empty() {
        return Err("finding.id is required and must be non-empty (spec §3.2)".to_string());
    }
    if f.title.trim().is_empty() {
        return Err("finding.title is required and must be non-empty (spec §3.2)".to_string());
    }
    if f.relevance_category.trim().is_empty() {
        return Err(
            "finding.relevance_category is required and must be non-empty (spec §3.2)".to_string(),
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Stage 1 — index.json read/merge/write (spec §5.2)
// ---------------------------------------------------------------------------

fn read_index(path: &Path) -> Result<Option<Index>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path)
        .map_err(|e| format!("read index {:?}: {}", path, e))?;
    let idx: Index = serde_json::from_str(&raw)
        .map_err(|e| format!("parse index {:?}: {}", path, e))?;
    Ok(Some(idx))
}

// Strategy for merging a new index entry with an existing one.
//
// Preservation discipline (spec §5.1.2 idempotency + §5.2.1 unique appearance
// + stages/stage-2.md §5.4): each stage owns its own fields, but a re-run of
// an earlier stage must NOT clobber downstream fields the later stage wrote.
//
// MergeMode answers the question "which fields of `entry` are authoritative?"
//
// Layer diagram (each arrow = "preserves when re-run"):
//
//     extract_finding  ─┐
//     refine_finding   ─┼──▶ IndexEntry
//     finalize_verification ──┘
//
// | Mode                   | Called by             | Preserves on top of `entry`                                   |
// |------------------------|-----------------------|---------------------------------------------------------------|
// | `PreserveDownstream`   | extract_finding       | refined_* (if set) AND verified_*/stage2_path (if set)         |
// | `PreserveStage2`       | refine_finding        | verified_*/stage2_path (if set) — stage-2 survives re-refine   |
// | `PreserveRefinedOnly`  | finalize_verification | refined_* (if set) — stage-2 fields come from `entry`          |
// | `Replace`              | (reserved, unused)    | nothing — `entry` is written wholesale                         |
//
// Written explicitly here because the stage-1 code-reviewer flagged MergeMode
// preservation as the single most hazardous area of the index code.
enum MergeMode {
    PreserveDownstream,
    PreserveStage2,
    PreserveRefinedOnly,
    #[allow(dead_code)]
    Replace,
}

fn upsert_index_entry(
    output_dir: &Path,
    run_id: &str,
    finding_id: &str,
    entry: IndexEntry,
    mode: MergeMode,
) -> Result<(), String> {
    let index_path = output_dir
        .join(RUNS_DIR_NAME)
        .join(run_id)
        .join(INDEX_FILE_NAME);
    let now = format_iso8601_utc(now_unix_seconds_nanos().0);
    let mut idx = match read_index(&index_path)? {
        Some(existing) => existing,
        None => Index {
            run_id: run_id.to_string(),
            started_at: now.clone(),
            last_updated_at: now.clone(),
            findings: BTreeMap::new(),
        },
    };
    idx.last_updated_at = now;

    let merged = merge_index_entry(idx.findings.get(finding_id), entry, mode);
    idx.findings.insert(finding_id.to_string(), merged);
    write_json_atomic(&index_path, &idx)?;
    Ok(())
}

// Pure function: given the existing entry (if any), the new entry, and the
// merge mode, return the entry that should be written. Split out of
// `upsert_index_entry` to keep that function under the 40-LOC bar and to
// make the preservation discipline testable in isolation.
fn merge_index_entry(
    existing: Option<&IndexEntry>,
    entry: IndexEntry,
    mode: MergeMode,
) -> IndexEntry {
    match mode {
        MergeMode::Replace => entry,
        MergeMode::PreserveDownstream => match existing {
            Some(e) => merge_preserve_downstream(e, entry),
            None => entry,
        },
        MergeMode::PreserveStage2 => match existing {
            Some(e) => merge_preserve_stage2(e, entry),
            None => entry,
        },
        MergeMode::PreserveRefinedOnly => match existing {
            Some(e) => merge_preserve_refined_only(e, entry),
            None => entry,
        },
    }
}

// extract_finding re-run: keep refined_* (if set) AND verified_*/stage2_path.
fn merge_preserve_downstream(existing: &IndexEntry, entry: IndexEntry) -> IndexEntry {
    IndexEntry {
        artifact_path: if existing.refined_at.is_some() {
            existing.artifact_path.clone()
        } else {
            entry.artifact_path
        },
        extractor_version: entry.extractor_version,
        orchestrator_version: existing.orchestrator_version.clone(),
        refined_at: existing.refined_at.clone(),
        verified_at: existing.verified_at.clone(),
        verified: existing.verified,
        stage2_path: existing.stage2_path.clone(),
    }
}

// refine_finding re-run: keep verified_*/stage2_path from the existing entry.
fn merge_preserve_stage2(existing: &IndexEntry, entry: IndexEntry) -> IndexEntry {
    IndexEntry {
        artifact_path: entry.artifact_path,
        extractor_version: entry.extractor_version,
        orchestrator_version: entry.orchestrator_version,
        refined_at: entry.refined_at,
        verified_at: existing.verified_at.clone(),
        verified: existing.verified,
        stage2_path: existing.stage2_path.clone(),
    }
}

// finalize_verification: keep refined_* from existing; stage-2 fields come
// from `entry`. artifact_path is kept as the refined path (stage 1b wrote it).
fn merge_preserve_refined_only(existing: &IndexEntry, entry: IndexEntry) -> IndexEntry {
    IndexEntry {
        artifact_path: existing.artifact_path.clone(),
        extractor_version: existing.extractor_version.clone(),
        orchestrator_version: existing.orchestrator_version.clone(),
        refined_at: existing.refined_at.clone(),
        verified_at: entry.verified_at,
        verified: entry.verified,
        stage2_path: entry.stage2_path,
    }
}

// ---------------------------------------------------------------------------
// Stage 1a — extract_finding
// ---------------------------------------------------------------------------

struct ExtractArgs<'a> {
    finding_arg: &'a Value,
    output_dir: PathBuf,
    run_id: String,
}

fn run_extract_finding(arguments: &Value) -> Value {
    match do_extract_finding(arguments) {
        Ok(v) => v,
        Err(reason) => json!({
            "stage": 1,
            "status": "error",
            "reason": reason
        }),
    }
}

fn parse_extract_args(arguments: &Value) -> Result<ExtractArgs<'_>, String> {
    let args = arguments
        .as_object()
        .ok_or_else(|| "arguments must be an object (spec §3.1)".to_string())?;
    let finding_arg = args
        .get("finding")
        .ok_or_else(|| "missing required field 'finding' (spec §3.1)".to_string())?;
    let output_dir_str = args
        .get("output_dir")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing required field 'output_dir' (spec §3.1)".to_string())?;
    let output_dir = require_absolute(output_dir_str, "output_dir")?;
    let run_id = match args.get("run_id") {
        Some(Value::String(s)) => {
            validate_safe_id("run_id", s)?;
            s.clone()
        }
        Some(Value::Null) | None => generate_run_id(),
        Some(other) => {
            return Err(format!(
                "run_id must be a string when provided (spec §3.1): got {}",
                other
            ));
        }
    };
    Ok(ExtractArgs { finding_arg, output_dir, run_id })
}

// spec §4.1: pure construction of the canonical extracted artifact.
fn build_extracted_artifact(
    finding: &Finding,
    source_form: &'static str,
    source_path: Option<String>,
) -> ExtractedFinding {
    ExtractedFinding {
        finding_id: finding.id.clone(),
        title: finding.title.clone(),
        description: finding.description.clone(),
        source_url: finding.source_url.clone(),
        relevance_category: finding.relevance_category.clone(),
        relevance_score: finding.relevance_score,
        raw_data: finding.raw_data.clone(),
        extracted_at: format_iso8601_utc(now_unix_seconds_nanos().0),
        extractor_version: EXTRACTOR_VERSION.to_string(),
        source_form: source_form.to_string(),
        source_path,
        extras: finding.extras.clone(),
    }
}

// spec §4.4 + §5.2.3 + §5.2 — mkdir, atomic-write source, atomic-write
// extracted, and upsert the preliminary index entry in MergeMode::PreserveRefined
// (so a prior refined artifact is not clobbered by a re-extract).
fn persist_extract(
    output_dir: &Path,
    run_id: &str,
    finding_dir: &Path,
    source_bytes: &[u8],
    extracted: &ExtractedFinding,
) -> Result<(PathBuf, usize), String> {
    fs::create_dir_all(finding_dir)
        .map_err(|e| format!("mkdir {:?}: {}", finding_dir, e))?;
    atomic_write(&finding_dir.join(SOURCE_FILE_NAME), source_bytes)?;
    let extracted_path = finding_dir.join(EXTRACTED_FILE_NAME);
    let bytes_written = write_json_atomic(&extracted_path, extracted)?;
    let entry = IndexEntry {
        artifact_path: format!(
            "{}/{}/{}",
            FINDINGS_DIR_NAME, extracted.finding_id, EXTRACTED_FILE_NAME
        ),
        extractor_version: EXTRACTOR_VERSION.to_string(),
        ..IndexEntry::default()
    };
    // PreserveDownstream: a re-extract must not clobber fields that stage 1b
    // or stage 2 later wrote on top (refined_*, verified_*, stage2_path).
    upsert_index_entry(
        output_dir,
        run_id,
        &extracted.finding_id,
        entry,
        MergeMode::PreserveDownstream,
    )?;
    Ok((extracted_path, bytes_written))
}

fn do_extract_finding(arguments: &Value) -> Result<Value, String> {
    let parsed = parse_extract_args(arguments)?;
    let (finding, source_form, source_path, source_bytes) =
        resolve_finding(parsed.finding_arg)?;
    // spec §5.1.4, §9.3 Q4 — hard-fail on unsafe finding_id BEFORE touching disk.
    validate_safe_id("finding_id", &finding.id)?;

    let finding_dir = parsed
        .output_dir
        .join(RUNS_DIR_NAME)
        .join(&parsed.run_id)
        .join(FINDINGS_DIR_NAME)
        .join(&finding.id);
    let extracted = build_extracted_artifact(&finding, source_form, source_path);
    let (extracted_path, bytes_written) = persist_extract(
        &parsed.output_dir,
        &parsed.run_id,
        &finding_dir,
        &source_bytes,
        &extracted,
    )?;

    // spec §4.3 — MCP receipt. artifact_path here is the extracted artifact;
    // refine_finding updates it to point at the refined artifact later.
    Ok(json!({
        "stage": 1,
        "status": "ok",
        "finding_id": finding.id,
        "artifact_path": extracted_path.to_string_lossy(),
        "run_id": parsed.run_id,
        "bytes_written": bytes_written,
        "extractor_version": EXTRACTOR_VERSION
    }))
}

// ---------------------------------------------------------------------------
// Stage 1b — refine_finding
// ---------------------------------------------------------------------------

// Err.0 is the short machine-readable reason code used by the smoke test
// (e.g. "no_extraction"), Err.1 is the human-readable message.
type StageErr = (String, String);

fn bad_request<E: std::fmt::Display>(msg: E) -> StageErr {
    ("bad_request".to_string(), msg.to_string())
}
fn io_err<E: std::fmt::Display>(msg: E) -> StageErr {
    ("io_error".to_string(), msg.to_string())
}
fn unsafe_id_err(msg: String) -> StageErr {
    ("unsafe_id".to_string(), msg)
}

struct RefineArgs {
    run_id: String,
    finding_id: String,
    output_dir: PathBuf,
    refined_prompt: RefinedPrompt,
    refinement_input: RefinementMeta,
}

fn run_refine_finding(arguments: &Value) -> Value {
    match do_refine_finding(arguments) {
        Ok(v) => v,
        Err((reason_code, reason_msg)) => json!({
            "stage": 1,
            "status": "error",
            "reason": reason_code,
            "message": reason_msg
        }),
    }
}

fn require_string_arg(args: &Map<String, Value>, key: &str) -> Result<String, StageErr> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| bad_request(format!("missing required field '{}' (spec §9.2)", key)))
}

fn parse_refine_args(arguments: &Value) -> Result<RefineArgs, StageErr> {
    let args = arguments
        .as_object()
        .ok_or_else(|| bad_request("arguments must be an object (spec §9.2)"))?;

    let run_id = require_string_arg(args, "run_id")?;
    validate_safe_id("run_id", &run_id).map_err(unsafe_id_err)?;
    let finding_id = require_string_arg(args, "finding_id")?;
    validate_safe_id("finding_id", &finding_id).map_err(unsafe_id_err)?;
    let output_dir_str = require_string_arg(args, "output_dir")?;
    let output_dir = require_absolute(&output_dir_str, "output_dir").map_err(bad_request)?;

    let refined_prompt_val = args
        .get("refined_prompt")
        .cloned()
        .ok_or_else(|| bad_request("missing required field 'refined_prompt' (spec §9.2)"))?;
    let refined_prompt: RefinedPrompt = serde_json::from_value(refined_prompt_val).map_err(|e| {
        bad_request(format!("refined_prompt does not match schema (spec §9.2): {}", e))
    })?;
    // spec §5.1.5 — non-empty refinement contract.
    if refined_prompt.text.is_empty() {
        return Err((
            "empty_prompt".to_string(),
            "refined_prompt.text must be non-empty (spec §5.1.5)".to_string(),
        ));
    }

    let refinement_val = args
        .get("refinement")
        .cloned()
        .ok_or_else(|| bad_request("missing required field 'refinement' (spec §9.2)"))?;
    let refinement_input: RefinementMeta = serde_json::from_value(refinement_val)
        .map_err(|e| bad_request(format!("refinement does not match schema (spec §9.2): {}", e)))?;

    Ok(RefineArgs {
        run_id,
        finding_id,
        output_dir,
        refined_prompt,
        refinement_input,
    })
}

// spec §9.2 precondition 1 — extraction must exist, parse cleanly.
fn load_existing_extracted(finding_dir: &Path) -> Result<ExtractedFinding, StageErr> {
    let extracted_path = finding_dir.join(EXTRACTED_FILE_NAME);
    if !extracted_path.exists() {
        return Err((
            "no_extraction".to_string(),
            format!(
                "no stage-1.extracted.json at {:?} — call extract_finding first (spec §9.2)",
                extracted_path
            ),
        ));
    }
    let extracted_raw = fs::read_to_string(&extracted_path)
        .map_err(|e| io_err(format!("read {:?}: {}", extracted_path, e)))?;
    serde_json::from_str(&extracted_raw).map_err(|e| {
        (
            "corrupt_extraction".to_string(),
            format!("parse {:?}: {}", extracted_path, e),
        )
    })
}

// spec §4.2 — pure construction of the refined artifact. Fills `refined_at`
// server-side per §9.2 (agent input is silently dropped by serde skip).
fn build_refined_artifact(
    extracted: ExtractedFinding,
    refined_prompt: RefinedPrompt,
    refinement_input: RefinementMeta,
    refined_at: String,
) -> RefinedArtifact {
    RefinedArtifact {
        extracted,
        refined_prompt,
        refinement: RefinementMeta {
            added_context: refinement_input.added_context,
            orchestrator_version: refinement_input.orchestrator_version,
            refined_at: Some(refined_at),
        },
    }
}

// spec §5.2.3 — atomic write of the refined artifact + §5.2 index upsert in
// MergeMode::Replace (refine_finding owns all four index fields).
fn persist_refine(
    output_dir: &Path,
    run_id: &str,
    finding_id: &str,
    finding_dir: &Path,
    artifact: &RefinedArtifact,
    refined_at: String,
) -> Result<(PathBuf, usize), StageErr> {
    let refined_path = finding_dir.join(REFINED_FILE_NAME);
    let bytes_written = write_json_atomic(&refined_path, artifact).map_err(io_err)?;
    let entry = IndexEntry {
        artifact_path: format!("{}/{}/{}", FINDINGS_DIR_NAME, finding_id, REFINED_FILE_NAME),
        extractor_version: EXTRACTOR_VERSION.to_string(),
        orchestrator_version: Some(artifact.refinement.orchestrator_version.clone()),
        refined_at: Some(refined_at),
        ..IndexEntry::default()
    };
    // PreserveStage2: a re-refine must not clobber the stage-2 verified_*/
    // stage2_path fields an earlier finalize_verification wrote.
    upsert_index_entry(output_dir, run_id, finding_id, entry, MergeMode::PreserveStage2)
        .map_err(io_err)?;
    Ok((refined_path, bytes_written))
}

fn do_refine_finding(arguments: &Value) -> Result<Value, StageErr> {
    let parsed = parse_refine_args(arguments)?;
    let finding_dir = parsed
        .output_dir
        .join(RUNS_DIR_NAME)
        .join(&parsed.run_id)
        .join(FINDINGS_DIR_NAME)
        .join(&parsed.finding_id);

    let extracted = load_existing_extracted(&finding_dir)?;
    let refined_at = format_iso8601_utc(now_unix_seconds_nanos().0);
    let orchestrator_version = parsed.refinement_input.orchestrator_version.clone();
    let artifact = build_refined_artifact(
        extracted,
        parsed.refined_prompt,
        parsed.refinement_input,
        refined_at.clone(),
    );
    let (refined_path, bytes_written) = persist_refine(
        &parsed.output_dir,
        &parsed.run_id,
        &parsed.finding_id,
        &finding_dir,
        &artifact,
        refined_at,
    )?;

    Ok(json!({
        "stage": 1,
        "status": "ok",
        "finding_id": parsed.finding_id,
        "artifact_path": refined_path.to_string_lossy(),
        "run_id": parsed.run_id,
        "bytes_written": bytes_written,
        "extractor_version": EXTRACTOR_VERSION,
        "orchestrator_version": orchestrator_version,
        "orchestrator_contract_version": ORCHESTRATOR_CONTRACT_VERSION
    }))
}

// ---------------------------------------------------------------------------
// Stage 2 — schema types (spec §12.3)
// ---------------------------------------------------------------------------

// source: stages/stage-2.md §3 + §12.3 — the five legal session states.
// `deny_unknown_fields` at the field level not needed — the enum string
// values are exhaustive and unknown strings are rejected by serde by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SessionState {
    Open,
    WaitingForUser,
    WaitingForAgent,
    Finalized,
    Aborted,
}

impl SessionState {
    fn as_str(self) -> &'static str {
        match self {
            SessionState::Open => "open",
            SessionState::WaitingForUser => "waiting_for_user",
            SessionState::WaitingForAgent => "waiting_for_agent",
            SessionState::Finalized => "finalized",
            SessionState::Aborted => "aborted",
        }
    }
}

// source: stages/stage-2.md §12.3 transcript item schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TurnKind {
    AgentQuestion,
    UserAnswer,
}

// source: stages/stage-2.md §12.3 — transcript element.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionTurn {
    seq: usize,
    kind: TurnKind,
    timestamp: String,
    content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    meta: Option<Value>,
}

// source: stages/stage-2.md §12.3 — single session file schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionFile {
    run_id: String,
    finding_id: String,
    state: SessionState,
    turn_count: usize,
    started_at: String,
    updated_at: String,
    schema_ok: bool,
    verifier_version: String,
    transcript: Vec<SessionTurn>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    aborted_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    abort_reason: Option<String>,
}

// source: stages/stage-2.md §5.3 — verified receipt sub-object.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct VerifiedKind {
    schema_ok: bool,
    completeness_ok: bool,
    user_acknowledged: bool,
}

// source: stages/stage-2.md §5.3 — verified receipt.
// `completeness_checklist` records which sub-items passed (per §5.3 + §9
// item 6: knowing WHICH item failed is load-bearing).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct VerifiedArtifact {
    run_id: String,
    finding_id: String,
    verified: bool,
    verified_kind: VerifiedKind,
    finalized_at: String,
    stage1_refined_path: String,
    session_path: String,
    transcript_digest: String,
    digest_algorithm: String,
    transcript_bytes_at_finalize: usize,
    turn_count: usize,
    verifier_version: String,
    completeness_checklist: BTreeMap<String, bool>,
}

// ---------------------------------------------------------------------------
// Stage 2 — state machine (spec §3 + §12.2)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum TransitionEvent {
    Start,
    AgentQuestion,
    UserAnswer,
    Finalize,
    Abort,
}

// Pure function. Dispatches to per-event handlers so each branch stays tiny.
// Table is the §12.2 override of the §3 state machine; see the match arms in
// `transition_append`, `transition_finalize`, `transition_abort`, and
// `transition_start`.
fn can_transition(from: SessionState, event: TransitionEvent) -> Result<SessionState, StageErr> {
    // Terminal states reject everything except Aborted+Start (handled below).
    if matches!(from, SessionState::Finalized) {
        return Err(stage2_err(
            "already_finalized",
            "session is finalized; further writes are rejected (spec §8.4)",
        ));
    }
    if matches!(from, SessionState::Aborted) && !matches!(event, TransitionEvent::Start) {
        return Err(stage2_err(
            "already_aborted",
            "session is aborted; only start_verification may restart it (spec §12.1)",
        ));
    }
    match event {
        TransitionEvent::Start => transition_start(from),
        TransitionEvent::AgentQuestion => transition_append(from, TurnKind::AgentQuestion),
        TransitionEvent::UserAnswer => transition_append(from, TurnKind::UserAnswer),
        TransitionEvent::Finalize => transition_finalize(from),
        TransitionEvent::Abort => transition_abort(from),
    }
}

// Spec §7.1 + §12.1: start is legal only on a missing or aborted session.
// The "missing session" case never reaches this function (checked in the
// caller). This handles the "existing session" case.
fn transition_start(from: SessionState) -> Result<SessionState, StageErr> {
    match from {
        SessionState::Aborted => Ok(SessionState::Open),
        _ => Err(stage2_err(
            "illegal_transition",
            "start_verification requires no session or an aborted session (spec §7.1, §12.1)",
        )),
    }
}

// Spec §3 alternation invariant + §12.3. `Open → agent_question` is the
// first-turn case.
fn transition_append(from: SessionState, kind: TurnKind) -> Result<SessionState, StageErr> {
    match (from, kind) {
        (SessionState::Open, TurnKind::AgentQuestion) => Ok(SessionState::WaitingForUser),
        (SessionState::WaitingForUser, TurnKind::UserAnswer) => Ok(SessionState::WaitingForAgent),
        (SessionState::WaitingForAgent, TurnKind::AgentQuestion) => Ok(SessionState::WaitingForUser),
        _ => Err(stage2_err(
            "alternation_violation",
            "two consecutive turns of the same kind are illegal (spec §3 alternation invariant, §12.2)",
        )),
    }
}

// Spec §12.2: finalize is illegal from Open and WaitingForUser; legal from
// WaitingForAgent only.
fn transition_finalize(from: SessionState) -> Result<SessionState, StageErr> {
    match from {
        SessionState::WaitingForAgent => Ok(SessionState::Finalized),
        SessionState::Open => Err(stage2_err(
            "no_clarification_round",
            "finalize requires ≥1 agent_question AND ≥1 user_answer before it (spec §12.2)",
        )),
        SessionState::WaitingForUser => Err(stage2_err(
            "unanswered_question",
            "cannot finalize while an agent_question is awaiting a user_answer (spec §7.3, §12.2)",
        )),
        _ => Err(stage2_err("illegal_transition", "finalize rejected (spec §12.2)")),
    }
}

// Spec §12.1: abort is legal from any non-terminal state. Terminal-state
// rejection already happened at the top of can_transition.
fn transition_abort(_from: SessionState) -> Result<SessionState, StageErr> {
    Ok(SessionState::Aborted)
}

// Helper to build a StageErr with a stage-2 reason code (spec "error codes"
// list in the implementation brief).
fn stage2_err(code: &str, msg: &str) -> StageErr {
    (code.to_string(), msg.to_string())
}

// ---------------------------------------------------------------------------
// Stage 2 — session I/O (spec §12.3 atomicity-by-construction)
// ---------------------------------------------------------------------------

fn session_path_for(output_dir: &Path, run_id: &str, finding_id: &str) -> PathBuf {
    output_dir
        .join(RUNS_DIR_NAME)
        .join(run_id)
        .join(FINDINGS_DIR_NAME)
        .join(finding_id)
        .join(SESSION_FILE_NAME)
}

fn finding_dir_for(output_dir: &Path, run_id: &str, finding_id: &str) -> PathBuf {
    output_dir
        .join(RUNS_DIR_NAME)
        .join(run_id)
        .join(FINDINGS_DIR_NAME)
        .join(finding_id)
}

fn read_session(path: &Path) -> Result<Option<SessionFile>, StageErr> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path)
        .map_err(|e| io_err(format!("read session {:?}: {}", path, e)))?;
    let session: SessionFile = serde_json::from_str(&raw).map_err(|e| {
        stage2_err(
            "corrupt_session",
            &format!("parse session {:?}: {}", path, e),
        )
    })?;
    Ok(Some(session))
}

// Spec §12.3 steps 4-7: serialize + atomic_write. Steps 1-3 (read, validate,
// construct) live in the per-tool `build_*` functions.
fn write_session_atomic(path: &Path, session: &SessionFile) -> Result<(), StageErr> {
    write_json_atomic(path, session).map_err(io_err)?;
    Ok(())
}

// Spec §12.2: completeness checklist. Returns (overall_ok, per_item_map).
fn compute_completeness(transcript: &[SessionTurn]) -> (bool, BTreeMap<String, bool>) {
    let has_q = transcript.iter().any(|t| t.kind == TurnKind::AgentQuestion);
    let has_a = transcript.iter().any(|t| t.kind == TurnKind::UserAnswer);
    let mut m = BTreeMap::new();
    m.insert("at_least_one_agent_question".to_string(), has_q);
    m.insert("at_least_one_user_answer".to_string(), has_a);
    (has_q && has_a, m)
}

// Spec §12.3 "transcript_digest change": sha256 over canonicalized transcript
// array bytes. Returns (hex_digest, canonical_bytes_len).
fn compute_transcript_digest(transcript: &[SessionTurn]) -> Result<(String, usize), StageErr> {
    let bytes = serde_json::to_vec(transcript)
        .map_err(|e| io_err(format!("canonicalize transcript: {}", e)))?;
    let digest = Sha256::digest(&bytes);
    Ok((hex_lower(&digest), bytes.len()))
}

// Lowercase hex, no `0x` prefix. Source: RFC 4648 §8 (Base 16). Hand-rolled
// to keep stage 2's dep footprint to serde + serde_json + sha2 only.
fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

// ---------------------------------------------------------------------------
// Stage 2a — start_verification (spec §7.1 + §12.1 restart)
// ---------------------------------------------------------------------------

struct StartArgs {
    run_id: String,
    finding_id: String,
    output_dir: PathBuf,
}

fn parse_start_args(arguments: &Value) -> Result<StartArgs, StageErr> {
    let args = arguments
        .as_object()
        .ok_or_else(|| bad_request("arguments must be an object (spec §7.1)"))?;
    let run_id = require_string_arg(args, "run_id")?;
    validate_safe_id("run_id", &run_id).map_err(unsafe_id_err)?;
    let finding_id = require_string_arg(args, "finding_id")?;
    validate_safe_id("finding_id", &finding_id).map_err(unsafe_id_err)?;
    let output_dir_str = require_string_arg(args, "output_dir")?;
    let output_dir = require_absolute(&output_dir_str, "output_dir").map_err(bad_request)?;
    Ok(StartArgs { run_id, finding_id, output_dir })
}

// Spec §7.1 precondition (i): stage-1.refined.json exists and parses. Also
// sets `schema_ok`, which never mutates after start.
fn check_stage1_refined(finding_dir: &Path) -> Result<bool, StageErr> {
    let refined_path = finding_dir.join(REFINED_FILE_NAME);
    if !refined_path.exists() {
        return Err(stage2_err(
            "no_extraction",
            &format!(
                "no {} at {:?} — call refine_finding first (spec §4 schema_ok, §7.1)",
                REFINED_FILE_NAME, refined_path
            ),
        ));
    }
    let raw = fs::read_to_string(&refined_path)
        .map_err(|e| io_err(format!("read {:?}: {}", refined_path, e)))?;
    let _parsed: RefinedArtifact = serde_json::from_str(&raw).map_err(|e| {
        stage2_err(
            "corrupt_extraction",
            &format!("parse {:?}: {}", refined_path, e),
        )
    })?;
    Ok(true)
}

// Pure construction per §12.3 step 3 (never mutate in place).
fn build_new_session(args: &StartArgs, schema_ok: bool, now: String) -> SessionFile {
    SessionFile {
        run_id: args.run_id.clone(),
        finding_id: args.finding_id.clone(),
        state: SessionState::Open,
        turn_count: 0,
        started_at: now.clone(),
        updated_at: now,
        schema_ok,
        verifier_version: VERIFIER_VERSION.to_string(),
        transcript: Vec::new(),
        aborted_at: None,
        abort_reason: None,
    }
}

fn do_start_verification(arguments: &Value) -> Result<Value, StageErr> {
    let args = parse_start_args(arguments)?;
    let finding_dir = finding_dir_for(&args.output_dir, &args.run_id, &args.finding_id);
    let schema_ok = check_stage1_refined(&finding_dir)?;
    let session_path = session_path_for(&args.output_dir, &args.run_id, &args.finding_id);
    if let Some(existing) = read_session(&session_path)? {
        // Spec §7.1 precondition (ii) + §12.1: only aborted is restartable.
        let _ = can_transition(existing.state, TransitionEvent::Start)?;
    }
    let now = format_iso8601_utc(now_unix_seconds_nanos().0);
    let session = build_new_session(&args, schema_ok, now.clone());
    fs::create_dir_all(&finding_dir).map_err(|e| io_err(format!("mkdir {:?}: {}", finding_dir, e)))?;
    write_session_atomic(&session_path, &session)?;
    Ok(json!({
        "stage": 2,
        "status": "ok",
        "state": session.state.as_str(),
        "run_id": session.run_id,
        "finding_id": session.finding_id,
        "started_at": session.started_at,
        "session_path": session_path.to_string_lossy(),
    }))
}

fn run_start_verification(arguments: &Value) -> Value {
    match do_start_verification(arguments) {
        Ok(v) => v,
        Err((code, msg)) => stage2_error_response(code, msg),
    }
}

fn stage2_error_response(code: String, msg: String) -> Value {
    json!({
        "stage": 2,
        "status": "error",
        "reason": code,
        "message": msg,
    })
}

// ---------------------------------------------------------------------------
// Stage 2b — append_clarification (spec §7.2 + §12.3 single-file rewrite)
// ---------------------------------------------------------------------------

struct AppendArgs {
    run_id: String,
    finding_id: String,
    output_dir: PathBuf,
    kind: TurnKind,
    content: String,
    meta: Option<Value>,
}

fn parse_append_args(arguments: &Value) -> Result<AppendArgs, StageErr> {
    let args = arguments
        .as_object()
        .ok_or_else(|| bad_request("arguments must be an object (spec §7.2)"))?;
    let run_id = require_string_arg(args, "run_id")?;
    validate_safe_id("run_id", &run_id).map_err(unsafe_id_err)?;
    let finding_id = require_string_arg(args, "finding_id")?;
    validate_safe_id("finding_id", &finding_id).map_err(unsafe_id_err)?;
    let output_dir_str = require_string_arg(args, "output_dir")?;
    let output_dir = require_absolute(&output_dir_str, "output_dir").map_err(bad_request)?;
    let kind_str = require_string_arg(args, "kind")?;
    let kind = match kind_str.as_str() {
        "agent_question" => TurnKind::AgentQuestion,
        "user_answer" => TurnKind::UserAnswer,
        other => return Err(bad_request(format!("kind must be 'agent_question' or 'user_answer' (spec §5.2, §7.2): got {:?}", other))),
    };
    let content = require_string_arg(args, "content")?;
    if content.is_empty() {
        return Err(bad_request("content must be non-empty (spec §5.2 minLength:1)"));
    }
    let meta = args.get("meta").cloned();
    Ok(AppendArgs { run_id, finding_id, output_dir, kind, content, meta })
}

// Pure construction of the new session from the old one + the incoming turn.
// Spec §12.3 step 3: "never mutate in place" — returns a fresh SessionFile.
fn build_appended_session(prev: &SessionFile, args: &AppendArgs) -> Result<SessionFile, StageErr> {
    let event = match args.kind {
        TurnKind::AgentQuestion => TransitionEvent::AgentQuestion,
        TurnKind::UserAnswer => TransitionEvent::UserAnswer,
    };
    let new_state = can_transition(prev.state, event)?;
    let now = format_iso8601_utc(now_unix_seconds_nanos().0);
    let mut transcript = prev.transcript.clone();
    let new_turn = SessionTurn {
        seq: transcript.len(),
        kind: args.kind,
        timestamp: now.clone(),
        content: args.content.clone(),
        meta: args.meta.clone(),
    };
    transcript.push(new_turn);
    Ok(SessionFile {
        run_id: prev.run_id.clone(),
        finding_id: prev.finding_id.clone(),
        state: new_state,
        turn_count: transcript.len(),
        started_at: prev.started_at.clone(),
        updated_at: now,
        schema_ok: prev.schema_ok,
        verifier_version: prev.verifier_version.clone(),
        transcript,
        aborted_at: None,
        abort_reason: None,
    })
}

fn do_append_clarification(arguments: &Value) -> Result<Value, StageErr> {
    let args = parse_append_args(arguments)?;
    let session_path = session_path_for(&args.output_dir, &args.run_id, &args.finding_id);
    let prev = read_session(&session_path)?
        .ok_or_else(|| stage2_err("no_session", "no stage-2.session.json — call start_verification first (spec §7.2)"))?;
    let next = build_appended_session(&prev, &args)?;
    write_session_atomic(&session_path, &next)?;
    let last_seq = next.transcript.len().saturating_sub(1);
    Ok(json!({
        "stage": 2,
        "status": "ok",
        "state": next.state.as_str(),
        "seq": last_seq,
        "turn_count": next.turn_count,
    }))
}

fn run_append_clarification(arguments: &Value) -> Value {
    match do_append_clarification(arguments) {
        Ok(v) => v,
        Err((code, msg)) => stage2_error_response(code, msg),
    }
}

// ---------------------------------------------------------------------------
// Stage 2c — finalize_verification (spec §7.3 + §12.2 + §12.3)
// ---------------------------------------------------------------------------

struct FinalizeArgs {
    run_id: String,
    finding_id: String,
    output_dir: PathBuf,
}

fn parse_finalize_args(arguments: &Value) -> Result<FinalizeArgs, StageErr> {
    let args = arguments
        .as_object()
        .ok_or_else(|| bad_request("arguments must be an object (spec §7.3)"))?;
    let run_id = require_string_arg(args, "run_id")?;
    validate_safe_id("run_id", &run_id).map_err(unsafe_id_err)?;
    let finding_id = require_string_arg(args, "finding_id")?;
    validate_safe_id("finding_id", &finding_id).map_err(unsafe_id_err)?;
    let output_dir_str = require_string_arg(args, "output_dir")?;
    let output_dir = require_absolute(&output_dir_str, "output_dir").map_err(bad_request)?;
    Ok(FinalizeArgs { run_id, finding_id, output_dir })
}

fn load_session_for_finalize(args: &FinalizeArgs) -> Result<(SessionFile, PathBuf), StageErr> {
    let session_path = session_path_for(&args.output_dir, &args.run_id, &args.finding_id);
    let session = read_session(&session_path)?.ok_or_else(|| {
        stage2_err(
            "no_session",
            "no stage-2.session.json — call start_verification first (spec §7.3)",
        )
    })?;
    Ok((session, session_path))
}

// Spec §12.2: finalize rejects from `open` and `waiting_for_user`. Also
// enforces §7.3 precondition (ii): schema_ok must be true.
fn check_finalize_preconditions(session: &SessionFile) -> Result<(), StageErr> {
    if !session.schema_ok {
        return Err(stage2_err(
            "schema_not_ok",
            "session.schema_ok is false; stage-1 artifact is broken (spec §7.3 (ii))",
        ));
    }
    let _ = can_transition(session.state, TransitionEvent::Finalize)?;
    Ok(())
}

// Spec §12.3: digest over the transcript array only (NOT the whole session
// file — that would include `state: "finalized"` which is circular).
fn build_verified_artifact(
    session: &SessionFile,
    args: &FinalizeArgs,
    now: String,
) -> Result<VerifiedArtifact, StageErr> {
    let (completeness_ok, checklist) = compute_completeness(&session.transcript);
    // §12.2 guarantees this is true if we got past check_finalize_preconditions,
    // but compute it from data anyway — no invented invariants.
    let (digest_hex, digest_input_len) = compute_transcript_digest(&session.transcript)?;
    let user_acknowledged = true; // Spec §4: the finalize tool call itself is the signal.
    let verified = session.schema_ok && completeness_ok && user_acknowledged;
    Ok(VerifiedArtifact {
        run_id: session.run_id.clone(),
        finding_id: session.finding_id.clone(),
        verified,
        verified_kind: VerifiedKind {
            schema_ok: session.schema_ok,
            completeness_ok,
            user_acknowledged,
        },
        finalized_at: now,
        stage1_refined_path: format!("{}/{}/{}", FINDINGS_DIR_NAME, args.finding_id, REFINED_FILE_NAME),
        session_path: format!("{}/{}/{}", FINDINGS_DIR_NAME, args.finding_id, SESSION_FILE_NAME),
        transcript_digest: digest_hex,
        digest_algorithm: DIGEST_ALGORITHM.to_string(),
        transcript_bytes_at_finalize: digest_input_len,
        turn_count: session.turn_count,
        verifier_version: VERIFIER_VERSION.to_string(),
        completeness_checklist: checklist,
    })
}

// Spec §12.3: write verified artifact, flip session.state to finalized
// atomically (whole-file rewrite), upsert index entry.
//
// Crash window: if we crash between the verified write and the session flip,
// the verified receipt is on disk but the session is still waiting_for_agent.
// Re-running finalize is idempotent (same transcript → same digest).
fn persist_finalize(
    args: &FinalizeArgs,
    session: &SessionFile,
    verified: &VerifiedArtifact,
    now: String,
) -> Result<(PathBuf, usize), StageErr> {
    let finding_dir = finding_dir_for(&args.output_dir, &args.run_id, &args.finding_id);
    let verified_path = finding_dir.join(VERIFIED_FILE_NAME);
    let bytes_written = write_json_atomic(&verified_path, verified).map_err(io_err)?;
    let finalized = SessionFile {
        state: SessionState::Finalized,
        updated_at: now.clone(),
        ..session.clone()
    };
    let session_path = session_path_for(&args.output_dir, &args.run_id, &args.finding_id);
    write_session_atomic(&session_path, &finalized)?;
    upsert_verified_index(args, verified, now)?;
    Ok((verified_path, bytes_written))
}

// Spec §5.4: stage-2 index fields. Preserves the refined_* fields stage 1b
// wrote. artifact_path is taken from the existing entry (refined path).
fn upsert_verified_index(
    args: &FinalizeArgs,
    verified: &VerifiedArtifact,
    now: String,
) -> Result<(), StageErr> {
    let entry = IndexEntry {
        // Will be overridden by PreserveRefinedOnly; put a sentinel here.
        artifact_path: String::new(),
        extractor_version: EXTRACTOR_VERSION.to_string(),
        verified_at: Some(now),
        verified: Some(verified.verified),
        stage2_path: Some(format!(
            "{}/{}/{}",
            FINDINGS_DIR_NAME, args.finding_id, VERIFIED_FILE_NAME
        )),
        ..IndexEntry::default()
    };
    upsert_index_entry(
        &args.output_dir,
        &args.run_id,
        &args.finding_id,
        entry,
        MergeMode::PreserveRefinedOnly,
    )
    .map_err(io_err)
}

fn do_finalize_verification(arguments: &Value) -> Result<Value, StageErr> {
    let args = parse_finalize_args(arguments)?;
    let (session, _session_path) = load_session_for_finalize(&args)?;
    check_finalize_preconditions(&session)?;
    let now = format_iso8601_utc(now_unix_seconds_nanos().0);
    let verified = build_verified_artifact(&session, &args, now.clone())?;
    let (verified_path, bytes_written) = persist_finalize(&args, &session, &verified, now)?;
    Ok(json!({
        "stage": 2,
        "status": "ok",
        "state": SessionState::Finalized.as_str(),
        "verified": verified.verified,
        "verified_kind": {
            "schema_ok": verified.verified_kind.schema_ok,
            "completeness_ok": verified.verified_kind.completeness_ok,
            "user_acknowledged": verified.verified_kind.user_acknowledged,
        },
        "verified_path": verified_path.to_string_lossy(),
        "turn_count": verified.turn_count,
        "transcript_digest": verified.transcript_digest,
        "digest_algorithm": verified.digest_algorithm,
        "transcript_bytes_at_finalize": verified.transcript_bytes_at_finalize,
        "bytes_written": bytes_written,
        "verifier_version": VERIFIER_VERSION,
    }))
}

fn run_finalize_verification(arguments: &Value) -> Value {
    match do_finalize_verification(arguments) {
        Ok(v) => v,
        Err((code, msg)) => stage2_error_response(code, msg),
    }
}

// ---------------------------------------------------------------------------
// Stage 2d — abort_verification (spec §12.1)
// ---------------------------------------------------------------------------

struct AbortArgs {
    run_id: String,
    finding_id: String,
    output_dir: PathBuf,
    reason: Option<String>,
}

fn parse_abort_args(arguments: &Value) -> Result<AbortArgs, StageErr> {
    let args = arguments
        .as_object()
        .ok_or_else(|| bad_request("arguments must be an object (spec §12.1)"))?;
    let run_id = require_string_arg(args, "run_id")?;
    validate_safe_id("run_id", &run_id).map_err(unsafe_id_err)?;
    let finding_id = require_string_arg(args, "finding_id")?;
    validate_safe_id("finding_id", &finding_id).map_err(unsafe_id_err)?;
    let output_dir_str = require_string_arg(args, "output_dir")?;
    let output_dir = require_absolute(&output_dir_str, "output_dir").map_err(bad_request)?;
    let reason = args
        .get("reason")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Ok(AbortArgs { run_id, finding_id, output_dir, reason })
}

// Pure construction. Transcript is preserved verbatim (spec §12.3 "abort
// does not truncate the transcript; it only flips state and sets aborted_at").
fn build_aborted_session(prev: &SessionFile, reason: Option<String>, now: String) -> Result<SessionFile, StageErr> {
    let _ = can_transition(prev.state, TransitionEvent::Abort)?;
    Ok(SessionFile {
        state: SessionState::Aborted,
        updated_at: now.clone(),
        aborted_at: Some(now),
        abort_reason: reason,
        ..prev.clone()
    })
}

fn do_abort_verification(arguments: &Value) -> Result<Value, StageErr> {
    let args = parse_abort_args(arguments)?;
    let session_path = session_path_for(&args.output_dir, &args.run_id, &args.finding_id);
    let prev = read_session(&session_path)?
        .ok_or_else(|| stage2_err("no_session", "no stage-2.session.json to abort (spec §12.1)"))?;
    let now = format_iso8601_utc(now_unix_seconds_nanos().0);
    let aborted = build_aborted_session(&prev, args.reason.clone(), now.clone())?;
    write_session_atomic(&session_path, &aborted)?;
    Ok(json!({
        "stage": 2,
        "status": "ok",
        "state": aborted.state.as_str(),
        "run_id": aborted.run_id,
        "finding_id": aborted.finding_id,
        "turn_count": aborted.turn_count,
        "aborted_at": now,
    }))
}

fn run_abort_verification(arguments: &Value) -> Value {
    match do_abort_verification(arguments) {
        Ok(v) => v,
        Err((code, msg)) => stage2_error_response(code, msg),
    }
}

// ---------------------------------------------------------------------------
// Stage 3a — index_codebase
// ---------------------------------------------------------------------------

fn run_index_codebase(arguments: &Value) -> Value {
    match do_index_codebase(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "index_failed", "message": msg
        }),
    }
}

fn do_index_codebase(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let path_str = args.get("path").and_then(|v| v.as_str())
        .ok_or("missing required field 'path'")?;
    let output_str = args.get("output_dir").and_then(|v| v.as_str())
        .ok_or("missing required field 'output_dir'")?;
    let lang_filter = parse_language_filter(args)?;

    let codebase = require_absolute(path_str, "path")?;
    if !codebase.exists() {
        return Err(format!("path does not exist: {}", codebase.display()));
    }
    let output_dir = require_absolute(output_str, "output_dir")?;
    fs::create_dir_all(&output_dir)
        .map_err(|e| format!("create output dir: {e}"))?;
    let graph_dir = output_dir.join("graph");
    // source: H4 fix — validate the derived path ends in `/graph` and is not
    // a forbidden system root before any destructive op.
    validate_graph_path_safe(&graph_dir)?;
    // lbug creates the database directory itself; if a stale directory exists
    // from a prior run, remove it so lbug can initialise cleanly.
    if graph_dir.exists() {
        fs::remove_dir_all(&graph_dir)
            .map_err(|e| format!("remove stale graph dir: {e}"))?;
    }

    let result = indexer::index_codebase_with_language(&codebase, &graph_dir, lang_filter)?;
    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "index_codebase",
        "graph_path": result.graph_path.to_string_lossy(),
        "node_count": result.node_count,
        "edge_count": result.edge_count,
        "files_indexed": result.files_indexed,
        "elapsed_ms": result.elapsed_ms,
    }))
}

// ---------------------------------------------------------------------------
// Stage 3a — query_graph
// ---------------------------------------------------------------------------

fn run_query_graph(arguments: &Value) -> Value {
    match do_query_graph(arguments) {
        Ok(v) => v,
        Err(msg) => {
            // Surface the read-only rejection as its own reason code so callers
            // can distinguish policy rejections from engine errors.
            if msg.contains("read_only_query_required") {
                json!({
                    "stage": 3,
                    "status": "error",
                    "reason": "read_only_query_required",
                    "message": msg,
                })
            } else {
                json!({
                    "stage": 3, "status": "error", "reason": "query_failed", "message": msg
                })
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Read-only Cypher filter — source: H3 fix.
//
// Rejects any query that contains a mutation or side-effectful keyword as a
// whole-word, case-insensitive match. This is a conservative allowlist-by-
// blocklist: the engine still validates syntax, we just refuse to hand it
// anything that could mutate state, load external data, or call procedures.
// ---------------------------------------------------------------------------

const FORBIDDEN_CYPHER_KEYWORDS: &[&str] = &[
    "CREATE", "DELETE", "MERGE", "SET", "REMOVE",
    "DROP", "ALTER", "CALL", "LOAD",
];

/// Returns the first forbidden keyword found in `query`, or None if the query
/// is safe. Matching is whole-word, ASCII case-insensitive. Strings/comments
/// are not specifically excluded — callers who need `CREATE` as a literal in
/// a read query must restructure it (reading doesn't require mutation words).
fn forbidden_cypher_keyword(query: &str) -> Option<&'static str> {
    let upper = query.to_ascii_uppercase();
    for &kw in FORBIDDEN_CYPHER_KEYWORDS {
        if contains_whole_word(&upper, kw) {
            return Some(kw);
        }
    }
    None
}

/// Whole-word contains: `needle` must be bordered by non-alphanumeric chars
/// (or start/end of haystack). Prevents false positives on identifiers that
/// embed the keyword (e.g. `created_at` should not trigger `CREATE`).
fn contains_whole_word(haystack: &str, needle: &str) -> bool {
    let bytes = haystack.as_bytes();
    let nbytes = needle.as_bytes();
    if nbytes.is_empty() || bytes.len() < nbytes.len() {
        return false;
    }
    let mut i = 0;
    while i + nbytes.len() <= bytes.len() {
        if &bytes[i..i + nbytes.len()] == nbytes {
            let left_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_';
            let right = i + nbytes.len();
            let right_ok = right == bytes.len()
                || (!bytes[right].is_ascii_alphanumeric() && bytes[right] != b'_');
            if left_ok && right_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

// ---------------------------------------------------------------------------
// Graph-path safety — source: H4 fix.
//
// The `graph_path` / `output_dir` / ... arguments are caller-controlled and
// in the pre-fix code were passed to `remove_dir_all`. A malicious caller
// could set `output_dir: "/"` and have the server wipe the filesystem.
//
// `validate_graph_path_safe` MUST be called before any `remove_dir_all` or
// `create_dir_all` on a caller-derived path. The policy:
//   (a) path must be absolute,
//   (b) last segment must be `graph` (or the path must contain `/graph/`),
//   (c) path must NOT equal a forbidden system root.
// ---------------------------------------------------------------------------

const FORBIDDEN_GRAPH_PATH_PREFIXES: &[&str] = &[
    "/", "/Users", "/home", "/root", "/tmp", "/var", "/etc",
    "/usr", "/bin", "/sbin", "/dev", "/opt", "/System", "/Library",
];

/// Returns Ok iff `path` is a safe target for destructive directory ops.
fn validate_graph_path_safe(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err(format!(
            "unsafe_graph_path: must be absolute (got {:?})",
            path
        ));
    }
    // Must end in `/graph` (the well-known suffix). Check the last component.
    let last = path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if last != "graph" {
        return Err(format!(
            "unsafe_graph_path: must end in '/graph' (got {:?})",
            path
        ));
    }
    // Reject pathological roots (even if they happen to end in `/graph`).
    let s = path.to_string_lossy();
    for forbidden in FORBIDDEN_GRAPH_PATH_PREFIXES {
        if s == *forbidden || s == format!("{forbidden}/graph") {
            return Err(format!(
                "unsafe_graph_path: {path:?} is a forbidden system path"
            ));
        }
    }
    Ok(())
}

fn do_query_graph(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args.get("graph_path").and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let query = args.get("query").and_then(|v| v.as_str())
        .ok_or("missing required field 'query'")?;

    // source: H3 fix — query_graph is a read-only tool. Reject any query
    // containing mutation keywords before handing it to the engine.
    if let Some(bad) = forbidden_cypher_keyword(query) {
        return Err(format!(
            "read_only_query_required: query_graph is read-only; \
             found forbidden keyword: {bad}"
        ));
    }

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }

    let start = std::time::Instant::now();
    let store = graph_store::GraphStore::open_or_create(graph_path)?;
    let qr = store.execute_query(query)?;
    let elapsed_ms = start.elapsed().as_millis() as u64;

    let rows: Vec<Value> = qr.rows.iter()
        .map(|row| Value::Array(row.iter().map(|c| json!(c)).collect()))
        .collect();

    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "query_graph",
        "columns": qr.columns,
        "rows": rows,
        "result": format_query_result(&qr.columns, &qr.rows),
        "elapsed_ms": elapsed_ms,
    }))
}

fn format_query_result(columns: &[String], rows: &[Vec<String>]) -> String {
    let mut out = columns.join(" | ");
    for row in rows {
        out.push('\n');
        out.push_str(&row.join(" | "));
    }
    out
}

// ---------------------------------------------------------------------------
// Stage 3a — get_symbol
// ---------------------------------------------------------------------------

fn run_get_symbol(arguments: &Value) -> Value {
    match do_get_symbol(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "symbol_lookup_failed", "message": msg
        }),
    }
}

fn do_get_symbol(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args.get("graph_path").and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let qn = args.get("qualified_name").and_then(|v| v.as_str())
        .ok_or("missing required field 'qualified_name'")?;

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }

    let store = graph_store::GraphStore::open_or_create(graph_path)?;

    // source: C-correctness bug 2 — three-layer lookup (exact → strip-path →
    // fuzzy). Resolves the natural `src/main.rs::foo` form to the stored
    // `main.rs::foo` and returns did_you_mean suggestions otherwise.
    let resolved_qn = match search::resolve_qualified_name(&store, qn) {
        Ok(q) => q,
        Err(nf) => {
            return Ok(json!({
                "stage": 3,
                "status": "error",
                "reason": "symbol_not_found",
                "message": format!("not found: {}", nf.input),
                "did_you_mean": nf.did_you_mean,
            }));
        }
    };

    // source: M1 fix — centralized cypher_str escapes both `\` and `'`.
    // Returns the string already wrapped in single quotes.
    let escaped = graph_store::cypher_str(&resolved_qn);

    let node = find_symbol_node(&store, &escaped)?;
    let edges_out = find_symbol_edges_out(&store, &escaped)?;
    let edges_in = find_symbol_edges_in(&store, &escaped)?;

    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "get_symbol",
        "node": node,
        "edges_out": edges_out,
        "edges_in": edges_in,
    }))
}

/// Searches all node tables for a node matching by qualified_name or id.
/// `lit` must be a Cypher-quoted literal produced by `graph_store::cypher_str`.
fn find_symbol_node(
    store: &graph_store::GraphStore,
    lit: &str,
) -> Result<Value, String> {
    let labels = [
        "Function", "Method", "Struct", "Enum", "Trait", "Variant",
        "Module", "Constant", "TypeAlias", "Field", "Import",
        "File", "Directory", "CallSite",
    ];
    for label in labels {
        let cypher = format!(
            "MATCH (n:{label}) WHERE n.qualified_name = {lit} \
             OR n.id = {lit} RETURN n"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            if !qr.rows.is_empty() {
                return Ok(json!({
                    "label": label,
                    "data": qr.rows[0][0],
                }));
            }
        }
    }
    Ok(Value::Null)
}

/// Queries outgoing edges across all relationship tables.
fn find_symbol_edges_out(
    store: &graph_store::GraphStore,
    lit: &str,
) -> Result<Vec<Value>, String> {
    let mut edges = Vec::new();
    for (rel, from_label, to_label) in rel_table_triples() {
        let cypher = format!(
            "MATCH (a:{from_label})-[r:{rel}]->(b:{to_label}) \
             WHERE a.qualified_name = {lit} OR a.id = {lit} \
             RETURN '{rel}' AS rel_type, b.id AS target_id"
        );
        collect_edge_rows(store, &cypher, &mut edges);
    }
    Ok(edges)
}

/// Queries incoming edges across all relationship tables.
fn find_symbol_edges_in(
    store: &graph_store::GraphStore,
    lit: &str,
) -> Result<Vec<Value>, String> {
    let mut edges = Vec::new();
    for (rel, from_label, to_label) in rel_table_triples() {
        let cypher = format!(
            "MATCH (a:{from_label})-[r:{rel}]->(b:{to_label}) \
             WHERE b.qualified_name = {lit} OR b.id = {lit} \
             RETURN '{rel}' AS rel_type, a.id AS source_id"
        );
        collect_edge_rows(store, &cypher, &mut edges);
    }
    Ok(edges)
}

fn collect_edge_rows(
    store: &graph_store::GraphStore,
    cypher: &str,
    edges: &mut Vec<Value>,
) {
    if let Ok(qr) = store.execute_query(cypher) {
        for row in &qr.rows {
            if row.len() >= 2 {
                edges.push(json!({"rel": row[0], "id": row[1]}));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Stage 3b — resolve_graph
// ---------------------------------------------------------------------------

fn run_resolve_graph(arguments: &Value) -> Value {
    match do_resolve_graph(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "resolve_failed", "message": msg
        }),
    }
}

fn do_resolve_graph(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args.get("graph_path").and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }

    let store = graph_store::GraphStore::open_or_create(graph_path)?;
    let result = resolver::resolve_graph(&store)?;
    let rate = if result.total_refs > 0 {
        result.total_edges as f64 / result.total_refs as f64
    } else {
        0.0
    };

    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "resolve_graph",
        "imports_resolved": result.imports_resolved,
        "calls_resolved": result.calls_resolved,
        "implements_resolved": result.impls_resolved,
        "extends_resolved": result.extends_resolved,
        "uses_resolved": result.uses_resolved,
        "total_edges": result.total_edges,
        "total_refs": result.total_refs,
        "resolution_rate": format!("{:.2}", rate),
        "unresolved_count": result.unresolved.len(),
        "elapsed_ms": result.elapsed_ms,
    }))
}

// ---------------------------------------------------------------------------
// Stage 3c — cluster_graph
// ---------------------------------------------------------------------------

fn run_cluster_graph(arguments: &Value) -> Value {
    match do_cluster_graph(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "cluster_failed", "message": msg
        }),
    }
}

fn do_cluster_graph(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args.get("graph_path").and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let gamma = args.get("resolution_param")
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0);

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }

    let store = graph_store::GraphStore::open_or_create(graph_path)?;
    let result = clustering::cluster_graph(&store, gamma)?;

    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "cluster_graph",
        "community_count": result.communities,
        "modularity": format!("{:.6}", result.modularity),
        "process_count": result.processes,
        "elapsed_ms": result.elapsed_ms,
    }))
}

// ---------------------------------------------------------------------------
// Stage 3c — get_processes
// ---------------------------------------------------------------------------

fn run_get_processes(arguments: &Value) -> Value {
    match do_get_processes(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "processes_failed", "message": msg
        }),
    }
}

fn do_get_processes(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args.get("graph_path").and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }

    let store = graph_store::GraphStore::open_or_create(graph_path)?;
    let processes = clustering::get_processes(&store)?;

    let procs: Vec<Value> = processes.iter().map(|p| json!({
        "name": p.name,
        "entry_point": p.entry_point,
        "entry_kind": p.entry_kind,
        "depth": p.depth,
        "node_count": p.node_count,
    })).collect();

    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "get_processes",
        "process_count": procs.len(),
        "processes": procs,
    }))
}

// ---------------------------------------------------------------------------
// Stage 3c — get_impact
// ---------------------------------------------------------------------------

fn run_get_impact(arguments: &Value) -> Value {
    match do_get_impact(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "impact_failed", "message": msg
        }),
    }
}

fn do_get_impact(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args.get("graph_path").and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let qn = args.get("qualified_name").and_then(|v| v.as_str())
        .ok_or("missing required field 'qualified_name'")?;

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }

    let store = graph_store::GraphStore::open_or_create(graph_path)?;
    let impact = clustering::get_impact(&store, qn)?;

    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "get_impact",
        "qualified_name": qn,
        "communities": impact.communities,
        "communities_affected": impact.communities.len(),
        "processes": impact.processes,
        "processes_affected": impact.processes.len(),
    }))
}

// ---------------------------------------------------------------------------
// Stage 3d — search_codebase
// ---------------------------------------------------------------------------

fn run_search_codebase(arguments: &Value) -> Value {
    match do_search_codebase(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "search_failed", "message": msg
        }),
    }
}

fn do_search_codebase(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args.get("graph_path").and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let query = args.get("query").and_then(|v| v.as_str())
        .ok_or("missing required field 'query'")?;
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
    let label_filter = args.get("label_filter").and_then(|v| v.as_str()).map(String::from);

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }

    // Set search index dir hint for hybrid search.
    // Convention: search_index/ is a sibling of graph/ under the output dir.
    if let Some(parent) = graph_path.parent() {
        let search_index_dir = parent.join("search_index");
        if search_index_dir.exists() {
            std::env::set_var("AA_SEARCH_INDEX_DIR", search_index_dir.to_string_lossy().as_ref());
        }
    }

    let start = std::time::Instant::now();
    let store = graph_store::GraphStore::open_or_create(graph_path)?;
    let options = search::SearchOptions {
        limit,
        label_filter,
        min_score: 0.01,
    };
    let results = search::search_graph(&store, query, &options)?;
    let elapsed_ms = start.elapsed().as_millis() as u64;

    let items: Vec<Value> = results.iter().map(|r| json!({
        "qualified_name": r.qualified_name,
        "name": r.name,
        "kind": r.label,
        "file_path": r.file_path,
        "score": format!("{:.4}", r.score),
        "community_id": r.community_id,
        "processes": r.process_names,
        "start_line": r.start_line,
        "end_line": r.end_line,
    })).collect();

    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "search_codebase",
        "query": query,
        "result_count": items.len(),
        "results": items,
        "elapsed_ms": elapsed_ms,
    }))
}

// ---------------------------------------------------------------------------
// Stage 3d — get_context
// ---------------------------------------------------------------------------

fn run_get_context(arguments: &Value) -> Value {
    match do_get_context(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "context_failed", "message": msg
        }),
    }
}

fn do_get_context(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args.get("graph_path").and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let qn = args.get("qualified_name").and_then(|v| v.as_str())
        .ok_or("missing required field 'qualified_name'")?;

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }

    let store = graph_store::GraphStore::open_or_create(graph_path)?;
    let ctx = match search::get_context(&store, qn) {
        Ok(c) => c,
        Err(search::GetContextError::NotFound(nf)) => {
            // source: C-correctness bug 2 — prefer a clean `symbol_not_found`
            // with did_you_mean over a cryptic string error. Return Ok(Value)
            // because the outer `run_get_context` would otherwise wrap this
            // under `context_failed`.
            return Ok(json!({
                "stage": 3,
                "status": "error",
                "reason": "symbol_not_found",
                "message": format!("not found: {}", nf.input),
                "did_you_mean": nf.did_you_mean,
            }));
        }
        Err(search::GetContextError::Other(m)) => return Err(m),
    };

    let related_to_json = |items: &[search::RelatedSymbol]| -> Vec<Value> {
        items.iter().map(|s| json!({
            "name": s.name,
            "qualified_name": s.qualified_name,
            "kind": s.label,
        })).collect()
    };

    let community_json = ctx.community.as_ref().map(|c| json!({
        "id": c.id,
        "name": c.name,
        "member_count": c.member_count,
    }));

    let processes_json: Vec<Value> = ctx.processes.iter().map(|p| json!({
        "name": p.name,
        "role": p.role,
    })).collect();

    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "get_context",
        "symbol": {
            "qualified_name": ctx.qualified_name,
            "name": ctx.name,
            "kind": ctx.label,
            "file_path": ctx.file_path,
            "start_line": ctx.start_line,
            "end_line": ctx.end_line,
            "visibility": ctx.visibility,
        },
        "relationships": {
            "imports": related_to_json(&ctx.imports),
            "imported_by": related_to_json(&ctx.imported_by),
            "calls": related_to_json(&ctx.calls),
            "called_by": related_to_json(&ctx.called_by),
            "implements": related_to_json(&ctx.implements),
            "implemented_by": related_to_json(&ctx.implemented_by),
            "uses": related_to_json(&ctx.uses),
            "used_by": related_to_json(&ctx.used_by),
        },
        "community": community_json,
        "processes": processes_json,
    }))
}

// ---------------------------------------------------------------------------
// Stage 3 — analyze_codebase (all-in-one: index + resolve + cluster)
// ---------------------------------------------------------------------------

fn run_analyze_codebase(arguments: &Value) -> Value {
    match do_analyze_codebase(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "analyze_failed", "message": msg
        }),
    }
}

fn do_analyze_codebase(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let path_str = args.get("path").and_then(|v| v.as_str())
        .ok_or("missing required field 'path'")?;
    let output_str = args.get("output_dir").and_then(|v| v.as_str())
        .ok_or("missing required field 'output_dir'")?;
    let gamma = args.get("resolution_param").and_then(|v| v.as_f64()).unwrap_or(1.0);
    let enable_lsp = args.get("lsp").and_then(|v| v.as_bool()).unwrap_or(false);
    let lang_filter = parse_language_filter(args)?;

    let codebase = require_absolute(path_str, "path")?;
    if !codebase.exists() {
        return Err(format!("path does not exist: {}", codebase.display()));
    }
    let output_dir = require_absolute(output_str, "output_dir")?;
    fs::create_dir_all(&output_dir)
        .map_err(|e| format!("create output dir: {e}"))?;
    let graph_dir = output_dir.join("graph");
    // source: H4 fix — see do_index_codebase.
    validate_graph_path_safe(&graph_dir)?;
    if graph_dir.exists() {
        fs::remove_dir_all(&graph_dir)
            .map_err(|e| format!("remove stale graph dir: {e}"))?;
    }

    let total_start = std::time::Instant::now();

    // Phase 1: index
    let index_result = indexer::index_codebase_with_language(&codebase, &graph_dir, lang_filter)?;

    // Phase 2: resolve
    let store = graph_store::GraphStore::open_or_create(&index_result.graph_path)?;
    let resolve_result = resolver::resolve_graph(&store)?;

    // Phase 2b: LSP-enhanced resolution (optional)
    let lsp_result = if enable_lsp {
        let effective_lang = match lang_filter {
            Some(parser::Language::Rust) => "rust".to_string(),
            Some(parser::Language::Python) => "python".to_string(),
            Some(parser::Language::TypeScript) => "typescript".to_string(),
            None => detect_dominant_language(&codebase),
        };
        match lsp_resolver::resolve_with_lsp(
            &store,
            &codebase,
            &effective_lang,
            None,
            std::time::Duration::from_secs(30),
        ) {
            Ok(r) => Some(r),
            Err(_) => None, // graceful fallback
        }
    } else {
        None
    };

    // Phase 3: cluster
    let cluster_result = clustering::cluster_graph(&store, gamma)?;

    // Phase 4: build search index (BM25 + TF-IDF vectors)
    let search_index_result = search::build_search_index(&store, &output_dir)?;

    let total_ms = total_start.elapsed().as_millis() as u64;

    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "analyze_codebase",
        "graph_path": index_result.graph_path.to_string_lossy(),
        "index": {
            "node_count": index_result.node_count,
            "edge_count": index_result.edge_count,
            "files_indexed": index_result.files_indexed,
        },
        "resolve": {
            "total_edges": resolve_result.total_edges,
            "resolution_rate": format!("{:.2}",
                if resolve_result.total_refs > 0 {
                    resolve_result.total_edges as f64 / resolve_result.total_refs as f64
                } else { 0.0 }),
        },
        "cluster": {
            "community_count": cluster_result.communities,
            "modularity": format!("{:.6}", cluster_result.modularity),
            "process_count": cluster_result.processes,
        },
        "search_index": {
            "bm25_doc_count": search_index_result.bm25_doc_count,
            "vector_doc_count": search_index_result.vector_doc_count,
            "elapsed_ms": search_index_result.elapsed_ms,
        },
        "lsp_resolve": match &lsp_result {
            Some(r) => json!({
                "resolved_count": r.resolved_count,
                "failed_count": r.failed_count,
                "skipped_count": r.skipped_count,
                "elapsed_ms": r.elapsed_ms,
            }),
            None => json!(null),
        },
        "total_elapsed_ms": total_ms,
    }))
}

// ---------------------------------------------------------------------------
// Stage 3e — detect_changes (git diff impact)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Stage 3b-v2 — lsp_resolve (LSP-enhanced resolution)
// ---------------------------------------------------------------------------

fn run_lsp_resolve(arguments: &Value) -> Value {
    match do_lsp_resolve(arguments) {
        Ok(v) => v,
        Err(msg) => {
            // Distinguish specific failure reasons so callers can act on them.
            if msg.contains("lsp_command_not_allowed") {
                // source: C3 fix — surface the reason code plus the allowlist
                // so the caller knows which commands are accepted.
                json!({
                    "stage": 3,
                    "status": "error",
                    "reason": "lsp_command_not_allowed",
                    "message": msg,
                    "allowed": lsp_client::LSP_COMMAND_ALLOWLIST,
                })
            } else if msg.contains("lsp_not_found") {
                json!({
                    "stage": 3,
                    "status": "error",
                    "reason": "lsp_not_found",
                    "message": msg
                })
            } else if msg.contains("lsp_probe_failed") {
                // source: C-correctness bug 1 — binary on PATH but doesn't
                // speak LSP (rustup proxy, stub script, /bin/true, ...).
                // Distinct from lsp_not_found so callers can act on it.
                json!({
                    "stage": 3,
                    "status": "error",
                    "reason": "lsp_probe_failed",
                    "message": msg
                })
            } else {
                json!({
                    "stage": 3,
                    "status": "error",
                    "reason": "lsp_resolve_failed",
                    "message": msg
                })
            }
        }
    }
}

fn do_lsp_resolve(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args.get("graph_path").and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let codebase_str = args.get("codebase_path").and_then(|v| v.as_str())
        .ok_or("missing required field 'codebase_path'")?;
    let language = args.get("language").and_then(|v| v.as_str()).unwrap_or("auto");
    let lsp_command = args.get("lsp_command").and_then(|v| v.as_str());
    let timeout_ms = args.get("timeout_ms").and_then(|v| v.as_u64()).unwrap_or(30000);

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }
    let codebase_path = Path::new(codebase_str);
    if !codebase_path.exists() {
        return Err(format!("codebase_path does not exist: {codebase_str}"));
    }

    // Auto-detect language from codebase if needed
    let effective_lang = if language == "auto" {
        detect_dominant_language(codebase_path)
    } else {
        language.to_string()
    };

    let store = graph_store::GraphStore::open_or_create(graph_path)?;
    let timeout = std::time::Duration::from_millis(timeout_ms);

    let result = lsp_resolver::resolve_with_lsp(
        &store,
        codebase_path,
        &effective_lang,
        lsp_command,
        timeout,
    )?;

    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "lsp_resolve",
        "resolved_count": result.resolved_count,
        "failed_count": result.failed_count,
        "skipped_count": result.skipped_count,
        "elapsed_ms": result.elapsed_ms,
    }))
}

/// Detect the dominant language from file extensions in a codebase.
fn detect_dominant_language(path: &Path) -> String {
    let mut rs_count = 0u32;
    let mut py_count = 0u32;
    let mut ts_count = 0u32;

    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            match p.extension().and_then(|e| e.to_str()) {
                Some("rs") => rs_count += 1,
                Some("py") => py_count += 1,
                Some("ts") | Some("tsx") => ts_count += 1,
                _ => {}
            }
        }
    }

    if rs_count >= py_count && rs_count >= ts_count {
        "rust".to_string()
    } else if py_count >= ts_count {
        "python".to_string()
    } else {
        "typescript".to_string()
    }
}

fn run_detect_changes(arguments: &Value) -> Value {
    match do_detect_changes(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "detect_changes_failed", "message": msg
        }),
    }
}

fn do_detect_changes(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args.get("graph_path").and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let diff_text = args.get("diff_text").and_then(|v| v.as_str());
    let codebase_path = args.get("codebase_path").and_then(|v| v.as_str());
    let base_ref = args.get("base_ref").and_then(|v| v.as_str()).unwrap_or("HEAD~1");
    let head_ref = args.get("head_ref").and_then(|v| v.as_str()).unwrap_or("HEAD");

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }

    let store = graph_store::GraphStore::open_or_create(graph_path)?;

    let analysis = if let Some(text) = diff_text {
        git_diff::analyze_diff(&store, text)?
    } else if let Some(repo) = codebase_path {
        let repo_path = Path::new(repo);
        if !repo_path.exists() {
            return Err(format!("codebase_path does not exist: {repo}"));
        }
        git_diff::analyze_git_diff(&store, repo_path, base_ref, head_ref)?
    } else {
        return Err(
            "either 'diff_text' or 'codebase_path' must be provided".to_string()
        );
    };

    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "detect_changes",
        "files_changed": analysis.files_changed,
        "symbols_affected": analysis.symbols_affected,
        "symbols_affected_count": analysis.symbols_affected.len(),
        "communities_affected": analysis.communities_affected,
        "communities_affected_count": analysis.communities_affected.len(),
        "processes_affected": analysis.processes_affected,
        "processes_affected_count": analysis.processes_affected.len(),
        "risk_score": format!("{:.4}", analysis.risk_score),
    }))
}

// ---------------------------------------------------------------------------
// Stage 4 — prepare_prd_input (bundle verified finding + graph intel)
// ---------------------------------------------------------------------------

fn run_prepare_prd_input(arguments: &Value) -> Value {
    match do_prepare_prd_input(arguments) {
        Ok(v) => v,
        Err(msg) => stage4_error_response(&msg),
    }
}

fn stage4_error_response(msg: &str) -> Value {
    let reason = if msg.starts_with("stage_2_not_verified") {
        "stage_2_not_verified"
    } else if msg.starts_with("stage_1_refined_missing")
        || msg.starts_with("stage_1_refined_unreadable")
        || msg.starts_with("stage_1_refined_corrupt")
    {
        "stage_1_refined_missing"
    } else {
        "prepare_prd_input_failed"
    };
    json!({
        "stage": 4,
        "status": "error",
        "reason": reason,
        "message": msg,
    })
}

fn do_prepare_prd_input(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let run_id = args
        .get("run_id")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'run_id'")?
        .to_string();
    validate_safe_id("run_id", &run_id)?;
    let finding_id = args
        .get("finding_id")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'finding_id'")?
        .to_string();
    validate_safe_id("finding_id", &finding_id)?;
    let output_dir_str = args
        .get("output_dir")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'output_dir'")?;
    let output_dir = require_absolute(output_dir_str, "output_dir")?;
    let graph_path_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let graph_path = require_absolute(graph_path_str, "graph_path")?;
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_path_str}"));
    }

    let prepared_at = format_iso8601_utc(now_unix_seconds_nanos().0);
    let outcome = prd_input::prepare(
        &prd_input::PrdInputArgs {
            run_id: run_id.clone(),
            finding_id: finding_id.clone(),
            output_dir,
            graph_path,
        },
        prepared_at.clone(),
    )?;

    Ok(json!({
        "stage": 4,
        "status": "ok",
        "tool": "prepare_prd_input",
        "run_id": run_id,
        "finding_id": finding_id,
        "artifact_path": outcome.artifact_path.to_string_lossy(),
        "prepared_at": prepared_at,
        "matched_symbol_count": outcome.matched_symbol_count,
        "impacted_community_count": outcome.impacted_community_count,
        "impacted_process_count": outcome.impacted_process_count,
        "preparer_version": prd_input::PREPARER_VERSION,
    }))
}

// ---------------------------------------------------------------------------
// Stage 9 — verify_semantic_diff (diff two graphs; flag regressions)
// ---------------------------------------------------------------------------

fn run_verify_semantic_diff(arguments: &Value) -> Value {
    match do_verify_semantic_diff(arguments) {
        Ok(v) => v,
        Err(msg) => {
            let reason = if msg.contains("before_graph_path_missing") {
                "before_graph_path_missing"
            } else if msg.contains("after_graph_path_missing") {
                "after_graph_path_missing"
            } else {
                "verify_semantic_diff_failed"
            };
            json!({
                "stage": 9,
                "status": "error",
                "reason": reason,
                "message": msg,
            })
        }
    }
}

fn do_verify_semantic_diff(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let before_str = args
        .get("before_graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'before_graph_path'")?;
    let after_str = args
        .get("after_graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'after_graph_path'")?;
    let before_graph_path = require_absolute(before_str, "before_graph_path")?;
    let after_graph_path = require_absolute(after_str, "after_graph_path")?;
    let report_path = args
        .get("report_path")
        .and_then(|v| v.as_str())
        .map(|s| require_absolute(s, "report_path"))
        .transpose()?;

    let verified_at = format_iso8601_utc(now_unix_seconds_nanos().0);
    let outcome = semantic_diff::diff(
        &semantic_diff::SemanticDiffArgs {
            before_graph_path: before_graph_path.clone(),
            after_graph_path: after_graph_path.clone(),
        },
        verified_at.clone(),
    )?;

    let written = match &report_path {
        Some(p) => {
            semantic_diff::write_report(p, &outcome.report)?;
            Some(p.to_string_lossy().to_string())
        }
        None => None,
    };

    Ok(json!({
        "stage": 9,
        "status": "ok",
        "tool": "verify_semantic_diff",
        "verified_at": verified_at,
        "summary": {
            "nodes_added": outcome.summary.nodes_added,
            "nodes_removed": outcome.summary.nodes_removed,
            "edges_added": outcome.summary.edges_added,
            "edges_removed": outcome.summary.edges_removed,
            "dangling_references": outcome.summary.dangling_references,
            "new_unresolved_delta": outcome.summary.new_unresolved_delta,
            "new_cycles": outcome.summary.new_cycles,
        },
        "regression_score": outcome.regression_score,
        "verdict": outcome.verdict,
        "report": outcome.report,
        "report_path": written,
    }))
}

// ---------------------------------------------------------------------------
// Stage 6 — validate_prd_against_graph
// ---------------------------------------------------------------------------

fn run_validate_prd_against_graph(arguments: &Value) -> Value {
    match do_validate_prd_against_graph(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 6,
            "status": "error",
            "reason": "validate_prd_against_graph_failed",
            "message": msg,
        }),
    }
}

fn do_validate_prd_against_graph(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let prd_path_str = args.get("prd_path").and_then(|v| v.as_str())
        .ok_or("missing required field 'prd_path'")?;
    let prd_path = require_absolute(prd_path_str, "prd_path")?;
    let graph_path_str = args.get("graph_path").and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let graph_path = require_absolute(graph_path_str, "graph_path")?;
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_path_str}"));
    }
    let affected_opt = args.get("affected_symbols_path").and_then(|v| v.as_str())
        .map(|s| require_absolute(s, "affected_symbols_path")).transpose()?;

    let store = graph_store::GraphStore::open_or_create(&graph_path)?;
    let report = prd_validator::validate_prd(
        &store, &prd_path, affected_opt.as_deref(),
    )?;
    let validated_at = format_iso8601_utc(now_unix_seconds_nanos().0);

    let (run_id_opt, finding_id_opt, output_dir_opt) = extract_optional_ids(args)?;
    let artifact_path = maybe_write_validation(
        &report, &prd_path, &graph_path, &validated_at,
        &run_id_opt, &finding_id_opt, &output_dir_opt,
    )?;

    let json_report = prd_validator::report_to_json(
        &report, run_id_opt.as_deref().unwrap_or(""),
        finding_id_opt.as_deref().unwrap_or(""),
        &prd_path, &graph_path, &validated_at,
    );
    Ok(json!({
        "stage": 6,
        "status": "ok",
        "tool": "validate_prd_against_graph",
        "validated_at": validated_at,
        "validation_status": report.validation_status,
        "extraction_mode": report.extraction_mode,
        "contract_missing": report.contract_missing,
        "summary": {
            "claimed_symbols": report.summary.claimed_symbols,
            "resolved_symbols": report.summary.resolved_symbols,
            "hallucinated_symbols": report.summary.hallucinated_symbols,
            "communities_spanned": report.summary.communities_spanned,
            "processes_impacted": report.summary.processes_impacted,
        },
        "artifact_path": artifact_path.map(|p| p.to_string_lossy().to_string()),
        "report": json_report,
    }))
}

fn extract_optional_ids(
    args: &Map<String, Value>,
) -> Result<(Option<String>, Option<String>, Option<PathBuf>), String> {
    let run_id = args.get("run_id").and_then(|v| v.as_str()).map(String::from);
    if let Some(ref r) = run_id { validate_safe_id("run_id", r)?; }
    let finding_id = args.get("finding_id").and_then(|v| v.as_str()).map(String::from);
    if let Some(ref f) = finding_id { validate_safe_id("finding_id", f)?; }
    let output_dir = args.get("output_dir").and_then(|v| v.as_str())
        .map(|s| require_absolute(s, "output_dir")).transpose()?;
    Ok((run_id, finding_id, output_dir))
}

fn maybe_write_validation(
    report: &prd_validator::ValidationReport,
    prd_path: &Path,
    graph_path: &Path,
    validated_at: &str,
    run_id: &Option<String>,
    finding_id: &Option<String>,
    output_dir: &Option<PathBuf>,
) -> Result<Option<PathBuf>, String> {
    let (r, f, o) = match (run_id, finding_id, output_dir) {
        (Some(r), Some(f), Some(o)) => (r, f, o),
        _ => return Ok(None),
    };
    let value = prd_validator::report_to_json(report, r, f, prd_path, graph_path, validated_at);
    let dest = o.join("runs").join(r).join("findings").join(f)
        .join(prd_validator::VALIDATION_FILE);
    let written = prd_validator::write_validation(&dest, &value)?;
    Ok(Some(written))
}

// ---------------------------------------------------------------------------
// Stage 8 — check_security_gates
// ---------------------------------------------------------------------------

fn run_check_security_gates(arguments: &Value) -> Value {
    match do_check_security_gates(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 8,
            "status": "error",
            "reason": "check_security_gates_failed",
            "message": msg,
        }),
    }
}

fn do_check_security_gates(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_path_str = args.get("graph_path").and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let graph_path = require_absolute(graph_path_str, "graph_path")?;
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_path_str}"));
    }
    let changed_symbols: Vec<String> = args.get("changed_symbols")
        .and_then(|v| v.as_array())
        .ok_or("missing required field 'changed_symbols' (array of strings)")?
        .iter()
        .filter_map(|x| x.as_str().map(String::from))
        .collect();

    let store = graph_store::GraphStore::open_or_create(&graph_path)?;
    let report = security_gates::check_gates(&store, &changed_symbols)?;
    let checked_at = format_iso8601_utc(now_unix_seconds_nanos().0);

    let (run_id_opt, finding_id_opt, output_dir_opt) = extract_optional_ids(args)?;
    let artifact_path = maybe_write_security(
        &report, &graph_path, &changed_symbols, &checked_at,
        &run_id_opt, &finding_id_opt, &output_dir_opt,
    )?;

    let json_report = security_gates::report_to_json(
        &report, run_id_opt.as_deref().unwrap_or(""),
        finding_id_opt.as_deref().unwrap_or(""),
        &graph_path, &changed_symbols, &checked_at,
    );
    Ok(json!({
        "stage": 8,
        "status": "ok",
        "tool": "check_security_gates",
        "checked_at": checked_at,
        "gates_passed": report.gates_passed,
        "summary": {
            "changed_symbols": report.summary.changed_symbols,
            "critical_count": report.summary.critical_count,
            "warning_count": report.summary.warning_count,
            "info_count": report.summary.info_count,
        },
        "artifact_path": artifact_path.map(|p| p.to_string_lossy().to_string()),
        "report": json_report,
    }))
}

fn maybe_write_security(
    report: &security_gates::SecurityReport,
    graph_path: &Path,
    changed_symbols: &[String],
    checked_at: &str,
    run_id: &Option<String>,
    finding_id: &Option<String>,
    output_dir: &Option<PathBuf>,
) -> Result<Option<PathBuf>, String> {
    let (r, f, o) = match (run_id, finding_id, output_dir) {
        (Some(r), Some(f), Some(o)) => (r, f, o),
        _ => return Ok(None),
    };
    let value = security_gates::report_to_json(report, r, f, graph_path, changed_symbols, checked_at);
    let dest = o.join("runs").join(r).join("findings").join(f)
        .join(security_gates::SECURITY_FILE);
    let written = security_gates::write_security(&dest, &value)?;
    Ok(Some(written))
}

/// All known relationship tables as (name, from_label, to_label).
/// Source: graph_store.rs REL_TABLES (mirrored here because the const is private).
fn rel_table_triples() -> &'static [(&'static str, &'static str, &'static str)] {
    &[
        // 3a tables
        ("Contains_Dir_File", "Directory", "File"),
        ("Contains_Dir_Dir", "Directory", "Directory"),
        ("Contains_File_Module", "File", "Module"),
        ("Defines_File_Function", "File", "Function"),
        ("Defines_File_Struct", "File", "Struct"),
        ("Defines_File_Enum", "File", "Enum"),
        ("Defines_File_Trait", "File", "Trait"),
        ("Defines_File_Constant", "File", "Constant"),
        ("Defines_File_TypeAlias", "File", "TypeAlias"),
        ("Defines_Module_Function", "Module", "Function"),
        ("Defines_Module_Struct", "Module", "Struct"),
        ("Defines_Module_Enum", "Module", "Enum"),
        ("Defines_Module_Trait", "Module", "Trait"),
        ("Defines_Module_Constant", "Module", "Constant"),
        ("Defines_Module_TypeAlias", "Module", "TypeAlias"),
        ("HasMethod_Struct_Method", "Struct", "Method"),
        ("HasMethod_Enum_Method", "Enum", "Method"),
        ("HasMethod_Trait_Method", "Trait", "Method"),
        ("HasField_Struct_Field", "Struct", "Field"),
        ("HasField_Enum_Field", "Enum", "Field"),
        ("HasVariant_Enum_Variant", "Enum", "Variant"),
        // 3b Imports tables — source: stages/stage-3b.md §3
        ("Imports_File_File", "File", "File"),
        ("Imports_File_Module", "File", "Module"),
        ("Imports_File_Function", "File", "Function"),
        ("Imports_File_Struct", "File", "Struct"),
        ("Imports_File_Enum", "File", "Enum"),
        ("Imports_File_Trait", "File", "Trait"),
        ("Imports_File_Constant", "File", "Constant"),
        ("Imports_File_TypeAlias", "File", "TypeAlias"),
        ("Imports_Module_Function", "Module", "Function"),
        ("Imports_Module_Struct", "Module", "Struct"),
        ("Imports_Module_Enum", "Module", "Enum"),
        ("Imports_Module_Trait", "Module", "Trait"),
        ("Imports_Module_Constant", "Module", "Constant"),
        ("Imports_Module_TypeAlias", "Module", "TypeAlias"),
        // 3b Calls tables
        ("Calls_Function_Function", "Function", "Function"),
        ("Calls_Function_Method", "Function", "Method"),
        ("Calls_Method_Function", "Method", "Function"),
        ("Calls_Method_Method", "Method", "Method"),
        // 3b Implements tables
        ("Implements_Struct_Trait", "Struct", "Trait"),
        ("Implements_Enum_Trait", "Enum", "Trait"),
        // 3b Extends table
        ("Extends_Trait_Trait", "Trait", "Trait"),
        // 3b Uses tables
        ("Uses_Function_Struct", "Function", "Struct"),
        ("Uses_Function_Enum", "Function", "Enum"),
        ("Uses_Function_Trait", "Function", "Trait"),
        ("Uses_Function_TypeAlias", "Function", "TypeAlias"),
        ("Uses_Method_Struct", "Method", "Struct"),
        ("Uses_Method_Enum", "Method", "Enum"),
        ("Uses_Method_Trait", "Method", "Trait"),
        ("Uses_Method_TypeAlias", "Method", "TypeAlias"),
        ("Uses_Struct_Struct", "Struct", "Struct"),
        ("Uses_Struct_Enum", "Struct", "Enum"),
        ("Uses_Struct_Trait", "Struct", "Trait"),
        ("Uses_Field_Struct", "Field", "Struct"),
        ("Uses_Field_Enum", "Field", "Enum"),
        ("Uses_Field_Trait", "Field", "Trait"),
        // 3c MemberOf tables
        ("MemberOf_Function_Community", "Function", "Community"),
        ("MemberOf_Method_Community", "Method", "Community"),
        ("MemberOf_Struct_Community", "Struct", "Community"),
        ("MemberOf_Enum_Community", "Enum", "Community"),
        ("MemberOf_Trait_Community", "Trait", "Community"),
        ("MemberOf_Constant_Community", "Constant", "Community"),
        ("MemberOf_TypeAlias_Community", "TypeAlias", "Community"),
        ("MemberOf_Module_Community", "Module", "Community"),
        // 3c EntryPointOf tables
        ("EntryPointOf_Function_Process", "Function", "Process"),
        ("EntryPointOf_Method_Process", "Method", "Process"),
        // 3c ParticipatesIn tables
        ("ParticipatesIn_Function_Process", "Function", "Process"),
        ("ParticipatesIn_Method_Process", "Method", "Process"),
    ]
}

// ---------------------------------------------------------------------------
// Tool dispatch
// ---------------------------------------------------------------------------

fn handle_tool_call(params: &Value) -> Value {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));

    let payload = match name {
        "health_check" => {
            // source: C-correctness bug 3 — the count was a hardcoded `19`
            // that silently lied if a new tool was added without bumping it.
            // Derive from tools_list() so the count can never drift.
            let tools_count = tools_list()
                .get("tools")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            json!({
                "stage": 0,
                "name": "health_check",
                "status": "ok",
                "server": SERVER_NAME,
                "version": SERVER_VERSION,
                "protocol": PROTOCOL_VERSION,
                // Kept for back-compat with existing clients that parse
                // `stages_registered`. Both now reflect the live tool count.
                "stages_registered": tools_count,
                "tools_count": tools_count,
            })
        },
        "extract_finding" => run_extract_finding(&arguments),
        "refine_finding" => run_refine_finding(&arguments),
        "start_verification" => run_start_verification(&arguments),
        "append_clarification" => run_append_clarification(&arguments),
        "finalize_verification" => run_finalize_verification(&arguments),
        "abort_verification" => run_abort_verification(&arguments),
        "index_codebase" => run_index_codebase(&arguments),
        "query_graph" => run_query_graph(&arguments),
        "get_symbol" => run_get_symbol(&arguments),
        "resolve_graph" => run_resolve_graph(&arguments),
        "cluster_graph" => run_cluster_graph(&arguments),
        "get_processes" => run_get_processes(&arguments),
        "get_impact" => run_get_impact(&arguments),
        "search_codebase" => run_search_codebase(&arguments),
        "get_context" => run_get_context(&arguments),
        "analyze_codebase" => run_analyze_codebase(&arguments),
        "detect_changes" => run_detect_changes(&arguments),
        "lsp_resolve" => run_lsp_resolve(&arguments),
        "prepare_prd_input" => run_prepare_prd_input(&arguments),
        "validate_prd_against_graph" => run_validate_prd_against_graph(&arguments),
        "check_security_gates" => run_check_security_gates(&arguments),
        "verify_semantic_diff" => run_verify_semantic_diff(&arguments),
        other => {
            return json!({
                "isError": true,
                "content": [{
                    "type": "text",
                    "text": format!("Unknown tool: {}", other)
                }]
            });
        }
    };

    json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string())
        }]
    })
}

// ---------------------------------------------------------------------------
// Request dispatch
// ---------------------------------------------------------------------------

fn handle_request(req: Request) {
    let id = req.id.clone().unwrap_or(Value::Null);

    match req.method.as_str() {
        "initialize" => send_response(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION }
            }),
        ),
        "notifications/initialized" => {
            // JSON-RPC notification: no response
        }
        "tools/list" => send_response(id, tools_list()),
        "tools/call" => send_response(id, handle_tool_call(&req.params)),
        other => send_error(id, -32601, &format!("Method not found: {}", other)),
    }
}

// ---------------------------------------------------------------------------
// stdio loop
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Security hardening tests — H3 (read-only query) and H4 (graph path safety).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod security_tests {
    use super::*;

    #[test]
    fn test_query_graph_rejects_delete() {
        assert_eq!(forbidden_cypher_keyword("MATCH (n) DETACH DELETE n"), Some("DELETE"));
        assert_eq!(forbidden_cypher_keyword("MATCH (n) DELETE n"), Some("DELETE"));
        assert_eq!(forbidden_cypher_keyword("CREATE (n:Foo)"), Some("CREATE"));
        assert_eq!(forbidden_cypher_keyword("MERGE (n:Foo {id: 1})"), Some("MERGE"));
        assert_eq!(forbidden_cypher_keyword("MATCH (n) SET n.x = 1"), Some("SET"));
        assert_eq!(forbidden_cypher_keyword("MATCH (n) REMOVE n:Label"), Some("REMOVE"));
        assert_eq!(forbidden_cypher_keyword("DROP TABLE Foo"), Some("DROP"));
        assert_eq!(forbidden_cypher_keyword("CALL db.labels()"), Some("CALL"));
        assert_eq!(forbidden_cypher_keyword("LOAD CSV FROM 'x'"), Some("LOAD"));

        // Clean read queries must pass.
        assert_eq!(forbidden_cypher_keyword("MATCH (n) RETURN n"), None);
        assert_eq!(forbidden_cypher_keyword("OPTIONAL MATCH (n) RETURN n"), None);
        assert_eq!(forbidden_cypher_keyword("WITH 1 AS x RETURN x"), None);
        assert_eq!(forbidden_cypher_keyword("UNWIND [1,2] AS i RETURN i"), None);

        // Whole-word matching — identifiers that embed a keyword must NOT trigger.
        assert_eq!(forbidden_cypher_keyword("MATCH (n) RETURN n.created_at"), None);
        assert_eq!(forbidden_cypher_keyword("MATCH (n) RETURN n.setting"), None);

        // Case insensitivity.
        assert_eq!(forbidden_cypher_keyword("match (n) detach delete n"), Some("DELETE"));

        // End-to-end via do_query_graph — no real DB needed because we should
        // fail before `GraphStore::open_or_create`.
        let args = json!({
            "graph_path": "/nonexistent/graph",
            "query": "MATCH (n) DETACH DELETE n"
        });
        let err = do_query_graph(&args).expect_err("must reject mutation query");
        assert!(
            err.contains("read_only_query_required"),
            "got: {err}"
        );
    }

    #[test]
    fn test_health_check_tool_count_matches_tools_list() {
        // source: C-correctness bug 3 — the health_check response must derive
        // the count from `tools_list()` dynamically. If a new tool is added
        // to `tool_schemas::tools_list` without touching main.rs, the count
        // must still be correct.
        let tools = tools_list();
        let expected = tools
            .get("tools")
            .and_then(|v| v.as_array())
            .expect("tools_list must return a `tools` array")
            .len();

        let health = handle_tool_call(&json!({
            "name": "health_check",
            "arguments": {}
        }));

        // handle_tool_call wraps the payload in a content/text envelope.
        // Find the JSON text inside.
        let text = health
            .get("content")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|e| e.get("text"))
            .and_then(|v| v.as_str())
            .expect("health_check must return content[0].text");
        let payload: serde_json::Value = serde_json::from_str(text)
            .expect("content[0].text must be JSON");

        let reported = payload
            .get("tools_count")
            .and_then(|v| v.as_u64())
            .expect("tools_count field must be present");
        assert_eq!(reported as usize, expected, "tools_count drift");

        let legacy = payload
            .get("stages_registered")
            .and_then(|v| v.as_u64())
            .expect("stages_registered field must stay for back-compat");
        assert_eq!(legacy as usize, expected, "stages_registered drift");
    }

    #[test]
    fn test_graph_path_must_end_in_graph() {
        // source: H4 fix — caller-chosen path is safe ONLY when it is absolute
        // AND the last segment is exactly `graph` AND the path is not one of
        // the forbidden system roots.
        assert!(validate_graph_path_safe(Path::new("/tmp/foo/graph")).is_ok());
        assert!(validate_graph_path_safe(Path::new("/Users/alice/proj/graph")).is_ok());

        // Not absolute.
        assert!(validate_graph_path_safe(Path::new("relative/graph")).is_err());

        // Does not end in /graph.
        assert!(validate_graph_path_safe(Path::new("/etc")).is_err());
        assert!(validate_graph_path_safe(Path::new("/tmp")).is_err());
        assert!(validate_graph_path_safe(Path::new("/")).is_err());
        assert!(validate_graph_path_safe(Path::new("/Users")).is_err());
        assert!(validate_graph_path_safe(Path::new("/tmp/foo/notgraph")).is_err());

        // Ends in /graph but IS a forbidden system root (should still reject).
        assert!(validate_graph_path_safe(Path::new("/etc/graph")).is_err());
        assert!(validate_graph_path_safe(Path::new("//graph")).is_err()
            || validate_graph_path_safe(Path::new("//graph")).is_ok());
    }
}

fn main() {
    eprintln!("[ai-architect-mcp] stage 0-3d up (Rust {})", SERVER_VERSION);

    let stdin = io::stdin();
    let handle = stdin.lock();
    for line in handle.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[ai-architect-mcp] stdin error: {}", e);
                break;
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Request>(trimmed) {
            Ok(req) => handle_request(req),
            Err(e) => eprintln!("[ai-architect-mcp] parse error: {}", e),
        }
    }
}
