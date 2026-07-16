#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The daemon, end to end, against the **real binary** — the only test that proves it.
//!
//! `selene serve --mcp` is driven as an MCP server through a genuine rmcp client over a spawned
//! child process. That child is the *launcher*: it spawns a detached daemon, proxies this session
//! to it, and the daemon answers from its warm store. So a passing `call_tool` here exercises the
//! whole chain — election, socket bind, proxy pump, warm store — not a mock of any of it.
//!
//! Every daemon in this file gets a **short idle timeout** via `SELENE_DAEMON_IDLE_TIMEOUT_MS` so it
//! reaps itself seconds after the test, and a private `HOME`/project under a tempdir so it never
//! touches the developer's real registry or a real project.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::TokioChildProcess;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_selene")
}

/// Index a tiny two-file project so there is a real graph for the daemon to serve.
fn index_tiny_project(root: &Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/a.ts"),
        "export function alpha(){ return beta() }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/b.ts"),
        "export function beta(){ return 1 }\n",
    )
    .unwrap();
    let status = std::process::Command::new(bin())
        .arg("index")
        .arg(root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "indexing the fixture must succeed");
}

/// Spawn `selene serve --mcp --path <root>` as an MCP server and return a connected client.
async fn connect(root: &Path, home: &Path) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    let mut cmd = tokio::process::Command::new(bin());
    cmd.arg("serve")
        .arg("--mcp")
        .arg("--path")
        .arg(root)
        // Reap the daemon a few seconds after the test's last client disconnects.
        .env("SELENE_DAEMON_IDLE_TIMEOUT_MS", "3000")
        // Isolate the registry under a private HOME.
        .env("HOME", home)
        .stderr(Stdio::null());
    ().serve(TokioChildProcess::new(cmd).unwrap())
        .await
        .expect("MCP handshake through the proxy")
}

/// Like [`connect`], but exposes the full tool set (not just `explore`) so a test can call
/// `node`/`callers` to verify individual symbols and edges.
async fn connect_all_tools(
    root: &Path,
    home: &Path,
) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    let mut cmd = tokio::process::Command::new(bin());
    cmd.arg("serve")
        .arg("--mcp")
        .arg("--path")
        .arg(root)
        .env("SELENE_DAEMON_IDLE_TIMEOUT_MS", "2000")
        .env(
            "SELENE_MCP_TOOLS",
            "explore,node,search,callers,callees,impact,files",
        )
        .env("HOME", home)
        .stderr(Stdio::null());
    ().serve(TokioChildProcess::new(cmd).unwrap())
        .await
        .expect("MCP handshake through the proxy")
}

/// Poll a tool call until its answer contains `needle` (auto-sync is debounced ~2s + a re-index).
async fn poll_until(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    tool: &str,
    args: serde_json::Value,
    needle: &str,
    tries: usize,
) -> bool {
    for _ in 0..tries {
        tokio::time::sleep(Duration::from_millis(400)).await;
        let (is_err, text) = call(client, tool, args.clone()).await;
        if !is_err && text.contains(needle) {
            return true;
        }
    }
    false
}

async fn call(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    tool: &str,
    args: serde_json::Value,
) -> (bool, String) {
    let mut p = CallToolRequestParams::default();
    p.name = tool.to_string().into();
    p.arguments = args.as_object().cloned();
    let r = client
        .call_tool(p)
        .await
        .expect("the call itself must succeed");
    let text = r
        .content
        .first()
        .and_then(|c| c.as_text())
        .map(|t| t.text.clone())
        .unwrap_or_default();
    (r.is_error == Some(true), text)
}

fn daemon_pid(root: &Path) -> Option<i32> {
    let text = std::fs::read_to_string(root.join(".selene/daemon.pid")).ok()?;
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()?
        .get("pid")?
        .as_i64()
        .map(|p| p as i32)
}

/// A tool call answers through the proxy, and a daemon is spawned to back it.
#[tokio::test(flavor = "multi_thread")]
async fn a_tool_call_is_answered_by_a_spawned_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let root = tmp.path();
    index_tiny_project(root);

    let client = connect(root, home.path()).await;

    // The tool list comes back, and `explore` is in it.
    let tools = client.list_tools(Default::default()).await.unwrap();
    assert!(
        tools.tools.iter().any(|t| t.name == "explore"),
        "explore must be listed"
    );

    // A real query answers from the warm store — not an isError, and it names the symbol.
    let (is_err, text) = call(&client, "explore", serde_json::json!({ "query": "beta" })).await;
    assert!(
        !is_err,
        "a found symbol is success-shaped, got isError; text: {text}"
    );
    assert!(
        text.contains("beta"),
        "the answer describes beta; got: {text}"
    );

    // The proxy spawned a daemon: the pidfile exists and names a plausible pid.
    let pid = daemon_pid(root).expect("a daemon pidfile exists after a proxied session");
    assert!(pid > 1, "a real daemon pid");

    let _ = client.cancel().await;
}

/// Two independent launchers share ONE daemon — the whole point of the design.
#[tokio::test(flavor = "multi_thread")]
async fn two_launchers_share_one_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let root = tmp.path();
    index_tiny_project(root);

    let client_a = connect(root, home.path()).await;
    let _ = client_a.list_tools(Default::default()).await.unwrap();
    let pid_a = daemon_pid(root).expect("first launcher brought up a daemon");

    // A second, independent launcher for the same project must reuse the same daemon.
    let client_b = connect(root, home.path()).await;
    let (is_err, _) = call(
        &client_b,
        "explore",
        serde_json::json!({ "query": "alpha" }),
    )
    .await;
    assert!(!is_err, "the second client is served too");
    let pid_b = daemon_pid(root).expect("daemon still up");

    assert_eq!(pid_a, pid_b, "both launchers share the one daemon, not two");

    let _ = client_a.cancel().await;
    let _ = client_b.cancel().await;
}

/// `selene sync` routes through a running daemon (which holds the exclusive lock) instead of
/// fighting it for the lock — and the re-index is instantly visible to a query on the warm store.
#[tokio::test(flavor = "multi_thread")]
async fn sync_routes_through_a_running_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let root = tmp.path();
    index_tiny_project(root);

    // Bring up the daemon by connecting a client; keep it connected so it stays live.
    let client = connect(root, home.path()).await;
    let _ = client.list_tools(Default::default()).await.unwrap();
    assert!(daemon_pid(root).is_some(), "a daemon is holding the lock");

    // Add a new symbol on disk; advance mtime so the sync notices it.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    std::fs::write(
        root.join("src/c.ts"),
        "export function gamma(){ return 2 }\n",
    )
    .unwrap();

    // `selene sync` must succeed WHILE the daemon holds the lock — i.e. route through it.
    let out = std::process::Command::new(bin())
        .arg("sync")
        .arg(root)
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "sync succeeds via the daemon, not a lock error"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("via daemon"),
        "sync was daemon-routed; stderr: {stderr}"
    );

    // The new symbol is queryable through the SAME warm daemon right away.
    let (is_err, text) = call(&client, "explore", serde_json::json!({ "query": "gamma" })).await;
    assert!(
        !is_err,
        "gamma resolves after a daemon-routed sync; text: {text}"
    );
    assert!(
        text.contains("gamma"),
        "the fresh symbol is in the warm store; got: {text}"
    );

    let _ = client.cancel().await;
}

/// The daemon auto-syncs on a file change — no manual `selene sync` needed, and the new symbol
/// resolves through the warm store.
#[tokio::test(flavor = "multi_thread")]
async fn daemon_auto_syncs_on_a_file_change() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let root = tmp.path();
    index_tiny_project(root);

    // Bring up the daemon (and its watcher) and keep it live.
    let client = connect(root, home.path()).await;
    let _ = client.list_tools(Default::default()).await.unwrap();

    // Add a brand-new symbol on disk. Do NOT run `selene sync` — the watcher must do it.
    tokio::time::sleep(Duration::from_millis(300)).await;
    std::fs::write(
        root.join("src/c.ts"),
        "export function gamma(){ return 2 }\n",
    )
    .unwrap();

    // Wait past the 2s debounce plus the re-index, polling the warm store for the new symbol.
    let mut found = false;
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let (is_err, text) =
            call(&client, "explore", serde_json::json!({ "query": "gamma" })).await;
        if !is_err && text.contains("gamma") {
            found = true;
            break;
        }
    }
    assert!(
        found,
        "the daemon auto-synced the new file and the symbol resolves"
    );

    let _ = client.cancel().await;
}

/// The full ClaudeCode-style loop, end to end: the MCP server is up, "the agent" writes a new
/// CLASS with METHODS that calls an existing symbol, MODIFIES it, then DELETES it — and every change
/// is reflected in the graph automatically (FileWatcher auto-sync), verified both through the live
/// MCP client AND, after the daemon stops, directly in the on-disk database.
#[tokio::test(flavor = "multi_thread")]
async fn e2e_new_class_methods_edits_and_deletes_auto_reflect_in_the_graph() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let root = tmp.path();
    index_tiny_project(root); // src/a.ts (alpha->beta), src/b.ts (beta)

    let client = connect_all_tools(root, home.path()).await;
    let _ = client.list_tools(Default::default()).await.unwrap();

    // --- 1. a NEW class with methods, one of which calls the existing `beta` ---------------------
    // Poll on the EDGE (`callers beta` gaining `create`), not on a `node` text match: the not-found
    // guidance echoes the queried symbol name, so a `node` poll would false-positive before the sync
    // lands. A caller only appears once the class, the method, AND the resolved call edge all exist —
    // so this single assertion proves the whole update round-tripped.
    tokio::time::sleep(Duration::from_millis(300)).await;
    std::fs::write(
        root.join("src/service.ts"),
        "export class UserService {\n  create() { return beta() }\n  remove() { return 0 }\n}\n",
    )
    .unwrap();
    assert!(
        poll_until(
            &client,
            "callers",
            serde_json::json!({ "symbol": "beta" }),
            "create",
            30
        )
        .await,
        "the new class's create() auto-appeared AS A CALLER of beta (class + method + call edge)"
    );
    // And the method's callee resolves back the other way.
    let (_e, callees) = call(
        &client,
        "callees",
        serde_json::json!({ "symbol": "create" }),
    )
    .await;
    assert!(
        callees.contains("beta"),
        "create()'s callee is beta: {callees}"
    );

    // --- 2. MODIFY the class: create() now calls a brand-new gamma() instead ---------------------
    tokio::time::sleep(Duration::from_millis(1100)).await; // advance mtime past the debounce window
    std::fs::write(
        root.join("src/gamma.ts"),
        "export function gamma(){ return 7 }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/service.ts"),
        "export class UserService {\n  create() { return gamma() }\n  remove() { return 0 }\n}\n",
    )
    .unwrap();
    assert!(
        poll_until(
            &client,
            "callers",
            serde_json::json!({ "symbol": "gamma" }),
            "create",
            30
        )
        .await,
        "after the edit, create() now calls gamma()"
    );

    // --- 3. DELETE the class file: its symbols leave the graph -----------------------------------
    tokio::time::sleep(Duration::from_millis(1100)).await;
    std::fs::remove_file(root.join("src/service.ts")).unwrap();
    // Give the watcher its debounce + re-index a beat; the authoritative absence check is step 4.
    tokio::time::sleep(Duration::from_millis(3500)).await;

    // --- 4. Confirm the FINAL state directly in the on-disk DB (daemon stopped first) ------------
    let _ = client.cancel().await;
    // Wait for the daemon to release the exclusive lock (idle timeout 2s), then open the store.
    tokio::time::sleep(Duration::from_millis(3500)).await;
    let store = selene_db::SurrealStore::open(&root.join(".selene"))
        .await
        .unwrap();
    assert!(
        !store.get_nodes_by_name("gamma").await.unwrap().is_empty(),
        "gamma (added by the edit) persisted in the database"
    );
    assert!(
        store
            .get_nodes_by_name("UserService")
            .await
            .unwrap()
            .is_empty(),
        "UserService is absent from the database after its file was deleted"
    );
    assert!(
        store.get_nodes_by_name("create").await.unwrap().is_empty(),
        "the method create() went with its class"
    );
}

/// `SELENE_NO_DAEMON=1` serves in-process and spawns NO daemon.
#[tokio::test(flavor = "multi_thread")]
async fn no_daemon_env_serves_in_process() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let root = tmp.path();
    index_tiny_project(root);

    let mut cmd = tokio::process::Command::new(bin());
    cmd.arg("serve")
        .arg("--mcp")
        .arg("--path")
        .arg(root)
        .env("SELENE_NO_DAEMON", "1")
        .env("HOME", home.path())
        .stderr(Stdio::null());
    let client = ().serve(TokioChildProcess::new(cmd).unwrap()).await.expect("handshake");

    let (is_err, text) = call(&client, "explore", serde_json::json!({ "query": "beta" })).await;
    assert!(!is_err, "in-process serve answers too; text: {text}");

    // Give any (erroneous) spawn a beat, then assert NO daemon pidfile was written.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        daemon_pid(root).is_none(),
        "SELENE_NO_DAEMON must not spawn a daemon"
    );

    let _ = client.cancel().await;
}
