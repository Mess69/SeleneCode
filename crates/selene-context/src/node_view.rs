//! The **node view** — everything about one symbol, in one answer.
//!
//! # Why this lives in Phase 4, not Phase 5
//!
//! The plan moved it here so **Task 13's gate can snapshot real output**. A gate that
//! snapshots a stub freezes the stub as correct — which is exactly how Phase 2 froze a bug
//! (a phantom Python edge) into a passing test. The renderer must exist before the thing that
//! certifies it.
//!
//! # The whole-file rule
//!
//! When a symbol's file is small enough to fit the per-file budget **whole**, we render the
//! whole file rather than a fragment. A fragment makes the agent wonder what surrounds it —
//! and wondering is one `Read` away from happening. If the file fits, there is nothing left
//! to wonder about.

use selene_core::{EdgeKind, Node};
use selene_db::GraphStore;
use selene_graph::{QueryManager, number_lines};

use crate::budgets::{ExploreBudget, budget_for};
use crate::error::Result;
use crate::flow::describe_hop;

/// Everything an agent needs about one symbol.
#[derive(Debug, Clone)]
pub struct NodeView {
    /// The symbol.
    pub node: Node,
    /// Its source, verbatim.
    pub code: Option<String>,
    /// Who calls it.
    pub callers: Vec<(Node, String)>,
    /// What it calls.
    pub callees: Vec<(Node, String)>,
    /// Whether the whole file was rendered (the whole-file rule).
    pub whole_file: bool,
}

/// Gather the node view. **`Ok(None)` when the symbol does not exist** — an ordinary answer,
/// never an error.
pub async fn build_node_view<S: GraphStore>(
    qm: &QueryManager<S>,
    symbol: &str,
) -> Result<Option<NodeView>> {
    let Some(node) = qm.find_symbol_matches(symbol).await?.into_iter().next() else {
        // #173: a qualified miss is NOTHING — never a fuzzy consolation prize, because node
        // mode is where the agent reads code and a confident wrong file is worse than none.
        return Ok(None);
    };

    let file_count = qm.file_count().await?;
    let budget = budget_for(file_count);

    // --- the whole-file rule ---------------------------------------------------
    let (code, whole_file) = match whole_file_if_it_fits(qm, &node, &budget).await? {
        Some(text) => (Some(text), true),
        None => (qm.code_of(&node)?, false),
    };

    let callers = qm
        .incoming(&node.id, &[EdgeKind::Calls, EdgeKind::References])
        .await?
        .into_iter()
        .map(|e| (e.node, describe_hop(&e.edge)))
        .collect();
    let callees = qm
        .outgoing(&node.id, &[EdgeKind::Calls, EdgeKind::References])
        .await?
        .into_iter()
        .map(|e| (e.node, describe_hop(&e.edge)))
        .collect();

    Ok(Some(NodeView {
        node,
        code,
        callers,
        callees,
        whole_file,
    }))
}

/// The whole file, if it fits the per-file budget. A fragment makes the agent wonder what
/// surrounds it — and wondering is one `Read` away from happening.
async fn whole_file_if_it_fits<S: GraphStore>(
    qm: &QueryManager<S>,
    node: &Node,
    budget: &ExploreBudget,
) -> Result<Option<String>> {
    let slice = qm.read_file_slice(&node.file_path, 1, 2000).await?;
    if slice.truncated || slice.text.len() > budget.max_chars_per_file {
        return Ok(None);
    }
    Ok(Some(slice.text))
}

/// Render the node view for an agent.
pub fn render_node_view(view: &NodeView) -> String {
    let mut out = format!(
        "## `{}` ({}:{})\n\n{} in `{}`\n\n",
        view.node.name,
        view.node.file_path,
        view.node.start_line,
        view.node.kind.as_str(),
        view.node.file_path
    );

    if !view.callers.is_empty() {
        out.push_str("### Called by\n\n");
        for (n, how) in &view.callers {
            out.push_str(&format!(
                "- `{}` ({}:{}) — {how}\n",
                n.name, n.file_path, n.start_line
            ));
        }
        out.push('\n');
    }

    if !view.callees.is_empty() {
        out.push_str("### Calls\n\n");
        for (n, how) in &view.callees {
            out.push_str(&format!(
                "- `{}` ({}:{}) — {how}\n",
                n.name, n.file_path, n.start_line
            ));
        }
        out.push('\n');
    }

    if let Some(code) = &view.code {
        // The whole-file case is already numbered from line 1 by `read_file_slice`; a
        // fragment is numbered from the symbol's own start line, so a citation off either one
        // lands where the agent expects.
        let numbered = if view.whole_file {
            code.clone()
        } else {
            number_lines(code, view.node.start_line as usize)
        };
        out.push_str(&format!(
            "**`{}`**\n\n```\n{numbered}```\n",
            view.node.file_path
        ));
    }

    out
}

/// The success-shaped miss. Node mode does not guess (#173), so it must say so usefully.
pub fn node_not_found(symbol: &str) -> String {
    format!(
        "## `{symbol}` not found\n\n\
         No symbol by that name is indexed. Node mode does **not** guess — a confident wrong \
         file is worse than none.\n\n\
         **What to do next:**\n\
         - Check the spelling, or qualify it (`Class.method`).\n\
         - Run the `explore` tool with a description instead — it searches by relevance and \
         will find the symbol even when the exact name is wrong.\n"
    )
}
