// lsp_client — lightweight LSP client over stdio JSON-RPC 2.0.
//
// Spawns a language server process, sends initialize/didOpen/definition
// requests, and parses responses. Uses Content-Length header framing
// per the LSP specification (not newline-delimited).
//
// source: LSP Specification 3.17 — https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub struct LspClient {
    process: Child,
    reader: BufReader<std::process::ChildStdout>,
    request_id: AtomicI64,
    timeout: Duration,
}

pub struct DefinitionResult {
    pub uri: String,
    pub start_line: u64,
    #[allow(dead_code)] // available for column-precise matching in future
    pub start_col: u64,
}

pub struct LspResolutionResult {
    pub resolved_count: u64,
    pub failed_count: u64,
    pub skipped_count: u64,
    pub elapsed_ms: u64,
}

// ---------------------------------------------------------------------------
// LSP command auto-detection
// ---------------------------------------------------------------------------

pub fn detect_lsp_command(language: &str) -> Option<(&'static str, &'static [&'static str])> {
    match language {
        "rust" => Some(("rust-analyzer", &[])),
        "python" => Some(("pyright-langserver", &["--stdio"])),
        "typescript" => Some(("typescript-language-server", &["--stdio"])),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// LSP command allowlist — security-critical.
//
// source: C3 fix — `lsp_command` is caller-controlled (MCP tool argument)
// and is passed to `Command::new`. Without an allowlist this is pure RCE:
// `lsp_command: "rm"` or `lsp_command: "/tmp/evil"` would execute anything.
//
// Only bare binary names from a fixed list may be used. No absolute paths
// (anything containing `/`), no relative paths with `..`, no shell metachars.
// ---------------------------------------------------------------------------

/// Names approved for caller-provided `lsp_command` overrides.
/// Callers may NOT pass absolute paths; they must rely on PATH lookup.
pub const LSP_COMMAND_ALLOWLIST: &[&str] = &[
    "rust-analyzer",
    "pyright",
    "pyright-langserver",
    "typescript-language-server",
];

/// Validates that `cmd` is an allowed bare binary name. Returns Err with
/// the reason code expected by callers on rejection.
pub fn validate_lsp_command(cmd: &str) -> Result<(), String> {
    if cmd.contains('/') || cmd.contains('\\') {
        return Err(format!(
            "lsp_command_not_allowed: path separators are forbidden (got {cmd:?})"
        ));
    }
    if !LSP_COMMAND_ALLOWLIST.contains(&cmd) {
        return Err(format!(
            "lsp_command_not_allowed: {cmd:?} not in allowlist {:?}",
            LSP_COMMAND_ALLOWLIST
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// file:// URI construction with percent-encoding.
// source: RFC 3986 §2.3 — unreserved chars are ALPHA / DIGIT / `-` / `.` / `_` / `~`.
// Path separator `/` is preserved since it is reserved for path structure.
// Everything else is percent-encoded per byte (UTF-8).
// ---------------------------------------------------------------------------

/// Converts an absolute filesystem path to a `file://` URI, percent-encoding
/// all bytes that are not RFC 3986 unreserved or `/`.
pub fn path_to_file_uri(path: &Path) -> String {
    let s = path.display().to_string();
    let mut out = String::from("file://");
    for b in s.as_bytes() {
        let c = *b;
        let keep = c.is_ascii_alphanumeric()
            || matches!(c, b'-' | b'.' | b'_' | b'~' | b'/');
        if keep {
            out.push(c as char);
        } else {
            out.push('%');
            out.push_str(&format!("{c:02X}"));
        }
    }
    out
}

pub fn is_command_available(command: &str) -> bool {
    Command::new("which")
        .arg(command)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Content-Length framing — LSP wire protocol
// source: LSP spec §Base Protocol
// ---------------------------------------------------------------------------

fn write_lsp_message(stdin: &mut std::process::ChildStdin, msg: &[u8]) -> Result<(), String> {
    let header = format!("Content-Length: {}\r\n\r\n", msg.len());
    stdin.write_all(header.as_bytes())
        .map_err(|e| format!("write header: {e}"))?;
    stdin.write_all(msg)
        .map_err(|e| format!("write body: {e}"))?;
    stdin.flush()
        .map_err(|e| format!("flush: {e}"))?;
    Ok(())
}

fn read_lsp_message(reader: &mut BufReader<std::process::ChildStdout>, timeout: Duration) -> Result<Value, String> {
    let deadline = Instant::now() + timeout;

    // Read headers until empty line. If the server closes stdout (EOF)
    // before we see a Content-Length header, callers must be able to tell
    // that apart from a real protocol message — we surface it via
    // `eof_before_header` so the probe path can map to `lsp_probe_failed`.
    let mut content_length: Option<usize> = None;
    let mut saw_any_byte = false;
    loop {
        if Instant::now() > deadline {
            return Err("timeout reading LSP header".to_string());
        }
        let mut line = String::new();
        let n = reader.read_line(&mut line)
            .map_err(|e| format!("read header line: {e}"))?;
        if n == 0 {
            // EOF — child closed stdout.
            if saw_any_byte {
                return Err("eof_before_body: partial header then EOF".to_string());
            }
            return Err("eof_before_header: child stdout closed without LSP framing".to_string());
        }
        saw_any_byte = true;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
            content_length = rest.trim().parse::<usize>().ok();
        }
    }

    let len = content_length.ok_or("missing Content-Length header")?;
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)
        .map_err(|e| format!("read body ({len} bytes): {e}"))?;

    serde_json::from_slice(&body)
        .map_err(|e| format!("parse JSON body: {e}"))
}

// ---------------------------------------------------------------------------
// LspClient implementation
// ---------------------------------------------------------------------------

impl LspClient {
    /// Spawn an LSP server process and return a client handle.
    ///
    /// SECURITY: `command` must be an allowlisted bare binary name — see
    /// `validate_lsp_command`. Callers that accept user input MUST validate
    /// before calling this; `start` double-checks in case it's bypassed.
    pub fn start(
        command: &str,
        args: &[&str],
        workspace_root: &Path,
        timeout: Duration,
    ) -> Result<Self, String> {
        // Defense-in-depth — reject anything not on the allowlist, even if
        // the caller forgot to validate upstream.
        validate_lsp_command(command)?;
        Self::start_unchecked(command, args, workspace_root, timeout)
    }

    /// Internal spawn that skips the allowlist. Only reachable from `start()`
    /// (which validates) and from unit tests that need to exercise the probe
    /// logic against a fake binary living outside the allowlist.
    ///
    /// `pub(crate)` — NOT reachable from MCP argument plumbing. All external
    /// call paths still go through `start()` → `validate_lsp_command`.
    #[doc(hidden)]
    pub(crate) fn start_unchecked(
        command: &str,
        args: &[&str],
        workspace_root: &Path,
        timeout: Duration,
    ) -> Result<Self, String> {
        let mut child = Command::new(command)
            .args(args)
            .current_dir(workspace_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("spawn {command}: {e}"))?;

        let stdout = child.stdout.take()
            .ok_or("failed to capture LSP stdout")?;
        let reader = BufReader::new(stdout);

        Ok(LspClient {
            process: child,
            reader,
            request_id: AtomicI64::new(1),
            timeout,
        })
    }

    /// Send `initialize` and `initialized` to the LSP server.
    ///
    /// This is the **probing handshake**: the first response is read under a
    /// short `probe_timeout` (separate from the tool's main timeout) so that
    /// binaries on PATH that aren't real LSP servers — rustup proxies,
    /// shell stubs, `/bin/true` — fail fast with `lsp_probe_failed` instead of
    /// the cryptic `missing Content-Length header` the old code surfaced.
    ///
    /// source: C-correctness bug 1 — graceful-fallback claim was false when
    /// the binary existed but didn't speak LSP.
    pub fn initialize(&mut self, workspace_root: &Path) -> Result<(), String> {
        self.initialize_with_probe(workspace_root, Duration::from_secs(2))
    }

    /// Initialize with an explicit probe timeout for the first response.
    /// Exposed so callers (and tests) can tune the probe window.
    pub fn initialize_with_probe(
        &mut self,
        workspace_root: &Path,
        probe_timeout: Duration,
    ) -> Result<(), String> {
        // source: M2 fix — percent-encode the path so spaces, unicode, and
        // URL-reserved chars in workspace paths don't produce a malformed URI.
        let root_uri = path_to_file_uri(workspace_root);
        let id = self.next_id();

        let init_req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": {
                    "textDocument": {
                        "definition": { "dynamicRegistration": false }
                    }
                },
                "workspaceFolders": [{
                    "uri": root_uri,
                    "name": workspace_root.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default()
                }]
            }
        });

        self.send_request(&init_req)
            .map_err(classify_probe_err)?;

        // Read the first response under probe_timeout. Any failure here —
        // EOF, timeout, parse error, missing jsonrpc field — is reclassified
        // as `lsp_probe_failed` so callers can distinguish "not on PATH"
        // from "on PATH but not an LSP server".
        let resp = self.read_initialize_response(id, probe_timeout)?;
        validate_probe_response(&resp)?;

        // Send initialized notification (no id, no response expected)
        let notif = json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        });
        self.send_notification(&notif)
            .map_err(classify_probe_err)?;

        Ok(())
    }

    fn read_initialize_response(&mut self, id: i64, probe_timeout: Duration) -> Result<Value, String> {
        let deadline = Instant::now() + probe_timeout;
        loop {
            let remaining = deadline.checked_duration_since(Instant::now())
                .unwrap_or(Duration::ZERO);
            if remaining.is_zero() {
                return Err(format!(
                    "lsp_probe_failed: no response within probe timeout ({}ms)",
                    probe_timeout.as_millis()
                ));
            }
            let msg = read_lsp_message(&mut self.reader, remaining)
                .map_err(classify_probe_err)?;
            if msg.get("id").and_then(|v| v.as_i64()) == Some(id) {
                return Ok(msg);
            }
            // notifications / other ids — keep reading within the window.
        }
    }

    /// Notify the server about an open document.
    pub fn did_open(&mut self, file_uri: &str, language_id: &str, text: &str) -> Result<(), String> {
        let notif = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": file_uri,
                    "languageId": language_id,
                    "version": 1,
                    "text": text
                }
            }
        });
        self.send_notification(&notif)
    }

    /// Query textDocument/definition at a specific position.
    pub fn get_definition(
        &mut self,
        file_uri: &str,
        line: u64,
        col: u64,
    ) -> Result<Option<DefinitionResult>, String> {
        let id = self.next_id();
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "textDocument/definition",
            "params": {
                "textDocument": { "uri": file_uri },
                "position": { "line": line, "character": col }
            }
        });

        self.send_request(&req)?;
        let resp = self.read_response_for_id(id)?;

        parse_definition_response(&resp)
    }

    /// Gracefully shut down the LSP server.
    pub fn shutdown(mut self) -> Result<(), String> {
        let id = self.next_id();
        let req = json!({ "jsonrpc": "2.0", "id": id, "method": "shutdown", "params": null });
        if self.send_request(&req).is_ok() {
            let _ = self.read_response_for_id(id);
        }
        let notif = json!({ "jsonrpc": "2.0", "method": "exit", "params": null });
        let _ = self.send_notification(&notif);
        let _ = self.process.wait();
        Ok(())
    }

    // -- private helpers --

    fn next_id(&self) -> i64 {
        self.request_id.fetch_add(1, Ordering::SeqCst)
    }

    fn send_request(&mut self, msg: &Value) -> Result<(), String> {
        let bytes = serde_json::to_vec(msg)
            .map_err(|e| format!("serialize request: {e}"))?;
        let stdin = self.process.stdin.as_mut()
            .ok_or("LSP stdin unavailable")?;
        write_lsp_message(stdin, &bytes)
    }

    fn send_notification(&mut self, msg: &Value) -> Result<(), String> {
        self.send_request(msg)
    }

    /// Read messages until we find one with the matching id.
    /// Discards notifications and other responses along the way.
    fn read_response_for_id(&mut self, target_id: i64) -> Result<Value, String> {
        let deadline = Instant::now() + self.timeout;
        loop {
            let remaining = deadline.checked_duration_since(Instant::now())
                .unwrap_or(Duration::ZERO);
            if remaining.is_zero() {
                return Err(format!("timeout waiting for response id={target_id}"));
            }
            let msg = read_lsp_message(&mut self.reader, remaining)?;
            // Check if this is our response
            if let Some(id) = msg.get("id") {
                if id.as_i64() == Some(target_id) {
                    return Ok(msg);
                }
            }
            // Otherwise it's a notification or different response — skip
        }
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

// ---------------------------------------------------------------------------
// Probe classification — correctness bug 1
// ---------------------------------------------------------------------------

/// Rewrites any low-level framing/parse/timeout error raised during the
/// handshake into an `lsp_probe_failed:` error so the resolver and MCP
/// response layer can report a distinct reason code. If the message is
/// already an `lsp_probe_failed:` string, it is returned unchanged.
fn classify_probe_err(e: String) -> String {
    if e.starts_with("lsp_probe_failed") {
        return e;
    }
    let reason = if e.contains("eof_before_header") {
        "found on PATH but didn't respond as an LSP server (stdout closed immediately; likely a stub, proxy, or non-LSP binary)"
    } else if e.contains("eof_before_body") {
        "found on PATH but dropped the connection mid-header (partial LSP framing then EOF)"
    } else if e.contains("missing Content-Length") {
        "found on PATH but sent non-LSP output (no Content-Length header)"
    } else if e.contains("timeout reading LSP header") || e.contains("no response within probe timeout") {
        "found on PATH but didn't respond within the probe window (not an LSP server or hung)"
    } else if e.contains("parse JSON body") {
        "found on PATH but sent non-JSON-RPC bytes as the first frame"
    } else {
        return format!("lsp_probe_failed: {e}");
    };
    format!("lsp_probe_failed: {reason} — underlying: {e}")
}

/// Validates the first message claims to be JSON-RPC 2.0 with a `result` or
/// `error` field. Anything else is a probe failure.
fn validate_probe_response(resp: &Value) -> Result<(), String> {
    let jsonrpc = resp.get("jsonrpc").and_then(|v| v.as_str());
    if jsonrpc != Some("2.0") {
        return Err(format!(
            "lsp_probe_failed: first response missing jsonrpc=\"2.0\" field (got {:?})",
            jsonrpc
        ));
    }
    if resp.get("result").is_none() && resp.get("error").is_none() {
        return Err(
            "lsp_probe_failed: first response has neither `result` nor `error`".to_string()
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Response parsing
// ---------------------------------------------------------------------------

fn parse_definition_response(resp: &Value) -> Result<Option<DefinitionResult>, String> {
    if let Some(err) = resp.get("error") {
        return Err(format!("LSP error: {err}"));
    }
    let result = match resp.get("result") {
        Some(Value::Null) | None => return Ok(None),
        Some(r) => r,
    };

    // LSP returns either a single Location, an array of Locations,
    // or an array of LocationLinks. Handle all cases.
    let location = if result.is_array() {
        let arr = result.as_array().unwrap();
        if arr.is_empty() { return Ok(None); }
        &arr[0]
    } else if result.is_object() {
        result
    } else {
        return Ok(None);
    };

    // LocationLink has targetUri + targetRange; Location has uri + range
    let uri = location.get("targetUri")
        .or_else(|| location.get("uri"))
        .and_then(|v| v.as_str());
    let range = location.get("targetRange")
        .or_else(|| location.get("range"));

    match (uri, range) {
        (Some(u), Some(r)) => {
            let start = r.get("start").ok_or("missing range.start")?;
            let line = start.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
            let col = start.get("character").and_then(|v| v.as_u64()).unwrap_or(0);
            Ok(Some(DefinitionResult {
                uri: u.to_string(),
                start_line: line,
                start_col: col,
            }))
        }
        _ => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_lsp_command() {
        assert!(detect_lsp_command("rust").is_some());
        assert!(detect_lsp_command("python").is_some());
        assert!(detect_lsp_command("typescript").is_some());
        assert!(detect_lsp_command("cobol").is_none());
    }

    #[test]
    fn test_parse_definition_single_location() {
        let resp = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "uri": "file:///src/main.rs",
                "range": {
                    "start": { "line": 10, "character": 4 },
                    "end": { "line": 10, "character": 20 }
                }
            }
        });
        let def = parse_definition_response(&resp).unwrap().unwrap();
        assert_eq!(def.uri, "file:///src/main.rs");
        assert_eq!(def.start_line, 10);
        assert_eq!(def.start_col, 4);
    }

    #[test]
    fn test_parse_definition_array() {
        let resp = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": [{
                "uri": "file:///src/lib.rs",
                "range": {
                    "start": { "line": 5, "character": 0 },
                    "end": { "line": 5, "character": 10 }
                }
            }]
        });
        let def = parse_definition_response(&resp).unwrap().unwrap();
        assert_eq!(def.uri, "file:///src/lib.rs");
        assert_eq!(def.start_line, 5);
    }

    #[test]
    fn test_parse_definition_null_result() {
        let resp = json!({ "jsonrpc": "2.0", "id": 3, "result": null });
        assert!(parse_definition_response(&resp).unwrap().is_none());
    }

    #[test]
    fn test_parse_definition_empty_array() {
        let resp = json!({ "jsonrpc": "2.0", "id": 4, "result": [] });
        assert!(parse_definition_response(&resp).unwrap().is_none());
    }

    #[test]
    fn test_parse_definition_location_link() {
        let resp = json!({
            "jsonrpc": "2.0",
            "id": 5,
            "result": [{
                "targetUri": "file:///src/graph_store.rs",
                "targetRange": {
                    "start": { "line": 20, "character": 0 },
                    "end": { "line": 30, "character": 1 }
                },
                "targetSelectionRange": {
                    "start": { "line": 20, "character": 11 },
                    "end": { "line": 20, "character": 21 }
                }
            }]
        });
        let def = parse_definition_response(&resp).unwrap().unwrap();
        assert_eq!(def.uri, "file:///src/graph_store.rs");
        assert_eq!(def.start_line, 20);
    }

    #[test]
    fn test_lsp_command_allowlist() {
        // source: C3 fix — only bare names from LSP_COMMAND_ALLOWLIST are OK.
        // Arbitrary binaries, absolute paths, and relative paths are rejected.

        // Rejected: bare command NOT in allowlist.
        let err = validate_lsp_command("rm").expect_err("rm must be rejected");
        assert!(err.contains("lsp_command_not_allowed"), "got: {err}");

        // Rejected: absolute path (contains '/').
        let err = validate_lsp_command("/tmp/evil")
            .expect_err("absolute path must be rejected");
        assert!(err.contains("lsp_command_not_allowed"), "got: {err}");

        // Rejected: relative path with separator.
        let err = validate_lsp_command("./evil")
            .expect_err("relative path must be rejected");
        assert!(err.contains("lsp_command_not_allowed"), "got: {err}");

        // Rejected: path with backslash (Windows-style).
        let err = validate_lsp_command("evil\\bin")
            .expect_err("backslash path must be rejected");
        assert!(err.contains("lsp_command_not_allowed"), "got: {err}");

        // Accepted: allowlisted bare names.
        assert!(validate_lsp_command("rust-analyzer").is_ok());
        assert!(validate_lsp_command("pyright").is_ok());
        assert!(validate_lsp_command("pyright-langserver").is_ok());
        assert!(validate_lsp_command("typescript-language-server").is_ok());
    }

    #[test]
    fn test_path_to_file_uri_percent_encodes() {
        // source: M2 fix — spaces and unicode must be percent-encoded.
        let uri = path_to_file_uri(Path::new("/tmp/a b/c"));
        assert_eq!(uri, "file:///tmp/a%20b/c");
        // `/` and unreserved chars are preserved.
        let uri = path_to_file_uri(Path::new("/Users/x/foo-bar_baz.ts"));
        assert_eq!(uri, "file:///Users/x/foo-bar_baz.ts");
        // `?` and `#` must be encoded (URL-reserved).
        let uri = path_to_file_uri(Path::new("/tmp/a?b#c"));
        assert_eq!(uri, "file:///tmp/a%3Fb%23c");
    }

    #[test]
    fn test_parse_definition_error() {
        let resp = json!({
            "jsonrpc": "2.0",
            "id": 6,
            "error": { "code": -32600, "message": "invalid request" }
        });
        assert!(parse_definition_response(&resp).is_err());
    }

    #[test]
    fn test_lsp_graceful_fallback_on_fake_lsp() {
        // source: C-correctness bug 1 — a binary that passes the PATH check
        // but exits silently (rustup proxy, stub, /bin/true) used to surface
        // the cryptic "missing Content-Length header". It must now be
        // classified as `lsp_probe_failed` so the MCP layer can report a
        // clean error instead of a protocol-level mystery.
        //
        // We bypass the allowlist via `start_unchecked` because the point of
        // this test is the probe logic, not the upstream command validator.
        let tmp = std::env::temp_dir();
        // `/usr/bin/true` exists on macOS and Linux; exits 0, writes nothing.
        // Use whichever `true` is available — all POSIX systems have one.
        let true_bin = if Path::new("/usr/bin/true").exists() {
            "/usr/bin/true"
        } else if Path::new("/bin/true").exists() {
            "/bin/true"
        } else {
            panic!("no `true` binary available for test");
        };

        let client = LspClient::start_unchecked(
            true_bin,
            &[],
            &tmp,
            Duration::from_secs(5),
        );
        let mut client = client.expect("spawn /bin/true should succeed");

        let err = client
            .initialize_with_probe(&tmp, Duration::from_secs(2))
            .expect_err("probe against /bin/true must fail");

        assert!(
            err.starts_with("lsp_probe_failed"),
            "expected lsp_probe_failed prefix, got: {err}"
        );
    }

    #[test]
    fn test_classify_probe_err_eof() {
        let classified = classify_probe_err(
            "eof_before_header: child stdout closed without LSP framing".to_string()
        );
        assert!(classified.starts_with("lsp_probe_failed:"));
        assert!(classified.contains("stub, proxy, or non-LSP binary"));
    }

    #[test]
    fn test_classify_probe_err_passthrough() {
        let already = "lsp_probe_failed: already classified".to_string();
        assert_eq!(classify_probe_err(already.clone()), already);
    }

    #[test]
    fn test_validate_probe_response_rejects_non_jsonrpc() {
        let bad = json!({ "id": 1, "result": {} });
        assert!(validate_probe_response(&bad).is_err());
        let bad2 = json!({ "jsonrpc": "2.0", "id": 1 });
        assert!(validate_probe_response(&bad2).is_err());
        let good = json!({ "jsonrpc": "2.0", "id": 1, "result": {} });
        assert!(validate_probe_response(&good).is_ok());
    }
}
