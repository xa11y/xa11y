//! Model Context Protocol server over stdio.
//!
//! `xa11y mcp` serves the same operations as the CLI's other subcommands as
//! MCP tools, so an agent can read and drive accessibility trees through a
//! protocol its client already speaks. It is not meant to be run by hand: an
//! MCP client launches it as a subprocess and talks JSON-RPC over its
//! standard streams.
//!
//! This module is `#[doc(hidden)]` and not part of the public API. It is
//! compiled only with the default `cli` feature, so a library consumer that
//! opts out (`default-features = false`) does not build it.
//!
//! # Layout
//!
//! - [`transport`] — newline-delimited JSON-RPC framing.
//! - [`protocol`] — envelope handling, dual-era method routing, error mapping.
//! - [`tools`] — the tool table and the handlers that reach the provider.
//! - [`base64`] — image-content encoding.
//!
//! Only `tools` touches the accessibility layer, so everything else is
//! covered by ordinary unit tests on every platform.
//!
//! # stdout is the wire
//!
//! The stdio transport reserves stdout for protocol messages: a stray
//! `println!` anywhere reachable from a tool handler corrupts the session for
//! the client, usually as a confusing parse error on the far side. Nothing in
//! this module or in the CLI helpers it calls may print. Diagnostics go to
//! stderr, which the spec explicitly leaves free for logging. `cargo xtask
//! check` enforces this by scanning for print macros under `src/mcp/`.

mod base64;
mod protocol;
mod tools;
mod transport;

use std::io::{BufReader, IsTerminal};

use crate::cli::CliResult;

/// Serve MCP over stdin/stdout until the client closes the input stream.
///
/// Returns `Ok(())` on a clean end of input, which is the spec's graceful
/// shutdown: "servers SHOULD exit promptly when their standard input is
/// closed or reads return end-of-file".
pub(crate) fn serve(args: &[String]) -> CliResult<()> {
    protocol::check_no_args(args)?;

    // A one-line hint on stderr, where it is safe. Without it, someone who
    // runs `xa11y mcp` expecting a subcommand sees a process that appears to
    // hang. Suppressed when stdin is a pipe, which is how a real client runs
    // it — the spec lets clients ignore stderr, but there is no reason to
    // emit noise into their logs on every launch.
    if std::io::stdin().is_terminal() {
        eprintln!(
            "xa11y mcp: serving Model Context Protocol over stdio (JSON-RPC on \
             stdin/stdout, one message per line). This is meant to be launched \
             by an MCP client, not run interactively. Ctrl-D to exit."
        );
    }

    let stdin = std::io::stdin();
    let mut input = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut output = stdout.lock();

    serve_streams(&mut input, &mut output)
}

/// The session loop, over any pair of streams.
///
/// Split out from [`serve`] so the loop itself is testable against in-memory
/// buffers rather than a real subprocess.
fn serve_streams<R: std::io::BufRead, W: std::io::Write>(
    input: &mut R,
    output: &mut W,
) -> CliResult<()> {
    let mut session = protocol::Session::new(tools::Xa11yTools);
    loop {
        let message = match transport::read_message(input) {
            Ok(Some(message)) => message,
            // End of input: the client is shutting us down.
            Ok(None) => return Ok(()),
            // A line that did not parse costs one message, not the session:
            // the framing is line-based, so the next read is still aligned.
            Err(err) => {
                transport::write_message(output, &protocol::parse_error_response(&err))?;
                continue;
            }
        };
        if let Some(response) = session.handle(message) {
            transport::write_message(output, &response)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    /// Drive a whole session over in-memory streams and return the responses.
    fn exchange(requests: &[Value]) -> Vec<Value> {
        let input: String = requests
            .iter()
            .map(|r| format!("{}\n", serde_json::to_string(r).unwrap()))
            .collect();
        let mut reader = std::io::Cursor::new(input.into_bytes());
        let mut output: Vec<u8> = Vec::new();
        serve_streams(&mut reader, &mut output).expect("clean EOF");
        String::from_utf8(output)
            .expect("responses are UTF-8")
            .lines()
            .map(|line| serde_json::from_str(line).expect("each line is one JSON message"))
            .collect()
    }

    #[test]
    fn a_legacy_handshake_lists_the_tools() {
        let responses = exchange(&[
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "protocolVersion": "2025-06-18", "capabilities": {} },
            }),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        ]);
        assert_eq!(responses.len(), 2, "the notification must not be answered");
        assert_eq!(responses[0]["result"]["serverInfo"]["name"], "xa11y");
        let tools = responses[1]["result"]["tools"].as_array().unwrap();
        assert!(tools.iter().any(|t| t["name"] == "tree"));
    }

    #[test]
    fn a_modern_client_discovers_and_lists_without_a_handshake() {
        let responses = exchange(&[
            json!({
                "jsonrpc": "2.0", "id": "d1", "method": "server/discover",
                "params": { "_meta": {
                    "io.modelcontextprotocol/protocolVersion": protocol::MODERN_VERSION,
                } },
            }),
            json!({ "jsonrpc": "2.0", "id": "t1", "method": "tools/list" }),
        ]);
        assert_eq!(
            responses[0]["result"]["supportedVersions"][0],
            protocol::MODERN_VERSION
        );
        assert_eq!(responses[1]["result"]["resultType"], "complete");
    }

    #[test]
    fn a_malformed_line_costs_one_message_not_the_session() {
        let mut input = std::io::Cursor::new(
            b"{ this is not json\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n".to_vec(),
        );
        let mut output: Vec<u8> = Vec::new();
        serve_streams(&mut input, &mut output).expect("session survives a bad line");

        let lines: Vec<Value> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["error"]["code"], -32700);
        assert!(lines[0]["id"].is_null(), "an unparsable id is null");
        assert_eq!(lines[1]["result"], json!({}), "ping still answered");
    }

    #[test]
    fn every_response_is_exactly_one_line() {
        let mut input = std::io::Cursor::new(
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n".to_vec(),
        );
        let mut output: Vec<u8> = Vec::new();
        serve_streams(&mut input, &mut output).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert_eq!(
            text.matches('\n').count(),
            1,
            "the tool list, schemas and all, must be one line"
        );
    }

    #[test]
    fn empty_input_exits_cleanly() {
        let mut input = std::io::Cursor::new(Vec::new());
        let mut output: Vec<u8> = Vec::new();
        serve_streams(&mut input, &mut output).expect("EOF is a clean shutdown");
        assert!(output.is_empty());
    }

    #[test]
    fn the_subcommand_takes_no_arguments() {
        let err = serve(&["--verbose".to_string()]).expect_err("must reject");
        assert!(err.to_string().contains("takes no arguments"), "{err}");
    }
}
