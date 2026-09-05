// Regression for dy-wcet: analyze_codebase discarded IndexResult.coverage,
// unlike index_codebase. Exercise the public stdio contract, including a
// second full analysis so a stale sidecar cannot masquerade as fresh coverage.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_ai-architect-mcp-codebase");

/// A live MCP server subprocess, driven over its real JSON-RPC stdio wire —
/// no library-function shortcuts. Killed on drop so a panicking assertion
/// never leaks a hung process.
struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Server {
    fn spawn() -> Self {
        let mut child = Command::new(BIN)
            .args(["--profile", "full"])
            .env_remove("AP_PROFILE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn ai-architect-mcp-codebase");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = BufReader::new(child.stdout.take().expect("child stdout"));
        Server {
            child,
            stdin,
            stdout,
        }
    }

    /// Sends one JSON-RPC request line and reads back exactly one response
    /// line (the server's stdio loop is one-request-per-line, source:
    /// `src/main.rs`'s `for line in handle.lines()` dispatch).
    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{req}").expect("write request line");
        self.stdin.flush().expect("flush stdin");
        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .expect("read response line");
        assert!(
            !line.is_empty(),
            "server closed stdout before responding to {method}"
        );
        serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("response line is not valid JSON ({e}): {line:?}"))
    }

    /// Calls an MCP tool and unwraps the double-encoded result: the
    /// JSON-RPC envelope's `result.content[0].text` is itself a JSON string
    /// (the tool's actual response), per `handle_tool_call` in `main.rs`.
    fn call_tool(&mut self, id: i64, name: &str, arguments: Value) -> Value {
        let resp = self.request(
            id,
            "tools/call",
            json!({"name": name, "arguments": arguments}),
        );
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("tool response has no content[0].text: {resp:?}"));
        serde_json::from_str(text)
            .unwrap_or_else(|e| panic!("tool response text is not valid JSON ({e}): {text}"))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn analyze(
    server: &mut Server,
    repo: &std::path::Path,
    out: &std::path::Path,
    excluded: bool,
) -> Value {
    server.call_tool(
        1,
        "analyze_codebase",
        json!({
            "path": repo, "output_dir": out,
            "exclude_dirs": if excluded { vec!["secrets"] } else { vec![] },
            "dependency_scope": "none"
        }),
    )
}

fn missed(server: &mut Server, out: &std::path::Path) -> Value {
    server.call_tool(
        2,
        "query_graph",
        json!({
            "graph_path": out.join("graph"), "graph": "missed"
        }),
    )
}

#[test]
fn analyze_persists_and_refreshes_coverage_over_stdio() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let repo = tmp.path().join("repo");
    let out = tmp.path().join("out");
    std::fs::create_dir_all(repo.join("secrets")).unwrap();
    std::fs::write(repo.join("main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(repo.join("secrets/key.rs"), "fn key() {}\n").unwrap();
    let mut server = Server::spawn();
    let first = analyze(&mut server, &repo, &out, true);
    assert_eq!(first["status"], "ok", "{first}");
    let first_missed = missed(&mut server, &out);
    assert!(
        out.join("index_coverage.json").is_file(),
        "analyze must persist coverage"
    );
    assert_eq!(
        first["coverage"]["skipped"]["user_excluded_count"], 1,
        "{first}"
    );
    assert_eq!(
        first_missed["coverage"], first["coverage"],
        "{first_missed}"
    );
    let saved: Value =
        serde_json::from_slice(&std::fs::read(out.join("index_coverage.json")).unwrap()).unwrap();
    assert_eq!(
        saved["files"]["secrets"]["detail"], "user_excluded",
        "{saved}"
    );

    let second = analyze(&mut server, &repo, &out, false);
    assert_eq!(second["status"], "ok", "{second}");
    assert_eq!(second["coverage"]["skipped"]["count"], 0, "{second}");
    assert_eq!(missed(&mut server, &out)["coverage"], second["coverage"]);
    let refreshed: Value =
        serde_json::from_slice(&std::fs::read(out.join("index_coverage.json")).unwrap()).unwrap();
    assert!(
        refreshed["files"].as_object().unwrap().is_empty(),
        "{refreshed}"
    );
}

#[test]
fn analyze_surfaces_a_coverage_save_failure_over_stdio() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let repo = tmp.path().join("repo");
    let out = tmp.path().join("out");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("main.rs"), "fn main() {}\n").unwrap();
    // A directory where the atomic writer needs a file deterministically
    // fails even for a privileged test process; no permission assumptions.
    std::fs::create_dir_all(out.join("index_coverage.json.tmp")).unwrap();
    let response = analyze(&mut Server::spawn(), &repo, &out, false);
    assert_eq!(response["status"], "error", "{response}");
    assert!(
        response["message"]
            .as_str()
            .unwrap()
            .contains("coverage: write"),
        "{response}"
    );
}
