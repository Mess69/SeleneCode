//! The control protocol — how a CLI command reaches a running daemon.
//!
//! The daemon owns the exclusive RocksDB lock, so `selene sync` cannot just open the store while a
//! daemon is up. Instead it opens the daemon's socket and sends **one control line**; the daemon
//! runs the operation against its own warm store and replies with **one line**. Because the daemon
//! writes through the very handle its query cache serves from, the re-index is visible to the next
//! tool call immediately.
//!
//! # Framing: how the daemon tells control from MCP
//!
//! Both a control client and an MCP proxy connect to the same socket. The daemon reads the first
//! newline-delimited line of every connection: if it parses as a [`ControlRequest`] it is handled
//! here and the connection closes; otherwise that first line was the MCP `initialize` request and
//! is replayed into the MCP session (see `serve.rs`). Control frames carry a `"selene_control"`
//! key that a JSON-RPC message never has, so the two are unambiguous.

use serde::{Deserialize, Serialize};

/// A control request — the first (and only) line a control client sends.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlRequest {
    /// The verb. `"sync"` is the only one so far.
    pub selene_control: String,
}

/// The daemon's one-line reply to a control request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlReply {
    pub ok: bool,
    #[serde(default)]
    pub changed: usize,
    #[serde(default)]
    pub removed: usize,
    #[serde(default)]
    pub unchanged: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ControlReply {
    pub fn failure(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            changed: 0,
            removed: 0,
            unchanged: 0,
            error: Some(msg.into()),
        }
    }
}

/// Try to parse a first line as a control request. `None` means "this is not control" — i.e. it is
/// an MCP message and must be replayed into a session.
pub fn parse_request(line: &str) -> Option<ControlRequest> {
    let req: ControlRequest = serde_json::from_str(line.trim()).ok()?;
    // A JSON-RPC message could in theory deserialize into a struct with a missing field defaulting;
    // require the marker field to be non-empty so only a genuine control frame matches.
    if req.selene_control.is_empty() {
        None
    } else {
        Some(req)
    }
}

// --- client side (used by the CLI) --------------------------------------------------------------

use std::path::Path;

use super::lock;
use super::proc::is_alive;

/// If a live, version-matched daemon holds `root`, send it `verb` and return its reply. `Ok(None)`
/// means there is no daemon to route to — the caller should do the operation directly. `Ok(Some(_))`
/// carries the daemon's reply (which may itself be a failure the caller should surface).
pub async fn route_to_daemon(root: &Path, verb: &str) -> std::io::Result<Option<ControlReply>> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    let pid_path = super::paths::pid_path(root);
    let Some(rec) = lock::read(&pid_path) else {
        return Ok(None); // no daemon
    };
    if !is_alive(rec.pid) || rec.version != env!("CARGO_PKG_VERSION") {
        return Ok(None); // dead, or a different version we must not command
    }

    let sock = super::paths::socket_path(root);
    let stream = match UnixStream::connect(&sock).await {
        Ok(s) => s,
        Err(_) => return Ok(None), // pidfile says alive but socket is gone — treat as no daemon
    };

    let (rh, mut wh) = stream.into_split();
    let req = ControlRequest {
        selene_control: verb.to_string(),
    };
    let mut line = serde_json::to_string(&req).unwrap_or_default();
    line.push('\n');
    wh.write_all(line.as_bytes()).await?;
    wh.flush().await?;

    let mut reader = BufReader::new(rh);
    let mut resp = String::new();
    reader.read_line(&mut resp).await?;
    match serde_json::from_str::<ControlReply>(resp.trim()) {
        Ok(reply) => Ok(Some(reply)),
        Err(e) => Ok(Some(ControlReply::failure(format!(
            "daemon reply unparseable: {e}"
        )))),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_control_frame_parses_and_an_mcp_message_does_not() {
        assert_eq!(
            parse_request(r#"{"selene_control":"sync"}"#)
                .unwrap()
                .selene_control,
            "sync"
        );
        // A JSON-RPC initialize is not a control frame.
        assert!(parse_request(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#).is_none());
        // An empty marker is rejected.
        assert!(parse_request(r#"{"selene_control":""}"#).is_none());
        assert!(parse_request("not json").is_none());
    }

    #[test]
    fn reply_round_trips() {
        let r = ControlReply {
            ok: true,
            changed: 2,
            removed: 1,
            unchanged: 5,
            error: None,
        };
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<ControlReply>(&s).unwrap(), r);
    }
}
