//! The `insights` tool's renderer: [`selene_graph::analysis::Insights`] →
//! compact agent-facing markdown. Pure (testable without a store); the handler
//! owns opening the graph and the output cap.
//!
//! The rendering answers the questions an agent would otherwise Read for:
//! which clusters exist (and what to call them), which symbols are the true
//! bottlenecks (betweenness, not degree — degree rewards plumbing), whether
//! the layering holds (import cycles), and which dependencies are one-edge
//! surprises.

use selene_graph::analysis::Insights;

/// Render the overview. Deterministic: the input is already deterministically
/// ordered by `compute_insights`.
pub fn render_insights(ins: &Insights, root_label: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "## Graph insights — {root_label}\n\n{} symbols ({} app-quality) · {} edges\n\n",
        ins.total_nodes, ins.app_nodes, ins.total_edges
    ));

    if !ins.hubs.is_empty() {
        let approx = if ins.approx_betweenness {
            " (sampled — large graph)"
        } else {
            ""
        };
        out.push_str(&format!(
            "### Structural bottlenecks (betweenness centrality{approx})\n\n\
             The symbols the most shortest paths flow THROUGH — change these carefully; \
             ask `explore` about them first.\n\n"
        ));
        for h in &ins.hubs {
            out.push_str(&format!(
                "- `{}` — betweenness {:.0}, {} in / {} out ({})\n",
                h.name, h.betweenness, h.in_deg, h.out_deg, h.file
            ));
        }
        out.push('\n');
    }

    if !ins.communities.is_empty() {
        out.push_str("### Call-graph clusters (Louvain — who CALLS whom, not who sits where)\n\n");
        for c in &ins.communities {
            let span = if c.module_span >= 3 {
                format!(" — spans {} directories", c.module_span)
            } else {
                String::new()
            };
            out.push_str(&format!(
                "- `{}` — {} symbols, mostly `{}`{span}\n",
                c.hub, c.size, c.dominant_module
            ));
        }
        out.push('\n');
    }

    out.push_str("### Module import cycles\n\n");
    if ins.import_cycles.is_empty() {
        out.push_str("None — the layering holds.\n\n");
    } else {
        for cycle in &ins.import_cycles {
            out.push_str(&format!("- {}\n", cycle.join(" ⇄ ")));
        }
        out.push('\n');
    }

    if !ins.rare_bridges.is_empty() {
        out.push_str("### Rare bridges (cross-module dependencies on ≤ 2 edges)\n\n");
        for (s, t, c) in &ins.rare_bridges {
            out.push_str(&format!(
                "- `{s}` → `{t}` — {c} edge{}\n",
                if *c == 1 { "" } else { "s" }
            ));
        }
        out.push('\n');
    }

    if !ins.orphan_modules.is_empty() {
        out.push_str("### Orphan modules (no edges in or out)\n\n");
        for (label, members) in &ins.orphan_modules {
            out.push_str(&format!("- `{label}` — {members} symbols\n"));
        }
        out.push('\n');
    }

    out.push_str(
        "Use `explore` with any name above to see its source, callers, and flow — do not Read files.\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use selene_graph::analysis::{CommunityInsight, HubInsight};

    #[test]
    fn renders_every_section_and_never_suggests_reading() {
        let ins = Insights {
            total_nodes: 100,
            total_edges: 300,
            app_nodes: 80,
            communities: vec![CommunityInsight {
                id: 0,
                size: 40,
                hub: "ResolveLadder".into(),
                dominant_module: "src/resolve".into(),
                module_span: 4,
            }],
            hubs: vec![HubInsight {
                name: "dispatch".into(),
                file: "src/bus.rs".into(),
                betweenness: 812.0,
                in_deg: 12,
                out_deg: 30,
            }],
            approx_betweenness: false,
            import_cycles: vec![vec!["a".into(), "b".into()]],
            rare_bridges: vec![("x".into(), "y".into(), 1)],
            orphan_modules: vec![("z".into(), 7)],
        };
        let md = render_insights(&ins, "demo");
        for needle in [
            "`dispatch` — betweenness 812",
            "`ResolveLadder` — 40 symbols",
            "spans 4 directories",
            "a ⇄ b",
            "`x` → `y` — 1 edge",
            "`z` — 7 symbols",
        ] {
            assert!(md.contains(needle), "missing {needle:?} in:\n{md}");
        }
        assert!(
            !md.to_lowercase().contains("read the file"),
            "must never suggest reading"
        );
    }

    #[test]
    fn empty_graph_renders_guidance_shape_not_noise() {
        let ins = Insights {
            total_nodes: 0,
            total_edges: 0,
            app_nodes: 0,
            communities: vec![],
            hubs: vec![],
            approx_betweenness: false,
            import_cycles: vec![],
            rare_bridges: vec![],
            orphan_modules: vec![],
        };
        let md = render_insights(&ins, "empty");
        assert!(md.contains("None — the layering holds"));
        assert!(
            !md.contains("### Structural bottlenecks"),
            "no empty sections"
        );
    }
}
