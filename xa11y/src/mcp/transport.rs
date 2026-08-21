//! Newline-delimited JSON-RPC framing for the MCP stdio transport.
//!
//! The [stdio binding] is deliberately minimal: one JSON-RPC message per
//! line, UTF-8, no embedded newlines, and nothing on stdout that is not a
//! protocol message. This module owns that framing and nothing else, so the
//! whole of it is exercised by unit tests over `&[u8]` — no display, no
//! accessibility backend, no subprocess.
//!
//! [stdio binding]: https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio

use std::io::{BufRead, Write};

use serde_json::Value;

use crate::cli::{CliError, CliResult};

/// Wrap an I/O failure on the transport as a CLI-level error.
///
/// A broken pipe or a non-UTF-8 line is fatal for the session: the framing
/// is no longer trustworthy, and continuing would mean guessing where the
/// next message starts.
fn io_err(context: &str, e: std::io::Error) -> CliError {
    CliError::Xa11y(crate::Error::Platform {
        code: e.raw_os_error().unwrap_or(-1) as i64,
        message: format!("mcp stdio transport: {context}: {e}"),
    })
}

/// Read one message from `input`.
///
/// Returns `Ok(None)` at end of input, which is the client's graceful
/// shutdown signal — the spec asks servers to exit promptly when stdin
/// closes, and that is the only portable shutdown mechanism.
///
/// Blank lines are skipped rather than parsed. They are not legal messages,
/// but they are the one malformation that carries no information, and a
/// client that emits a stray newline should not take the session down.
pub(crate) fn read_message<R: BufRead>(input: &mut R) -> CliResult<Option<Value>> {
    let mut line = String::new();
    loop {
        line.clear();
        let n = input
            .read_line(&mut line)
            .map_err(|e| io_err("read stdin", e))?;
        if n == 0 {
            return Ok(None);
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // A parse failure is reported to the caller, which answers with a
        // JSON-RPC -32700 and keeps the session alive: the framing is still
        // intact (we consumed exactly one line), only this message is bad.
        return match serde_json::from_str::<Value>(trimmed) {
            Ok(v) => Ok(Some(v)),
            Err(e) => Err(CliError::Usage(format!("invalid JSON on stdin: {e}"))),
        };
    }
}

/// Write one message to `output`, followed by a newline, and flush.
///
/// `serde_json::to_string` never emits a raw newline inside a JSON scalar
/// (control characters are escaped), so the "no embedded newlines" rule holds
/// by construction. The flush is not optional: the client is blocked on
/// reading this line, and a buffered response is a hung session.
pub(crate) fn write_message<W: Write>(output: &mut W, message: &Value) -> CliResult<()> {
    let encoded = serde_json::to_string(message).map_err(|e| {
        CliError::Xa11y(crate::Error::Platform {
            code: -1,
            message: format!("mcp stdio transport: encode response: {e}"),
        })
    })?;
    debug_assert!(
        !encoded.contains('\n'),
        "serialized MCP message must not contain a raw newline"
    );
    output
        .write_all(encoded.as_bytes())
        .and_then(|()| output.write_all(b"\n"))
        .and_then(|()| output.flush())
        .map_err(|e| io_err("write stdout", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn read_all(input: &str) -> Vec<CliResult<Option<Value>>> {
        let mut cursor = std::io::Cursor::new(input.as_bytes());
        let mut out = Vec::new();
        loop {
            let msg = read_message(&mut cursor);
            let done = matches!(msg, Ok(None));
            out.push(msg);
            if done {
                break;
            }
        }
        out
    }

    #[test]
    fn reads_one_message_per_line() {
        let msgs = read_all("{\"a\":1}\n{\"b\":2}\n");
        assert_eq!(msgs.len(), 3, "two messages then EOF");
        assert_eq!(msgs[0].as_ref().unwrap().as_ref().unwrap()["a"], json!(1));
        assert_eq!(msgs[1].as_ref().unwrap().as_ref().unwrap()["b"], json!(2));
        assert!(msgs[2].as_ref().unwrap().is_none(), "EOF is Ok(None)");
    }

    #[test]
    fn eof_without_trailing_newline_still_yields_the_message() {
        let msgs = read_all("{\"a\":1}");
        assert_eq!(msgs[0].as_ref().unwrap().as_ref().unwrap()["a"], json!(1));
        assert!(msgs[1].as_ref().unwrap().is_none());
    }

    #[test]
    fn blank_lines_are_skipped_not_fatal() {
        let msgs = read_all("\n\n{\"a\":1}\n");
        assert_eq!(msgs[0].as_ref().unwrap().as_ref().unwrap()["a"], json!(1));
    }

    #[test]
    fn malformed_json_is_an_error_not_a_silent_skip() {
        let mut cursor = std::io::Cursor::new(&b"not json\n"[..]);
        let err = read_message(&mut cursor).expect_err("malformed line must error");
        assert!(matches!(err, CliError::Usage(_)), "got: {err:?}");
    }

    #[test]
    fn malformed_line_leaves_framing_intact() {
        // The bad line is consumed whole, so the next read starts at the next
        // message rather than mid-line. This is what lets the server answer
        // -32700 and keep serving.
        let mut cursor = std::io::Cursor::new(&b"not json\n{\"a\":1}\n"[..]);
        assert!(read_message(&mut cursor).is_err());
        let next = read_message(&mut cursor).unwrap().unwrap();
        assert_eq!(next["a"], json!(1));
    }

    #[test]
    fn writes_exactly_one_newline_terminated_line() {
        let mut out = Vec::new();
        write_message(&mut out, &json!({"jsonrpc": "2.0", "id": 1})).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.ends_with('\n'));
        assert_eq!(text.matches('\n').count(), 1, "no embedded newlines");
    }

    #[test]
    fn embedded_newlines_in_payloads_are_escaped() {
        let mut out = Vec::new();
        write_message(&mut out, &json!({"text": "line one\nline two"})).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.matches('\n').count(), 1, "payload newline must escape");
        assert!(text.contains("\\n"));
    }
}
