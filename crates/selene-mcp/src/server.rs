//! `SeleneMcp` — the rmcp server.
//!
//! # The store is opened LAZILY, and that is a requirement, not an optimization
//!
//! `tools/list` must answer at a root that has **never been indexed** (#964), and the
//! handshake must answer **before** any heavy init (#172). The spike proved rmcp allows it:
//! the tool router is built from the impl block, not from server state. So the state here is
//! an `Option`, and a handler that needs a graph opens one — or returns success-shaped
//! guidance if there is none.
//!
//! # `#[tool_handler(router = self.tool_router)]` — not optional
//!
//! The spike's finding #7: the macro's **default** router expression is `Self::tool_router()`,
//! which **rebuilds the router on every call** and ignores the field. Everything still works,
//! which is why it would ship — but Task 15 constructs a router **filtered by
//! `SELENE_MCP_TOOLS`**, and a rebuilt router silently discards that filter. The `router =`
//! argument is what makes the visibility gate real.

use std::path::PathBuf;
use std::sync::Arc;

use rmcp::handler::server::tool::ToolRouter;
use rmcp::model::{Implementation, ProtocolVersion, ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, tool_handler};

use crate::instructions::SERVER_INSTRUCTIONS;

/// The MCP server.
#[derive(Clone)]
pub struct SeleneMcp {
    /// The project root, if one was resolved. `None` = no default project — the tools still
    /// list, and every handler answers with guidance.
    pub root: Arc<Option<PathBuf>>,
    /// The tool router — **filtered at construction** by `SELENE_MCP_TOOLS` (Task 15).
    pub tool_router: ToolRouter<SeleneMcp>,
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for SeleneMcp {
    fn get_info(&self) -> ServerInfo {
        // `ServerInfo`/`Implementation` are #[non_exhaustive] (spike finding #6): a struct
        // literal — including `{ ..Default::default() }` — does not compile. Default, then
        // assign.
        let mut info = ServerInfo::default();
        info.protocol_version = ProtocolVersion::LATEST;
        info.capabilities = ServerCapabilities::builder().enable_tools().build();

        let mut me = Implementation::default();
        me.name = "selene".into();
        me.version = env!("CARGO_PKG_VERSION").into();
        info.server_info = me;

        // THE single source of agent-facing guidance. Nothing else in this workspace may
        // duplicate it — a second copy drifts, and the drifted copy is the one the agent reads.
        info.instructions = Some(SERVER_INSTRUCTIONS.to_string());
        info
    }
}

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use serde::Deserialize;

use crate::handlers;
use crate::outcome::ToolOutcome;
use crate::tools::{TOOLS_ENV, visible_tools};
use crate::validate;

/// A tool call's arguments. `projectPath` is optional on every tool — the no-default-project
/// instructions variant depends on it.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ExploreArgs {
    /// A natural-language question, or a bag of symbol/file names.
    pub query: String,
    /// The project to query. Defaults to the server's own root.
    #[serde(default, rename = "projectPath")]
    pub project_path: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SymbolArgs {
    /// The symbol name (optionally qualified: `Class.method`).
    pub symbol: String,
    /// The project to query.
    #[serde(default, rename = "projectPath")]
    pub project_path: Option<String>,
    /// How far to walk (impact only; clamped 1–10, default 2).
    #[serde(default)]
    pub depth: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FilesArgs {
    /// An optional path filter.
    #[serde(default)]
    pub path: Option<String>,
    /// The project to query.
    #[serde(default, rename = "projectPath")]
    pub project_path: Option<String>,
}

/// Cap a `query`-shaped call: a required free-form string plus the optional project path.
fn check_query(query: &str, project_path: Option<&str>) -> Result<(), ToolOutcome> {
    validate::free_form("query", query)?;
    validate::path_like("projectPath", project_path)
}

/// Cap a `symbol`-shaped call.
fn check_symbol(symbol: &str, project_path: Option<&str>) -> Result<(), ToolOutcome> {
    validate::free_form("symbol", symbol)?;
    validate::path_like("projectPath", project_path)
}

#[tool_router]
impl SeleneMcp {
    /// Build the server for a (possibly absent) project root.
    pub fn new(root: Option<PathBuf>) -> Self {
        // The router is filtered ONCE, here — and `#[tool_handler(router = self.tool_router)]`
        // is what makes that stick (spike finding #7: the macro's default rebuilds the router
        // per call and would silently discard this filter).
        let visible = visible_tools(std::env::var(TOOLS_ENV).ok().as_deref());
        let mut router = Self::tool_router();
        for name in crate::tools::ALL_TOOLS {
            if !visible.contains(name) {
                router.remove_route(name);
            }
        }

        Self {
            root: Arc::new(root),
            tool_router: router,
        }
    }

    fn root_for(&self, arg: Option<&str>) -> Option<PathBuf> {
        handlers::resolve_root(self.root.as_ref().as_ref(), arg)
    }

    /// THE tool. One call: the verbatim source, the call path (dynamic hops included), and
    /// the blast radius.
    #[tool(
        name = "explore",
        description = "Explore indexed code: returns verbatim line-numbered source of the \
                       relevant symbols, the call path among them (including dynamic-dispatch \
                       hops), and a blast-radius summary. Use INSTEAD of reading files."
    )]
    async fn explore(&self, Parameters(a): Parameters<ExploreArgs>) -> CallToolResult {
        if let Err(o) = check_query(&a.query, a.project_path.as_deref()) {
            return o.to_call_result();
        }
        handlers::explore(self.root_for(a.project_path.as_deref()), &a.query)
            .await
            .to_call_result()
    }

    #[tool(
        name = "node",
        description = "Everything about one symbol: its source, callers, and callees."
    )]
    async fn node(&self, Parameters(a): Parameters<SymbolArgs>) -> CallToolResult {
        if let Err(o) = check_symbol(&a.symbol, a.project_path.as_deref()) {
            return o.to_call_result();
        }
        handlers::node(self.root_for(a.project_path.as_deref()), &a.symbol)
            .await
            .to_call_result()
    }

    #[tool(name = "search", description = "Find symbols by name.")]
    async fn search(&self, Parameters(a): Parameters<ExploreArgs>) -> CallToolResult {
        if let Err(o) = check_query(&a.query, a.project_path.as_deref()) {
            return o.to_call_result();
        }
        handlers::search(self.root_for(a.project_path.as_deref()), &a.query)
            .await
            .to_call_result()
    }

    #[tool(
        name = "callers",
        description = "Who calls this symbol, grouped by definition site."
    )]
    async fn callers(&self, Parameters(a): Parameters<SymbolArgs>) -> CallToolResult {
        if let Err(o) = check_symbol(&a.symbol, a.project_path.as_deref()) {
            return o.to_call_result();
        }
        handlers::adjacency(self.root_for(a.project_path.as_deref()), &a.symbol, true)
            .await
            .to_call_result()
    }

    #[tool(
        name = "callees",
        description = "What this symbol calls, grouped by definition site."
    )]
    async fn callees(&self, Parameters(a): Parameters<SymbolArgs>) -> CallToolResult {
        if let Err(o) = check_symbol(&a.symbol, a.project_path.as_deref()) {
            return o.to_call_result();
        }
        handlers::adjacency(self.root_for(a.project_path.as_deref()), &a.symbol, false)
            .await
            .to_call_result()
    }

    #[tool(name = "impact", description = "What breaks if this symbol changes.")]
    async fn impact(&self, Parameters(a): Parameters<SymbolArgs>) -> CallToolResult {
        if let Err(o) = check_symbol(&a.symbol, a.project_path.as_deref()) {
            return o.to_call_result();
        }
        handlers::impact(
            self.root_for(a.project_path.as_deref()),
            &a.symbol,
            a.depth.unwrap_or(0),
        )
        .await
        .to_call_result()
    }

    #[tool(
        name = "insights",
        description = "Structural summary of the whole graph: betweenness bottlenecks, \
                       call-graph clusters (Louvain), module import cycles, rare bridges, \
                       orphan modules. Use for architecture-level questions."
    )]
    async fn insights(&self, Parameters(a): Parameters<FilesArgs>) -> CallToolResult {
        if let Err(o) = validate::path_like("projectPath", a.project_path.as_deref()) {
            return o.to_call_result();
        }
        handlers::insights(self.root_for(a.project_path.as_deref()))
            .await
            .to_call_result()
    }

    #[tool(
        name = "files",
        description = "The indexed files, optionally filtered by path."
    )]
    async fn files(&self, Parameters(a): Parameters<FilesArgs>) -> CallToolResult {
        // `files` has no required free-form arg — `path` is an optional filter. Cap both path-likes.
        if let Err(o) = validate::path_like("path", a.path.as_deref())
            .and_then(|_| validate::path_like("projectPath", a.project_path.as_deref()))
        {
            return o.to_call_result();
        }
        handlers::files(self.root_for(a.project_path.as_deref()), a.path.as_deref())
            .await
            .to_call_result()
    }
}
