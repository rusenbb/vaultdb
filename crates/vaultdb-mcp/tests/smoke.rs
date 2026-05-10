//! End-to-end smoke test for the MCP server.
//!
//! Spawns the release-built `vaultdb-mcp` binary against a temp vault,
//! drives the MCP handshake (`initialize` + `notifications/initialized`),
//! lists tools, and round-trips a real `tools/call` for each major tool
//! family (read, graph, plan-only mutation). Catches regressions in the
//! tool router wiring, the params shape, and the stdio framing — none
//! of which the unit tests can cover.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use tempfile::TempDir;

const SERVER_BIN: &str = env!("CARGO_BIN_EXE_vaultdb-mcp");

/// Set up a tiny vault with two notes, one of which links to the other.
fn fixture_vault() -> TempDir {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join(".obsidian")).unwrap();
    std::fs::create_dir(dir.path().join("notes")).unwrap();
    std::fs::write(
        dir.path().join("notes/alpha.md"),
        "---\nstatus: active\ntags:\n  - topic/ai\n---\nLinks to [[beta]] and [[gamma]].\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("notes/beta.md"),
        "---\nstatus: draft\ntags:\n  - topic/ai\n---\nA pure body.\n",
    )
    .unwrap();
    dir
}

/// Spawn the binary, drive the handshake, send each request line, then
/// read N response lines. Returns the response lines in order.
fn run_session(vault_root: &Path, requests: &[&str]) -> Vec<String> {
    let mut child = Command::new(SERVER_BIN)
        .arg("--vault")
        .arg(vault_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn vaultdb-mcp");

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();

    // initialize + notifications/initialized + every test request
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"smoke","version":"0.1"}}}"#;
    let initialized = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;

    writeln!(stdin, "{}", init).unwrap();
    writeln!(stdin, "{}", initialized).unwrap();
    for req in requests {
        writeln!(stdin, "{}", req).unwrap();
    }
    drop(stdin); // close stdin so server exits when done

    let mut reader = BufReader::new(stdout);
    let mut lines = Vec::new();
    let mut buf = String::new();
    // Read up to (initialize-response + one per request) lines, with a
    // small wall-clock cap for safety.
    let want = 1 + requests.len();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while lines.len() < want && std::time::Instant::now() < deadline {
        buf.clear();
        match reader.read_line(&mut buf) {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = buf.trim().to_string();
                if !trimmed.is_empty() {
                    lines.push(trimmed);
                }
            }
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    lines
}

fn pick_response(lines: &[String], id: i64) -> serde_json::Value {
    for line in lines {
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("id").and_then(|x| x.as_i64()) == Some(id) {
            return v;
        }
    }
    panic!(
        "no response with id={} found among {} lines: {:?}",
        id,
        lines.len(),
        lines
    );
}

#[test]
fn lists_all_expected_tools() {
    let vault = fixture_vault();
    let lines = run_session(
        vault.path(),
        &[r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#],
    );
    let resp = pick_response(&lines, 2);
    let tools = resp["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();

    for expected in [
        "ping",
        "query",
        "find_by_name",
        "list_folders",
        "links",
        "traverse",
        "unresolved",
        "schema_show",
        "schema_infer",
        "plan_update",
        "plan_delete",
        "plan_move",
        "plan_rename",
    ] {
        assert!(
            names.contains(&expected),
            "expected tool '{}' missing from {:?}",
            expected,
            names
        );
    }
}

#[test]
fn ping_returns_pong() {
    let vault = fixture_vault();
    let lines = run_session(
        vault.path(),
        &[
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"ping","arguments":{}}}"#,
        ],
    );
    let resp = pick_response(&lines, 2);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert_eq!(text, "pong");
}

#[test]
fn query_filters_by_status() {
    let vault = fixture_vault();
    let req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"query","arguments":{"folder":"notes","where":"status = active"}}}"#;
    let lines = run_session(vault.path(), &[req]);
    let resp = pick_response(&lines, 2);
    let body = resp["result"]["content"][0]["text"].as_str().unwrap();
    let records: serde_json::Value = serde_json::from_str(body).unwrap();
    let arr = records.as_array().expect("records is an array");
    assert_eq!(arr.len(), 1, "only alpha is active");
    let path = arr[0]["path"].as_str().unwrap();
    assert!(
        path.ends_with("alpha.md"),
        "expected alpha.md, got {}",
        path
    );
}

#[test]
fn links_returns_outgoing_targets() {
    let vault = fixture_vault();
    let req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"links","arguments":{"name":"alpha"}}}"#;
    let lines = run_session(vault.path(), &[req]);
    let resp = pick_response(&lines, 2);
    let body = resp["result"]["content"][0]["text"].as_str().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(body).unwrap();
    let outgoing: Vec<&str> = parsed["outgoing"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(outgoing.contains(&"beta"));
    assert!(outgoing.contains(&"gamma"));
}

#[test]
fn plan_update_describes_change_without_writing() {
    let vault = fixture_vault();
    let beta_path = vault.path().join("notes/beta.md");
    let before = std::fs::read_to_string(&beta_path).unwrap();

    let req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"plan_update","arguments":{"folder":"notes","where":"status = draft","set":["status=published"]}}}"#;
    let lines = run_session(vault.path(), &[req]);
    let resp = pick_response(&lines, 2);
    let body = resp["result"]["content"][0]["text"].as_str().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(body).unwrap();
    let changes = parsed["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 1);
    let desc = changes[0]["description"].as_str().unwrap();
    assert!(
        desc.contains("status") && desc.contains("published"),
        "description should mention the field and value, got: {}",
        desc
    );

    // Crucial: plan_update must NOT have written.
    let after = std::fs::read_to_string(&beta_path).unwrap();
    assert_eq!(before, after, "plan_update wrote to disk!");
}
