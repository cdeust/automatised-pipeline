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

    // Read headers until empty line
    let mut content_length: Option<usize> = None;
    loop {
        if Instant::now() > deadline {
            return Err("timeout reading LSP header".to_string());
        }
        let mut line = String::new();
        reader.read_line(&mut line)
            .map_err(|e| format!("read header line: {e}"))?;
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
    pub fn start(
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
    pub fn initialize(&mut self, workspace_root: &Path) -> Result<(), String> {
        let root_uri = format!("file://{}", workspace_root.display());
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

        self.send_request(&init_req)?;
        // Read initialize response (may include server capabilities)
        self.read_response_for_id(id)?;

        // Send initialized notification (no id, no response expected)
        let notif = json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        });
        self.send_notification(&notif)?;

        Ok(())
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
    fn test_parse_definition_error() {
        let resp = json!({
            "jsonrpc": "2.0",
            "id": 6,
            "error": { "code": -32600, "message": "invalid request" }
        });
        assert!(parse_definition_response(&resp).is_err());
    }
}
