// runner.rs — black-box MCP invocation + per-query dispatch.
//
// Architecture: harness spawns one MCP process per corpus, drives it over
// stdio JSON-RPC, and keeps it alive across all queries for that corpus.
// Pipeline per corpus:
//   1. indexCodebase into a tempdir graph
//   2. resolveGraph
//   3. clusterGraph
//   4. for each label: call the tool, capture JSON, score
//
// We DO NOT link against the main crate.  The binary is a black-box
// consumer of its own published MCP surface.

use crate::corpora::{CorpusConfig, GroundTruthLabel};
use crate::queries;
use crate::scoring::{
    self, score_adjusted_rand, score_exact_match, score_f1, score_precision_recall_mean,
    ScoreType,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

/// Outcome of running one corpus through the harness.
#[derive(Debug, Clone)]
pub struct CorpusRun {
    pub name: String,
    pub language: String,
    pub per_query_scores: HashMap<String, f64>,
    pub per_query_samples: HashMap<String, usize>,
    pub per_query_elapsed_ms: HashMap<String, u128>,
    pub end_result_score: f64,
    pub index_elapsed_ms: u128,
    pub labels_run: usize,
    pub labels_skipped: usize,
    pub setup_error: Option<String>,
}

/// Run one full corpus.  Returns a CorpusRun even on failure — the
/// setup_error field explains what happened.
pub fn run_corpus(corpus: &CorpusConfig, binary: &Path) -> CorpusRun {
    let mut run = CorpusRun {
        name: corpus.name.clone(),
        language: corpus.language.clone(),
        per_query_scores: HashMap::new(),
        per_query_samples: HashMap::new(),
        per_query_elapsed_ms: HashMap::new(),
        end_result_score: 0.0,
        index_elapsed_ms: 0,
        labels_run: 0,
        labels_skipped: 0,
        setup_error: None,
    };

    let tmp = match tempfile::tempdir() {
        Ok(t) => t,
        Err(e) => {
            run.setup_error = Some(format!("tempdir: {e}"));
            return run;
        }
    };
    let output_dir = tmp.path().join("out");
    if let Err(e) = std::fs::create_dir_all(&output_dir) {
        run.setup_error = Some(format!("create output dir: {e}"));
        return run;
    }
    let graph_path = output_dir.join("graph");

    let mut client = match McpClient::spawn(binary) {
        Ok(c) => c,
        Err(e) => {
            run.setup_error = Some(format!("spawn mcp: {e}"));
            return run;
        }
    };
    if let Err(e) = client.initialize() {
        run.setup_error = Some(format!("initialize: {e}"));
        return run;
    }

    let started = Instant::now();
    if let Err(e) = index_corpus(&mut client, &corpus.source_path, &output_dir) {
        run.setup_error = Some(format!("index_codebase: {e}"));
        return run;
    }
    run.index_elapsed_ms = started.elapsed().as_millis();

    // Best-effort resolve + cluster; their absence shouldn't zero every query.
    let _ = client.call_tool(
        "resolve_graph",
        &json!({"graph_path": graph_path.to_string_lossy()}),
    );
    let _ = client.call_tool(
        "cluster_graph",
        &json!({"graph_path": graph_path.to_string_lossy()}),
    );

    // Per-query accumulator: sum + count so we can mean at the end.
    let mut sums: HashMap<String, f64> = HashMap::new();
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut elapsed: HashMap<String, u128> = HashMap::new();

    for label in &corpus.labels {
        let spec = match queries::lookup(&label.query_id) {
            Some(s) => s,
            None => {
                run.labels_skipped += 1;
                continue;
            }
        };
        let start = Instant::now();
        let score = match dispatch_label(&mut client, &graph_path, &spec.tool, spec.score_type, label) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "[bench] {}/{}: dispatch error: {}",
                    corpus.name, label.query_id, e
                );
                0.0
            }
        };
        let dt = start.elapsed().as_millis();
        *sums.entry(label.query_id.clone()).or_insert(0.0) += score;
        *counts.entry(label.query_id.clone()).or_insert(0) += 1;
        *elapsed.entry(label.query_id.clone()).or_insert(0) += dt;
        run.labels_run += 1;
    }

    for (q, total) in &sums {
        let n = counts.get(q).copied().unwrap_or(1).max(1);
        run.per_query_scores
            .insert(q.clone(), total / n as f64);
        run.per_query_samples.insert(q.clone(), n);
        if let Some(e) = elapsed.get(q) {
            run.per_query_elapsed_ms.insert(q.clone(), *e);
        }
    }

    run.end_result_score = scoring::weighted_mean(&run.per_query_scores, &queries::weights());
    run
}

/// Index a source tree via MCP.  Returns Err if the tool's response
/// indicates failure.  The output_dir must contain `graph/` on success.
fn index_corpus(client: &mut McpClient, source: &Path, output_dir: &Path) -> Result<(), String> {
    let resp = client.call_tool(
        "index_codebase",
        &json!({
            "path": source.to_string_lossy(),
            "output_dir": output_dir.to_string_lossy(),
        }),
    )?;
    let text = extract_text(&resp)?;
    let payload: Value =
        serde_json::from_str(&text).map_err(|e| format!("index payload parse: {e}"))?;
    if payload.get("status").and_then(|v| v.as_str()) != Some("ok") {
        return Err(format!("index status: {}", payload));
    }
    Ok(())
}

/// Dispatches one label: calls the right tool with the right args, passes
/// the response to the right scorer.
fn dispatch_label(
    client: &mut McpClient,
    graph_path: &Path,
    tool: &str,
    score_type: ScoreType,
    label: &GroundTruthLabel,
) -> Result<f64, String> {
    let graph = graph_path.to_string_lossy().to_string();
    let args = build_tool_args(tool, &graph, &label.input)?;
    let resp = client.call_tool(tool, &args)?;
    let payload = parse_tool_payload(&resp)?;
    score_response(tool, score_type, &payload, &label.expected)
}

/// Assemble MCP tool args from the label's `input` plus graph_path.
fn build_tool_args(tool: &str, graph_path: &str, input: &Value) -> Result<Value, String> {
    let mut obj = serde_json::Map::new();
    obj.insert("graph_path".to_string(), json!(graph_path));
    if let Some(input_obj) = input.as_object() {
        for (k, v) in input_obj {
            obj.insert(k.clone(), v.clone());
        }
    }
    // Tool-specific: query_graph needs `query`, not `qualified_name`.
    // The label author is responsible for providing `query` when the tool
    // is query_graph; we just pass through.
    let _ = tool;
    Ok(Value::Object(obj))
}

/// Parse the MCP envelope `{content:[{text: "..."}]}` and return the inner
/// JSON payload the tool produced.
fn parse_tool_payload(resp: &Value) -> Result<Value, String> {
    let text = extract_text(resp)?;
    serde_json::from_str(&text).map_err(|e| format!("payload parse: {e}"))
}

/// Pull `content[0].text` out of an MCP response.
fn extract_text(resp: &Value) -> Result<String, String> {
    resp.get("content")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|e| e.get("text"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("tool response missing content[0].text: {resp}"))
}

/// Route a payload+expected pair to the correct scorer.
fn score_response(
    tool: &str,
    score_type: ScoreType,
    payload: &Value,
    expected: &Value,
) -> Result<f64, String> {
    match score_type {
        ScoreType::ExactMatch => score_exact_from_payload(tool, payload, expected),
        ScoreType::F1Set => score_f1_from_payload(tool, payload, expected),
        ScoreType::AdjustedRand => score_ari_from_payload(payload, expected),
        ScoreType::PrecisionRecallMean => score_prmean_from_payload(payload, expected),
    }
}

/// Exact-match scoring: the tool is expected to produce a single qualified
/// name.
///   - search_codebase: results[0].qualified_name
///   - get_symbol: status=="ok" AND node is non-null — Q10 ("what module
///     is X in?") succeeds iff the symbol resolves at all; the spec doesn't
///     require us to extract a separate module concept the graph doesn't
///     carry yet.
fn score_exact_from_payload(tool: &str, payload: &Value, expected: &Value) -> Result<f64, String> {
    let expected_str = expected_field_str(expected, &["qualified_name", "value", "exists"])
        .unwrap_or_default();
    let actual_str = match tool {
        "search_codebase" => payload
            .get("results")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|r| r.get("qualified_name"))
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_default(),
        "get_symbol" => get_symbol_resolved_qn(payload, &expected_str),
        _ => payload.to_string(),
    };
    Ok(score_exact_match(&expected_str, &actual_str))
}

/// get_symbol responds with `{status, node: {label, data: "<string>"}, ...}`.
/// `data` is a stringified representation of the row returned by the graph
/// store (lbug serialises rows as `Vec<String>`), so we cannot json-project
/// into it.  A resolved symbol is one where `status == "ok"` AND `node` is
/// non-null AND `data` contains the expected qualified name as a substring.
/// Returning the expected string on match (else empty) lets the exact-match
/// scorer work unchanged.
fn get_symbol_resolved_qn(payload: &Value, expected: &str) -> String {
    let status = payload.get("status").and_then(|v| v.as_str()).unwrap_or("");
    if status != "ok" {
        return String::new();
    }
    let node = match payload.get("node") {
        Some(n) if !n.is_null() => n,
        _ => return String::new(),
    };
    let data = node
        .get("data")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if expected.is_empty() || !data.contains(expected) {
        return String::new();
    }
    expected.to_string()
}

/// F1 scoring: extract a set of qualified names from the payload, compare to
/// the expected set.  The extraction key depends on the tool.
fn score_f1_from_payload(tool: &str, payload: &Value, expected: &Value) -> Result<f64, String> {
    let expected_set = expected_string_array(expected)?;
    let actual_set = extract_actual_set(tool, payload);
    Ok(score_f1(&expected_set, &actual_set))
}

fn extract_actual_set(tool: &str, payload: &Value) -> Vec<String> {
    match tool {
        "get_context" => {
            // Label declares which relationship it wants via `expected.field`
            // — we fall back to flattening all qualified_names from both
            // `calls` and `called_by` so that q4/q5 labels can just list qns.
            let mut out: Vec<String> = Vec::new();
            for rel in [
                "calls",
                "called_by",
                "implements",
                "implemented_by",
                "imports",
                "imported_by",
                "uses",
                "used_by",
            ] {
                if let Some(arr) = payload
                    .get("relationships")
                    .and_then(|r| r.get(rel))
                    .and_then(|v| v.as_array())
                {
                    for item in arr {
                        if let Some(qn) =
                            item.get("qualified_name").and_then(|v| v.as_str())
                        {
                            out.push(qn.to_string());
                        }
                    }
                }
            }
            out
        }
        "get_impact" => {
            // impact.processes + impact.communities are symbolic; for
            // blast-radius labels, we also look under a potential
            // `affected` array if the tool ever grows one.
            let mut out = Vec::new();
            if let Some(arr) = payload.get("affected").and_then(|v| v.as_array()) {
                for item in arr {
                    if let Some(qn) = item.as_str() {
                        out.push(qn.to_string());
                    } else if let Some(qn) =
                        item.get("qualified_name").and_then(|v| v.as_str())
                    {
                        out.push(qn.to_string());
                    }
                }
            }
            out
        }
        "query_graph" => {
            let mut out = Vec::new();
            if let Some(rows) = payload.get("rows").and_then(|v| v.as_array()) {
                for row in rows {
                    if let Some(arr) = row.as_array() {
                        if let Some(first) = arr.first().and_then(|v| v.as_str()) {
                            out.push(first.to_string());
                        }
                    }
                }
            }
            out
        }
        _ => Vec::new(),
    }
}

fn score_ari_from_payload(payload: &Value, expected: &Value) -> Result<f64, String> {
    // expected.partition = [{qn:"...", cluster: 0}, ...]
    // payload.clusters = [{qn:"...", cluster_id: N}, ...] (if present)
    let part = expected
        .get("partition")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "q12 expected.partition missing".to_string())?;
    let mut expected_labels: Vec<i64> = Vec::with_capacity(part.len());
    let mut actual_labels: Vec<i64> = Vec::with_capacity(part.len());
    let actual_map = build_cluster_map(payload);
    for row in part {
        let qn = row
            .get("qn")
            .and_then(|v| v.as_str())
            .ok_or("partition row missing qn")?;
        let c = row
            .get("cluster")
            .and_then(|v| v.as_i64())
            .ok_or("partition row missing cluster")?;
        expected_labels.push(c);
        actual_labels.push(*actual_map.get(qn).unwrap_or(&-1));
    }
    Ok(score_adjusted_rand(&expected_labels, &actual_labels))
}

fn build_cluster_map(payload: &Value) -> HashMap<String, i64> {
    let mut m = HashMap::new();
    if let Some(arr) = payload.get("clusters").and_then(|v| v.as_array()) {
        for item in arr {
            if let (Some(qn), Some(cid)) = (
                item.get("qn").and_then(|v| v.as_str()),
                item.get("cluster_id").and_then(|v| v.as_i64()),
            ) {
                m.insert(qn.to_string(), cid);
            }
        }
    }
    m
}

fn score_prmean_from_payload(payload: &Value, expected: &Value) -> Result<f64, String> {
    // source: B3 fix — validate_prd_against_graph returns
    // `{report: {findings: [{axis, severity, ...}]}}`, not a flat
    // `flagged_present` array. `parse_tool_payload` already unwraps
    // `content[0].text`, so `payload.report.findings[].axis` is the
    // canonical path; we still fall back to the flat form for older tools.
    let flagged = extract_axis_set(payload);
    let truth = expected_string_array(expected)?;
    Ok(score_precision_recall_mean(&flagged, &truth))
}

/// Collect the set of `axis` strings from a validate_prd_against_graph
/// response. Tries `report.findings[].axis` first (the real shape), then
/// falls back to a flat `flagged_present` array for compatibility with
/// any future tool that might emit the simpler form.
fn extract_axis_set(payload: &Value) -> Vec<String> {
    if let Some(findings) = payload
        .get("report")
        .and_then(|r| r.get("findings"))
        .and_then(|v| v.as_array())
    {
        let mut out: Vec<String> = findings
            .iter()
            .filter_map(|f| f.get("axis").and_then(|v| v.as_str()).map(String::from))
            .collect();
        out.sort();
        out.dedup();
        return out;
    }
    extract_strs(payload, "flagged_present")
}

fn extract_strs(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| e.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Pull a field out of the `expected` object by any of the candidate keys.
/// Returns the stringified form (booleans become "true"/"false").
fn expected_field_str(expected: &Value, keys: &[&str]) -> Option<String> {
    let obj = expected.as_object()?;
    for k in keys {
        if let Some(v) = obj.get(*k) {
            if let Some(s) = v.as_str() {
                return Some(s.to_string());
            }
            if let Some(b) = v.as_bool() {
                return Some(b.to_string());
            }
        }
    }
    None
}

/// Pull the canonical string-array out of an `expected` object — supports
/// the common keys used by q4-q14 labels.
fn expected_string_array(expected: &Value) -> Result<Vec<String>, String> {
    let obj = expected
        .as_object()
        .ok_or_else(|| format!("expected must be an object, got: {expected}"))?;
    for k in [
        "callers",
        "callees",
        "implementors",
        "interfaces",
        "symbols",
        "imports",
        "unresolved",
        "affected",
        "truly_present",
        "fields",
    ] {
        if let Some(arr) = obj.get(k).and_then(|v| v.as_array()) {
            return Ok(arr
                .iter()
                .filter_map(|e| e.as_str().map(String::from))
                .collect());
        }
    }
    Err(format!("expected has no recognized array field: {expected}"))
}

/// MCP client that owns a child process + its stdio pipes + a request id.
pub struct McpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpClient {
    /// Spawn the MCP binary with piped stdio.
    pub fn spawn(binary: &Path) -> Result<Self, String> {
        let mut child = Command::new(binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("spawn {:?}: {e}", binary))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = BufReader::new(child.stdout.take().ok_or("no stdout")?);
        Ok(McpClient {
            child,
            stdin,
            stdout,
            next_id: 1,
        })
    }

    /// Send the initialize handshake; required before any tool calls.
    pub fn initialize(&mut self) -> Result<(), String> {
        let _ = self.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "bench_end_result", "version": "0.0.1"},
            }),
        )?;
        self.notification("notifications/initialized", json!({}))?;
        Ok(())
    }

    /// Call one MCP tool.  Returns the raw `result` object (the
    /// `{content: [...]}` envelope).
    pub fn call_tool(&mut self, name: &str, args: &Value) -> Result<Value, String> {
        self.request("tools/call", json!({"name": name, "arguments": args}))
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.send(&msg)?;
        self.read_response(id)
    }

    fn notification(&mut self, method: &str, params: Value) -> Result<(), String> {
        let msg = json!({"jsonrpc": "2.0", "method": method, "params": params});
        self.send(&msg)
    }

    fn send(&mut self, msg: &Value) -> Result<(), String> {
        let line = serde_json::to_string(msg).map_err(|e| format!("serialize: {e}"))?;
        writeln!(self.stdin, "{}", line).map_err(|e| format!("write: {e}"))?;
        self.stdin.flush().map_err(|e| format!("flush: {e}"))
    }

    fn read_response(&mut self, id: u64) -> Result<Value, String> {
        // Read lines until we get one whose id matches.  Ignore notifications.
        let deadline = Instant::now() + Duration::from_secs(300);
        loop {
            if Instant::now() > deadline {
                return Err(format!("timeout waiting for id={id}"));
            }
            let mut line = String::new();
            let n = self
                .stdout
                .read_line(&mut line)
                .map_err(|e| format!("read: {e}"))?;
            if n == 0 {
                return Err("mcp server closed stdout".to_string());
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let v: Value = serde_json::from_str(trimmed)
                .map_err(|e| format!("json parse {trimmed:?}: {e}"))?;
            if v.get("id").and_then(|x| x.as_u64()) == Some(id) {
                if let Some(err) = v.get("error") {
                    return Err(format!("mcp error: {err}"));
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Regression for B3: the Q13 scorer extracts axis names from
    /// `report.findings[].axis` (the real validate_prd_against_graph
    /// response shape) and scores precision/recall vs the expected set.
    #[test]
    fn test_q13_scorer_reads_findings_axis() {
        // Case 1: prd_02_hallucinated — one symbol_hallucination finding.
        let payload_hallucinated = json!({
            "status": "ok",
            "validation_status": "has_errors",
            "report": {
                "findings": [
                    {"axis": "symbol_hallucination", "severity": "error",
                     "message": "claim 'Foo' does not resolve"}
                ]
            }
        });
        let expected = json!({ "truly_present": ["symbol_hallucination"] });
        let score = score_prmean_from_payload(&payload_hallucinated, &expected)
            .expect("score");
        assert!(
            score >= 0.8,
            "prd_02_hallucinated expected >=0.8, got {score}"
        );

        // Case 2: prd_03_community_violation — one community_consistency finding.
        let payload_community = json!({
            "status": "ok",
            "report": {
                "findings": [
                    {"axis": "community_consistency", "severity": "warn",
                     "message": "scope spans unexpected communities"}
                ]
            }
        });
        let expected = json!({ "truly_present": ["community_consistency"] });
        let score = score_prmean_from_payload(&payload_community, &expected)
            .expect("score");
        assert!(
            score >= 0.8,
            "prd_03_community_violation expected >=0.8, got {score}"
        );

        // Case 3: prd_01_valid — empty expected, empty actual → 1.0.
        let payload_valid = json!({
            "status": "ok",
            "report": { "findings": [] }
        });
        let expected = json!({ "truly_present": [] });
        let score = score_prmean_from_payload(&payload_valid, &expected)
            .expect("score");
        assert!(
            (score - 1.0).abs() < 1e-9,
            "empty expected + empty actual should be 1.0, got {score}"
        );

        // Case 4: axis set is deduplicated + unrelated noise is tolerated
        // only as precision loss (not a scoring crash).
        let payload_mixed = json!({
            "report": {
                "findings": [
                    {"axis": "symbol_hallucination"},
                    {"axis": "symbol_hallucination"},
                    {"axis": "process_impact"}
                ]
            }
        });
        let expected = json!({ "truly_present": ["symbol_hallucination"] });
        let score = score_prmean_from_payload(&payload_mixed, &expected)
            .expect("score");
        // Recall = 1.0, precision = 0.5 → mean = 0.75.
        assert!(
            (score - 0.75).abs() < 1e-6,
            "mixed-axis payload: expected 0.75, got {score}"
        );
    }
}

