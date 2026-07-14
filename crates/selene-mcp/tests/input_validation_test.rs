#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 19 — input caps, driven through the **real server** over the SDK transport.
//!
//! A malformed argument is the one recoverable-looking condition that is *correctly* `isError:
//! true`: unlike "not found", there is no query for the agent to reformulate — the argument itself
//! is wrong. These prove the cap fires, sets the flag, and does so *before* the graph is touched
//! (the server is pointed at a project that was never indexed, so any handler that ran would return
//! not-indexed guidance — `isError: false` — which is the opposite of what we assert).

use rmcp::model::CallToolRequestParams;
use rmcp::{RoleClient, ServiceExt, service::RunningService};
use selene_mcp::SeleneMcp;

async fn pair() -> RunningService<RoleClient, ()> {
    let (server_io, client_io) = tokio::io::duplex(1 << 20);
    tokio::spawn(async move {
        // A never-indexed root: if validation did NOT fire, the handler would return not-indexed
        // guidance (isError:false), so an isError:true here can ONLY be the cap.
        if let Ok(s) = SeleneMcp::new(Some("/nonexistent-project".into()))
            .serve(server_io)
            .await
        {
            let _ = s.waiting().await;
        }
    });
    ().serve(client_io).await.expect("handshake")
}

async fn call(
    client: &RunningService<RoleClient, ()>,
    tool: &str,
    args: serde_json::Value,
) -> (bool, String) {
    let mut p = CallToolRequestParams::default();
    p.name = tool.to_string().into();
    p.arguments = args.as_object().cloned();
    let r = client
        .call_tool(p)
        .await
        .expect("the CALL itself must always succeed");
    let text = r
        .content
        .first()
        .and_then(|c| c.as_text())
        .map(|t| t.text.clone())
        .unwrap_or_default();
    (r.is_error == Some(true), text)
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_query_is_a_reserved_is_error() {
    let client = pair().await;
    let (is_error, text) = call(&client, "explore", serde_json::json!({ "query": "" })).await;
    assert!(
        is_error,
        "an empty query is malformed input, not a recoverable condition: {text}"
    );
    assert!(
        text.contains("must be a non-empty string"),
        "the message names the fix: {text}"
    );
    client.cancel().await.ok();
}

// NOTE: the `symbol`-shaped tools (`node`, `callers`, …) are NOT exposed by default — `explore` is
// the only door (see `tools::DEFAULT_VISIBLE`), so calling `node` here fails at the transport, not
// the validation. Enabling them means the process-global `SELENE_MCP_TOOLS` env var, which is racy
// under the concurrent test runner. The `symbol` path is covered by `validate`'s unit tests, and
// its wiring (`check_symbol`) calls the same `free_form` as `check_query`, which these exercise
// end-to-end through the real server.

#[tokio::test(flavor = "multi_thread")]
async fn ten_thousand_and_one_bytes_is_rejected_ten_thousand_is_not() {
    let client = pair().await;

    // 10 001: rejected as too large.
    let (is_error, text) = call(
        &client,
        "explore",
        serde_json::json!({ "query": "x".repeat(10_001) }),
    )
    .await;
    assert!(
        is_error,
        "10 001 bytes must be rejected: {}",
        &text[..text.len().min(120)]
    );
    assert!(text.contains("maximum is 10000"), "{text}");

    // 10 000: accepted — it reaches the handler, which returns not-indexed GUIDANCE (isError:false).
    // The point is that the cap did NOT fire: the boundary is inclusive.
    let (is_error, _) = call(
        &client,
        "explore",
        serde_json::json!({ "query": "x".repeat(10_000) }),
    )
    .await;
    assert!(
        !is_error,
        "10 000 bytes is within the cap; the handler runs and guides, isError:false"
    );

    client.cancel().await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_over_long_project_path_is_rejected() {
    let client = pair().await;
    let (is_error, text) = call(
        &client,
        "explore",
        serde_json::json!({ "query": "ok", "projectPath": "p".repeat(4_097) }),
    )
    .await;
    assert!(
        is_error,
        "a 4 097-byte path is rejected before the graph is opened: {}",
        &text[..text.len().min(120)]
    );
    assert!(text.contains("projectPath is 4097 bytes"), "{text}");
    client.cancel().await.ok();
}
