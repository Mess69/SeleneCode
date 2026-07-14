#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The server, driven through a **real client** over the SDK's in-memory transport.
//!
//! # The `isError` discipline is the point of this file
//!
//! The spike found three outcomes, and the trap is that the naive one is wrong:
//! `Err(ErrorData)` becomes a JSON-RPC **-32603 transport failure**, not `isError: true`. So
//! every recoverable condition must come back **success-shaped**, and these tests drive the
//! real handlers to prove it — because one `isError` early and an agent abandons the tool for
//! the whole session.

use rmcp::model::CallToolRequestParams;
use rmcp::{RoleClient, ServiceExt, service::RunningService};
use selene_mcp::SeleneMcp;

async fn pair(root: Option<std::path::PathBuf>) -> RunningService<RoleClient, ()> {
    let (server_io, client_io) = tokio::io::duplex(8192);
    tokio::spawn(async move {
        if let Ok(s) = SeleneMcp::new(root).serve(server_io).await {
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
        .expect("the CALL must succeed even when the answer is 'no'");
    let text = r
        .content
        .first()
        .and_then(|c| c.as_text())
        .map(|t| t.text.clone())
        .unwrap_or_default();
    (r.is_error == Some(true), text)
}

/// **The handshake answers, and the tools list, with NO index anywhere** (#964, #172).
#[tokio::test(flavor = "multi_thread")]
async fn tools_list_at_a_root_that_was_never_indexed() {
    let client = pair(Some(std::path::PathBuf::from("/nonexistent-project"))).await;

    let info = client.peer_info().expect("initialize answered");
    assert!(
        info.instructions
            .as_deref()
            .unwrap()
            .contains("selene_explore")
    );

    let tools = client.list_tools(Default::default()).await.unwrap();
    let names: Vec<&str> = tools.tools.iter().map(|t| t.name.as_ref()).collect();
    assert_eq!(
        names,
        vec!["explore"],
        "ONE door. An agent facing seven tools composes a four-call research plan; the bet is \
         that explore answers in one."
    );

    client.cancel().await.ok();
}

/// **THE ISERROR DISCIPLINE.** A not-indexed project is guidance — `isError: false`.
#[tokio::test(flavor = "multi_thread")]
async fn a_not_indexed_project_is_success_shaped_guidance() {
    let client = pair(Some(std::path::PathBuf::from("/nonexistent-project"))).await;

    let (is_error, text) = call(
        &client,
        "explore",
        serde_json::json!({ "query": "anything" }),
    )
    .await;

    assert!(
        !is_error,
        "'not indexed' is the most common FIRST CONTACT an agent has with this tool. An \
         isError here and it never calls us again — it goes back to Read, and every call \
         after that is one we never see."
    );
    assert!(
        text.contains("selene index"),
        "…and the guidance says what to do: {text}"
    );

    client.cancel().await.ok();
}

/// A real project: the whole answer, in one call, success-shaped.
#[tokio::test(flavor = "multi_thread")]
async fn explore_answers_from_a_real_index_in_one_call() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    std::fs::create_dir_all(root.join("src")).unwrap();
    // THREE files, so the call graph holds a THREE-node chain: handleLogin → login → hashPassword.
    //
    // It used to be two (`handleLogin → login`, and `login` returned `u.length`), and this test
    // had been **failing on HEAD** because of it. Not a flake, and not something the Flow section
    // got wrong: two nodes joined by one edge is not a flow, and `flow.rs` says so in as many
    // words (`MIN_FLOW_NODES = 3` — *"a 2-node flow is just an edge — it tells the agent nothing
    // it did not ask"*). The fixture was asking the builder to draw a spine the graph did not
    // contain, and the builder was right to decline. What this test means to prove is that the
    // **MCP surface** renders a real, proven chain end-to-end — so give it a graph that has one.
    std::fs::write(root.join("src/app.ts"), "import { login } from './service';\nexport function handleLogin(u: string) {\n  return login(u);\n}\n").unwrap();
    std::fs::write(
        root.join("src/service.ts"),
        "import { hashPassword } from './crypto';\nexport function login(u: string) {\n  return hashPassword(u);\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/crypto.ts"),
        "export function hashPassword(u: string) {\n  return u.length;\n}\n",
    )
    .unwrap();

    // Index it with the REAL pipeline, exactly as `selene index` does.
    let dir = root.join(".selene");
    std::fs::create_dir_all(&dir).unwrap();
    let store = selene_db::SurrealStore::open(&dir).await.unwrap();
    store.apply_schema().await.unwrap();
    let indexer = selene_extract::Indexer::new(root.clone(), store);
    indexer.index_all(None).await;
    let store = indexer.into_store();
    selene_resolve::resolve_and_persist_batched(&store, &root, None)
        .await
        .unwrap();
    drop(store);

    let client = pair(Some(root)).await;
    let (is_error, text) = call(
        &client,
        "explore",
        serde_json::json!({ "query": "handleLogin login" }),
    )
    .await;

    assert!(!is_error);
    assert!(text.contains("handleLogin"), "the symbol: {text}");
    assert!(
        text.contains("### Flow"),
        "THE FLOW — the thing the agent reads instead of the code:\n{text}"
    );
    assert!(
        text.lines()
            .any(|l| l.starts_with(char::is_numeric) && l.contains('\t')),
        "…and the VERBATIM NUMBERED SOURCE. Without it the agent knows where the code is and \
         not what it does, so it opens the file:\n{text}"
    );

    client.cancel().await.ok();
}
