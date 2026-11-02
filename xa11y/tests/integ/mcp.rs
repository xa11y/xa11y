//! `xa11y mcp` driven end to end against the AccessKit test app.
//!
//! The raw-JSON MCP suite (`tests/suites/cli/test_mcp.py`) runs against every
//! other test app but never against AccessKit (`declared_suite_skips` in
//! `tests/harness/launch.py`), so a fixture that only the AccessKit app has
//! is reached from here instead: the test spawns the real `xa11y` binary,
//! speaks JSON-RPC to it over stdio, and points it at the test app's pid.

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

    use serde_json::{json, Value};

    use crate::integ as h;

    /// One `xa11y mcp` process on the handshake era.
    struct Session {
        child: Child,
        stdin: ChildStdin,
        stdout: BufReader<ChildStdout>,
    }

    impl Session {
        fn start() -> Self {
            let mut child = Command::new(env!("CARGO_BIN_EXE_xa11y"))
                .arg("mcp")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .expect("spawn `xa11y mcp`");
            let stdin = child.stdin.take().expect("piped stdin");
            let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
            let mut session = Session {
                child,
                stdin,
                stdout,
            };
            session.request(
                1,
                "initialize",
                json!({"protocolVersion": "2025-06-18", "capabilities": {},
                       "clientInfo": {"name": "integ_test"}}),
            );
            session.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
            session
        }

        fn send(&mut self, message: &Value) {
            writeln!(self.stdin, "{message}").expect("write to `xa11y mcp`");
            self.stdin.flush().expect("flush `xa11y mcp` stdin");
        }

        /// Send a request and return its response, skipping notifications.
        fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
            self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
            loop {
                let mut line = String::new();
                let n = self
                    .stdout
                    .read_line(&mut line)
                    .expect("read from `xa11y mcp`");
                assert!(n > 0, "`xa11y mcp` closed stdout before answering {method}");
                let message: Value = serde_json::from_str(&line)
                    .unwrap_or_else(|e| panic!("non-JSON line on the wire ({e}): {line}"));
                if message.get("id") == Some(&json!(id)) {
                    return message;
                }
            }
        }

        fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
            let response = self.request(
                99,
                "tools/call",
                json!({"name": name, "arguments": arguments}),
            );
            response
                .get("result")
                .cloned()
                .unwrap_or_else(|| panic!("tools/call {name} returned no result: {response}"))
        }
    }

    impl Drop for Session {
        fn drop(&mut self) {
            // Best-effort teardown of a test child: a kill that fails because
            // the process already exited leaves nothing to clean up.
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// The test app's ~4 KiB label comes back as a bounded prefix that names
    /// its cut. Selectors still match the full text, which is how the label
    /// is found.
    #[test]
    #[ignore]
    fn mcp_find_caps_a_document_sized_label_and_says_so() {
        let pid = h::app_root().pid.expect("the test app reports a pid");
        let mut mcp = Session::start();
        let result = mcp.call_tool(
            "find",
            json!({"pid": pid, "selector": "[name^=\"Long text:\"], [value^=\"Long text:\"]"}),
        );
        assert_eq!(result["isError"], json!(false), "{result}");
        let matches = result["structuredContent"]["matches"]
            .as_array()
            .unwrap_or_else(|| panic!("find returned no matches array: {result}"));
        let label = matches
            .first()
            .unwrap_or_else(|| panic!("the long label did not match: {result}"));

        let mut cut_fields = Vec::new();
        for field in ["name", "value", "description"] {
            let Some(text) = label.get(field).and_then(Value::as_str) else {
                continue;
            };
            assert!(text.len() <= 1024, "{field} is {} bytes", text.len());
            if label.get(format!("{field}_truncated")) == Some(&json!(true)) {
                let original = label[format!("{field}_bytes")]
                    .as_u64()
                    .unwrap_or_else(|| panic!("{field}_bytes missing: {label}"));
                assert!(original >= 4096, "{field}_bytes = {original}");
                assert!(text.starts_with("Long text: "), "{field}: {text:.40}");
                cut_fields.push(field);
            } else {
                assert!(
                    label.get(format!("{field}_bytes")).is_none(),
                    "an uncut {field} must not carry a byte count: {label}"
                );
            }
        }
        assert!(
            !cut_fields.is_empty(),
            "the long text must be reported as cut: {label}"
        );
    }
}
