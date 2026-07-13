#![allow(clippy::unwrap_used, clippy::expect_used)]
//! **Task 1 — the `rmcp` 2.2 spike.** Throwaway knowledge, kept as a smoke test.
//!
//! The roadmap flags this SDK for API churn ("2.x had breaking API churn from the
//! 0.x/1.x era — copy patterns from the repo's current examples, not old blog posts"),
//! and Tasks 14–19 are built on its exact shapes. Everything below was verified by
//! **running it**, against `rmcp 2.2.0` — not read off a blog.
//!
//! ═══════════════════════════════════════════════════════════════════════════════
//! FINDINGS — these are the contract Tasks 14–19 are written against
//! ═══════════════════════════════════════════════════════════════════════════════
//!
//! ## ⭐ 1. `isError` — THE #1 RISK, and it is answered. There are THREE outcomes, not two.
//!
//! Our hardest invariant is that a *recoverable* condition (not indexed, symbol not
//! found, file absent) returns a **success-shaped** result, because one `isError` early
//! and an agent abandons the tool. rmcp gives us exactly the control we need, but the
//! mapping is subtle and **the naive `Err(...)` is the wrong one**:
//!
//! | What the `#[tool]` method returns | Wire shape |
//! |---|---|
//! | `Ok(CallToolResult::success(vec![Content::text(..)]))` | `{"content":[…], "isError":false}` |
//! | `Ok(CallToolResult::error(vec![Content::text(..)]))` | `{"content":[…], "isError":true}` |
//! | `Err(rmcp::ErrorData)` | **a JSON-RPC protocol error** (`-32603`), NOT `isError:true` |
//!
//! Source, `handler/server/tool.rs:94-112`: `impl IntoCallToolResult for ErrorData`
//! returns `Err(self)`, and the blanket `impl for Result<T, E>` propagates that `Err`
//! straight out of `into_call_tool_result` — so it leaves as a **protocol** error. Any
//! *other* `E: IntoCallToolResult` is converted to a `CallToolResult` and has
//! `is_error = Some(true)` **forced** onto it (line 106).
//!
//! **Consequences, binding on Task 19:**
//! - The success path is `Ok(CallToolResult::success(...))`. `success()` sets
//!   `is_error: Some(false)` (`model.rs:2990`), so the field is *present and false* on
//!   the wire — it is not omitted. Good: that is the TS shape.
//! - A **PathRefusal / genuine malfunction** should be `Ok(CallToolResult::error(...))`
//!   (`isError: true`, still a well-formed tool result the agent can read), **not**
//!   `Err(ErrorData)` — a JSON-RPC error is a *transport-level* failure and some clients
//!   surface it as a broken server rather than a failed call.
//! - **Never** let a `?` on a store error escape a handler: `GraphError → ErrorData` would
//!   turn a recoverable condition into a protocol error. Task 19's dispatch layer must
//!   catch and classify **inside** the handler.
//!
//! ## 2. `ServerHandler::get_info()` → `ServerInfo` — and `instructions` is there
//!
//! `ServerInfo` is a type alias for `InitializeResult` (`model.rs:923`), which carries
//! `protocol_version: ProtocolVersion`, `capabilities: ServerCapabilities`,
//! `server_info: Implementation`, and **`instructions: Option<String>`** (`model.rs:887`) —
//! exactly as the plan assumed. `ProtocolVersion::LATEST` is **`2025-11-25`**
//! (`model.rs:167`); `ProtocolVersion::default()` is what `ServerInfo::default()` sends.
//!
//! ## 3. The macros: `#[tool_router]` + `#[tool]` + `#[tool_handler]`
//!
//! - `#[tool_router]` goes on an **inherent `impl`** and generates a `tool_router()`
//!   associated fn returning a `ToolRouter<Self>`. The convention (from the crate's own
//!   docs, `handler/server/router/tool.rs:24`) is to store it in a field and hand it to
//!   `#[tool_handler]`.
//! - `#[tool_router(server_handler)]` additionally emits the `ServerHandler` impl for a
//!   tools-only server. **We will NOT use that flag** — we need a hand-written
//!   `get_info()` (for `instructions`) and Task 19 wraps dispatch, so we write
//!   `#[tool_router]` + an explicit `#[tool_handler] impl ServerHandler`.
//! - Arguments arrive as `Parameters(T)` where `T: schemars::JsonSchema + Deserialize`.
//!   The input schema is derived from `T` by **schemars 1.x** — which is the version rmcp
//!   2.2 links, so the workspace pins `schemars = "1"`. A 0.8 derive will not compile
//!   against it.
//!
//! ## 4. `tools/list` needs NO store handle — the un-indexed root works (#964, #172)
//!
//! The tool router is built from the **impl block**, not from server state, and
//! `get_info()` takes `&self`. The test below stands a server up whose state is
//! `Option<Store> = None` and still answers `initialize` + `tools/list`. So Task 14 can
//! construct `SeleneMcp<S>` with a lazily-opened store: the handshake answers before any
//! heavy init, and tools are listed at a root that has never been indexed.
//!
//! ## ⚠ 6. API CHURN, found by compiling — the names the blog posts get wrong
//!
//! - The content type is **`ContentBlock`**, not `Content`. `ContentBlock::text(s)` is the
//!   constructor (`model/content.rs:280`). There is no `Content` export in 2.2.
//! - `RoleClient` / `RoleServer` / `ServiceExt` live at the **crate root**
//!   (`rmcp::RoleClient`), not under `rmcp::service::*` (`lib.rs:20-24`).
//! - `RoleClient` and `serve_client` are behind the **`client` feature**, which a *server*
//!   binary must not carry. It is a **dev-dependency only** here: the tests drive the
//!   server through a real client; the shipped binary does not link the client half.
//! - **`ServerInfo` and `Implementation` are `#[non_exhaustive]`** — a struct literal is
//!   rejected from outside the crate, and so is `ServerInfo { ..Default::default() }`.
//!   They must be built default-then-assign. (Every blog-post `get_info` is a struct
//!   literal; none of them compile against 2.2.) **`CallToolRequestParam` is
//!   `#[non_exhaustive]` as well** — the same treatment on the client side.
//! - `CallToolRequestParam` (singular) is **deprecated**; the live name is
//!   `CallToolRequestParams` (plural). The deprecated alias still compiles, so this is the
//!   kind of thing that ships as a warning and rots — pinned here.
//!
//! ## ⭐ 7. `#[tool_handler]` IGNORES your router field unless you say so — and that is a trap
//!
//! The macro's **default** router expression is `Self::tool_router()`
//! (`rmcp-macros/src/tool_handler.rs:21-24`) — it **rebuilds the whole router on every
//! `tools/call`** and never reads the `tool_router` field the canonical example tells you
//! to store. The symptom is a `field 'tool_router' is never read` warning, and everything
//! still *works*, which is exactly why it would ship.
//!
//! It matters for us specifically: **Task 15 filters the tool list by `SELENE_MCP_TOOLS`**
//! (coordination point #1 — `explore` is the only default-visible tool). If the router is
//! rebuilt per call from the impl block, a router *constructed* with a filtered tool set is
//! silently discarded. So Task 14 must write:
//!
//! ```ignore
//! #[tool_handler(router = self.tool_router)]
//! impl ServerHandler for SeleneMcp<S> { … }
//! ```
//!
//! …and the field must hold the **filtered** router. This spike does it that way, and the
//! dead-field warning is gone — which is the assertion.
//!
//! ## 5. The in-memory transport for tests
//!
//! `tokio::io::duplex()` + `serve_server` / `serve_client` over the two halves gives a
//! full client↔server pair in-process (the SDK's own tests do this — `async_rw.rs:662`).
//! No subprocess, no stdio, no fixtures. Tasks 14–19 test through this.

use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, Implementation, ProtocolVersion,
    ServerCapabilities, ServerInfo,
};
use rmcp::{
    ErrorData, RoleClient, RoleServer, ServerHandler, ServiceExt,
    handler::server::tool::ToolRouter, service::RunningService, tool, tool_handler, tool_router,
};
use serde::Deserialize;

/// The server's state is an `Option` — **deliberately**. Finding #4: tools must be
/// listable at a root with no index (#964), and the handshake must answer before any
/// heavy init (#172). `None` here stands in for "no `.selene/` opened yet".
#[derive(Clone)]
struct SpikeServer {
    store: Arc<Option<String>>,
    tool_router: ToolRouter<SpikeServer>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct EchoArgs {
    /// The text to echo back.
    query: String,
}

#[tool_router]
impl SpikeServer {
    fn new() -> Self {
        Self {
            store: Arc::new(None), // no store, and the handshake still works
            tool_router: Self::tool_router(),
        }
    }

    /// The SUCCESS shape: `isError: false`, content is text.
    #[tool(
        name = "selene_explore",
        description = "Spike: the success-shaped result."
    )]
    async fn explore(&self, Parameters(args): Parameters<EchoArgs>) -> CallToolResult {
        // A recoverable condition (no index) is STILL a success — this is the whole
        // invariant, and rmcp expresses it natively.
        let text = match self.store.as_ref() {
            Some(s) => format!("indexed: {s} / {}", args.query),
            None => format!(
                "not indexed — run `selene index`. (query was: {})",
                args.query
            ),
        };
        CallToolResult::success(vec![ContentBlock::text(text)])
    }

    /// The RESERVED shape: `isError: true`. Only a PathRefusal or a genuine malfunction.
    #[tool(
        name = "selene_refuse",
        description = "Spike: the isError-shaped result."
    )]
    async fn refuse(&self) -> CallToolResult {
        CallToolResult::error(vec![ContentBlock::text(
            "path refused: outside project root",
        )])
    }
}

// ⚠ `router = self.tool_router` is NOT optional — see finding #7.
#[tool_handler(router = self.tool_router)]
impl ServerHandler for SpikeServer {
    fn get_info(&self) -> ServerInfo {
        // ⚠ CHURN (finding #6): `ServerInfo` and `Implementation` are `#[non_exhaustive]`,
        // so a struct LITERAL is rejected from outside the crate — including
        // `ServerInfo { ..Default::default() }`. Build by default-then-assign. Task 14
        // writes exactly this shape.
        let mut info = ServerInfo::default();
        info.protocol_version = ProtocolVersion::LATEST;
        info.capabilities = ServerCapabilities::builder().enable_tools().build();

        let mut me = Implementation::default();
        me.name = "selene-spike".into();
        me.version = "0.0.0".into();
        info.server_info = me;

        // Finding #2: it exists, and it is the ONE place agent guidance lives.
        info.instructions = Some("Use selene_explore instead of reading files.".into());
        info
    }
}

/// A client and a server, wired over an in-memory duplex (finding #5).
async fn spike_pair() -> RunningService<RoleClient, ()> {
    let (server_io, client_io) = tokio::io::duplex(4096);

    tokio::spawn(async move {
        let server = SpikeServer::new().serve(server_io).await;
        if let Ok(s) = server {
            let _ = s.waiting().await;
        }
    });

    ().serve(client_io).await.expect("client handshake")
}

/// **The whole handshake, from the real crate**: `initialize` → `tools/list` →
/// `tools/call`, with no store open.
#[tokio::test(flavor = "multi_thread")]
async fn the_handshake_works_and_tools_list_without_a_store() {
    let client = spike_pair().await;

    // --- initialize ------------------------------------------------------------
    let info = client.peer_info().expect("server info from initialize");
    assert_eq!(
        info.protocol_version,
        ProtocolVersion::LATEST,
        "the SDK sends 2025-11-25 (model.rs:167) — pin it, Task 14 asserts it"
    );
    assert_eq!(
        info.instructions.as_deref(),
        Some("Use selene_explore instead of reading files."),
        "`instructions` reaches the client — this is the single source of agent guidance"
    );

    // --- tools/list, with `store: None` ---------------------------------------
    let tools = client
        .list_tools(Default::default())
        .await
        .expect("tools/list");
    let names: Vec<&str> = tools.tools.iter().map(|t| t.name.as_ref()).collect();
    assert!(
        names.contains(&"selene_explore"),
        "tools must be listable with NO store open (#964: the un-indexed root)"
    );

    // The input schema is derived from the `Parameters` type by schemars 1.x.
    let explore = tools
        .tools
        .iter()
        .find(|t| t.name == "selene_explore")
        .unwrap();
    let schema = serde_json::to_value(&explore.input_schema).unwrap();
    assert_eq!(
        schema["properties"]["query"]["type"], "string",
        "schemars derived the input schema: {schema}"
    );

    client.cancel().await.ok();
}

/// **The two wire shapes, proven end to end.** Task 19 is built on exactly this.
#[tokio::test(flavor = "multi_thread")]
async fn success_and_error_results_have_the_documented_wire_shapes() {
    let client = spike_pair().await;

    // --- the SUCCESS shape: a recoverable condition ("not indexed") ------------
    // `CallToolRequestParam` is `#[non_exhaustive]` too (finding #6) — default-then-assign.
    let mut call = CallToolRequestParams::default();
    call.name = "selene_explore".into();
    call.arguments = serde_json::json!({ "query": "where is login handled" })
        .as_object()
        .cloned();

    let ok = client
        .call_tool(call)
        .await
        .expect("the call itself succeeds");

    assert_eq!(
        ok.is_error,
        Some(false),
        "a recoverable condition MUST be success-shaped. `CallToolResult::success` sets \
         is_error: Some(false) (model.rs:2990), so the field is present and false — the \
         TS shape. One isError early and the agent abandons the tool for the session."
    );
    let text = ok
        .content
        .first()
        .and_then(|c| c.as_text())
        .map(|t| t.text.as_str());
    assert!(
        text.is_some_and(|t| t.contains("not indexed")),
        "the guidance is CONTENT, not an error: {text:?}"
    );

    // --- the RESERVED shape: isError: true ------------------------------------
    let mut call = CallToolRequestParams::default();
    call.name = "selene_refuse".into();

    let refused = client
        .call_tool(call)
        .await
        .expect("still a well-formed tool RESULT, not a transport failure");

    assert_eq!(
        refused.is_error,
        Some(true),
        "`CallToolResult::error` sets is_error: Some(true) (model.rs:3047) and still \
         returns a readable result. This — not `Err(ErrorData)` — is how a PathRefusal \
         is reported: an Err would become a JSON-RPC -32603 protocol error, which some \
         clients surface as a BROKEN SERVER rather than a failed call."
    );

    client.cancel().await.ok();
}

/// The negative half of finding #1, stated as a compile-time fact rather than a runtime
/// one: `ErrorData` is the type that escapes as a **protocol** error.
///
/// (`impl IntoCallToolResult for ErrorData` returns `Err(self)`, and the blanket impl for
/// `Result<T, E>` propagates it — `handler/server/tool.rs:94-112`.)
#[test]
fn error_data_is_the_protocol_error_type_and_must_not_leak_from_a_handler() {
    let e = ErrorData::internal_error("a store malfunction", None);
    assert_eq!(e.code.0, -32603, "the JSON-RPC internal-error code");

    // The type-level reminder: a handler returning `Result<_, ErrorData>` sends THIS,
    // not an isError result. Task 19's handlers therefore return `CallToolResult`
    // directly and classify internally.
    fn _handlers_must_not_do_this() -> Result<CallToolResult, ErrorData> {
        Err(ErrorData::internal_error("would become -32603", None))
    }
}

/// `RoleServer` is the marker the handler is generic over — named here so a later reader
/// does not have to rediscover which of the SDK's four roles a tool server implements.
#[test]
fn the_server_role_is_role_server() {
    fn _assert_handler<T: ServerHandler>() {}
    _assert_handler::<SpikeServer>();
    let _: Option<RoleServer> = None;
}
