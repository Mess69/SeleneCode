//! `selene export` — the pure serialization half (no I/O): the full graph as
//! canonical JSON, JSONL, GraphML, or DOT. `cmd::export` owns opening the
//! store and writing the bytes.
//!
//! # Canonical order is the contract
//!
//! The JSON dump doubles as the measuring instrument for the determinism gates
//! (PRD 2026-08-18 §3.2: one canonical serialization, three usages — export,
//! `selene diff`, and the doc-ingestion G1/G2 gates). So every format sorts:
//! nodes by id, edges by (source, target, kind, provenance). Input order —
//! which is store-dependent — must never leak into output bytes.
//!
//! # Escaping is the library's job, never ours
//!
//! GraphML goes through quick-xml's writer (attribute + text escaping); DOT
//! has a single `dot_escape` with its own injection test. A symbol named
//! `"</x>&` with a newline must survive every format — the same discipline as
//! the viz's `</script>` breakout test.

use selene_core::{Edge, Node, Provenance};

/// `Provenance` has no `as_str()` in core (serde-only enum); the wire strings
/// are pinned by serde `kebab-case`. Kept local until a second consumer needs it.
fn prov_str(p: Option<Provenance>) -> &'static str {
    match p {
        Some(Provenance::TreeSitter) => "tree-sitter",
        Some(Provenance::Scip) => "scip",
        Some(Provenance::Heuristic) => "heuristic",
        Some(Provenance::Parser) => "parser",
        Some(Provenance::Embedding) => "embedding",
        None => "",
    }
}

/// The formats `selene export` speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Json,
    Jsonl,
    Graphml,
    Dot,
}

impl ExportFormat {
    /// Parse a `--format` flag value. Case-insensitive; `None` for unknown.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "json" => Some(Self::Json),
            "jsonl" => Some(Self::Jsonl),
            "graphml" => Some(Self::Graphml),
            "dot" => Some(Self::Dot),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Jsonl => "jsonl",
            Self::Graphml => "graphml",
            Self::Dot => "dot",
        }
    }
}

/// Sort nodes and edges into the canonical order every format emits.
/// Returns sorted *references* — the caller's vectors are untouched.
fn canonical<'a>(nodes: &'a [Node], edges: &'a [Edge]) -> (Vec<&'a Node>, Vec<&'a Edge>) {
    let mut ns: Vec<&Node> = nodes.iter().collect();
    ns.sort_by(|a, b| a.id.cmp(&b.id));
    let mut es: Vec<&Edge> = edges.iter().collect();
    es.sort_by(|a, b| {
        (a.source.as_str(), a.target.as_str(), a.kind.as_str())
            .cmp(&(b.source.as_str(), b.target.as_str(), b.kind.as_str()))
            .then_with(|| prov_str(a.provenance).cmp(prov_str(b.provenance)))
    });
    (ns, es)
}

/// The canonical JSON dump: `{meta, nodes, edges}`, pretty-printed, sorted.
pub fn to_json(nodes: &[Node], edges: &[Edge], root_label: &str) -> String {
    let (ns, es) = canonical(nodes, edges);
    let doc = serde_json::json!({
        "meta": {
            "generator": "selene",
            "version": env!("CARGO_PKG_VERSION"),
            "extractionVersion": selene_core::EXTRACTION_VERSION,
            "root": root_label,
            "nodes": ns.len(),
            "edges": es.len(),
        },
        "nodes": ns,
        "edges": es,
    });
    // A value built from serializable structs cannot fail to serialize.
    serde_json::to_string_pretty(&doc).unwrap_or_default()
}

/// JSONL: one `{"type":"node",...}` / `{"type":"edge",...}` object per line.
pub fn to_jsonl(nodes: &[Node], edges: &[Edge]) -> String {
    let (ns, es) = canonical(nodes, edges);
    let mut out = String::new();
    for n in ns {
        let mut v = serde_json::to_value(n).unwrap_or_default();
        if let Some(obj) = v.as_object_mut() {
            obj.insert("type".into(), "node".into());
        }
        out.push_str(&serde_json::to_string(&v).unwrap_or_default());
        out.push('\n');
    }
    for e in es {
        let mut v = serde_json::to_value(e).unwrap_or_default();
        if let Some(obj) = v.as_object_mut() {
            obj.insert("type".into(), "edge".into());
        }
        out.push_str(&serde_json::to_string(&v).unwrap_or_default());
        out.push('\n');
    }
    out
}

/// GraphML with TYPED keys (`line` is an int — Gephi/yEd import typed columns).
pub fn to_graphml(nodes: &[Node], edges: &[Edge]) -> String {
    use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};
    use quick_xml::writer::Writer;

    let (ns, es) = canonical(nodes, edges);
    let mut w = Writer::new_with_indent(Vec::new(), b' ', 2);
    let _ = w.write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)));

    let mut gml = BytesStart::new("graphml");
    gml.push_attribute(("xmlns", "http://graphml.graphdrawing.org/xmlns"));
    let _ = w.write_event(Event::Start(gml));

    // <key> declarations: (id, for, attr.name, attr.type)
    const KEYS: [(&str, &str, &str, &str); 7] = [
        ("d0", "node", "kind", "string"),
        ("d1", "node", "name", "string"),
        ("d2", "node", "file", "string"),
        ("d3", "node", "line", "int"),
        ("d4", "node", "language", "string"),
        ("d5", "edge", "kind", "string"),
        ("d6", "edge", "provenance", "string"),
    ];
    for (id, target, name, ty) in KEYS {
        let mut k = BytesStart::new("key");
        k.push_attribute(("id", id));
        k.push_attribute(("for", target));
        k.push_attribute(("attr.name", name));
        k.push_attribute(("attr.type", ty));
        let _ = w.write_event(Event::Empty(k));
    }

    let mut graph = BytesStart::new("graph");
    graph.push_attribute(("id", "selene"));
    graph.push_attribute(("edgedefault", "directed"));
    let _ = w.write_event(Event::Start(graph));

    let data = |w: &mut Writer<Vec<u8>>, key: &str, value: &str| {
        let mut d = BytesStart::new("data");
        d.push_attribute(("key", key));
        let _ = w.write_event(Event::Start(d));
        let _ = w.write_event(Event::Text(BytesText::new(value)));
        let _ = w.write_event(Event::End(BytesEnd::new("data")));
    };

    for n in &ns {
        let mut el = BytesStart::new("node");
        el.push_attribute(("id", n.id.as_str()));
        let _ = w.write_event(Event::Start(el));
        data(&mut w, "d0", n.kind.as_str());
        data(&mut w, "d1", &n.name);
        data(&mut w, "d2", &n.file_path);
        data(&mut w, "d3", &n.start_line.to_string());
        data(&mut w, "d4", n.language.as_str());
        let _ = w.write_event(Event::End(BytesEnd::new("node")));
    }
    for e in &es {
        let mut el = BytesStart::new("edge");
        el.push_attribute(("source", e.source.as_str()));
        el.push_attribute(("target", e.target.as_str()));
        let _ = w.write_event(Event::Start(el));
        data(&mut w, "d5", e.kind.as_str());
        data(&mut w, "d6", prov_str(e.provenance));
        let _ = w.write_event(Event::End(BytesEnd::new("edge")));
    }

    let _ = w.write_event(Event::End(BytesEnd::new("graph")));
    let _ = w.write_event(Event::End(BytesEnd::new("graphml")));
    let mut bytes = w.into_inner();
    bytes.push(b'\n');
    String::from_utf8(bytes).unwrap_or_default()
}

/// Escape a string for a double-quoted DOT id/label.
fn dot_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

/// Graphviz DOT: node ids are the selene ids (quoted), labels are names.
pub fn to_dot(nodes: &[Node], edges: &[Edge]) -> String {
    let (ns, es) = canonical(nodes, edges);
    let mut out = String::from("digraph selene {\n  rankdir=LR;\n  node [shape=box];\n");
    for n in ns {
        out.push_str(&format!(
            "  \"{}\" [label=\"{}\", kind=\"{}\"];\n",
            dot_escape(&n.id),
            dot_escape(&n.name),
            n.kind.as_str()
        ));
    }
    for e in es {
        out.push_str(&format!(
            "  \"{}\" -> \"{}\" [label=\"{}\"];\n",
            dot_escape(&e.source),
            dot_escape(&e.target),
            e.kind.as_str()
        ));
    }
    out.push_str("}\n");
    out
}

/// Render `format` — the single entry point `cmd::export` calls.
pub fn render(format: ExportFormat, nodes: &[Node], edges: &[Edge], root_label: &str) -> String {
    match format {
        ExportFormat::Json => to_json(nodes, edges, root_label),
        ExportFormat::Jsonl => to_jsonl(nodes, edges),
        ExportFormat::Graphml => to_graphml(nodes, edges),
        ExportFormat::Dot => to_dot(nodes, edges),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use selene_core::{EdgeKind, Language, NodeKind, Provenance};

    fn node(id: &str, name: &str) -> Node {
        Node {
            id: id.to_string(),
            kind: NodeKind::Function,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: "src/lib.rs".to_string(),
            language: Language::Rust,
            start_line: 7,
            end_line: 9,
            start_column: 0,
            end_column: 0,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: None,
            is_async: None,
            is_static: None,
            is_abstract: None,
            decorators: vec![],
            type_parameters: vec![],
            return_type: None,
            route_method: None,
            route_path: None,
            framework: None,
            updated_at: 0,
        }
    }

    fn edge(s: &str, t: &str) -> Edge {
        Edge {
            source: s.to_string(),
            target: t.to_string(),
            kind: EdgeKind::Calls,
            metadata: None,
            line: None,
            column: None,
            provenance: Some(Provenance::TreeSitter),
        }
    }

    fn fixture() -> (Vec<Node>, Vec<Edge>) {
        (
            vec![node("function:b", "b"), node("function:a", "a")],
            vec![
                edge("function:b", "function:a"),
                edge("function:a", "function:b"),
            ],
        )
    }

    #[test]
    fn every_format_is_a_pure_function_of_the_graph_not_of_input_order() {
        let (nodes, edges) = fixture();
        let mut rn = nodes.clone();
        rn.reverse();
        let mut re = edges.clone();
        re.reverse();
        for f in [
            ExportFormat::Json,
            ExportFormat::Jsonl,
            ExportFormat::Graphml,
            ExportFormat::Dot,
        ] {
            assert_eq!(
                render(f, &nodes, &edges, "r"),
                render(f, &rn, &re, "r"),
                "{} must not leak input order",
                f.as_str()
            );
        }
    }

    #[test]
    fn json_carries_meta_and_parses_back() {
        let (nodes, edges) = fixture();
        let v: serde_json::Value =
            serde_json::from_str(&to_json(&nodes, &edges, "/tmp/x")).unwrap();
        assert_eq!(v["meta"]["nodes"], 2);
        assert_eq!(v["meta"]["edges"], 2);
        assert_eq!(v["nodes"][0]["id"], "function:a", "sorted by id");
        assert_eq!(v["nodes"][0]["startLine"], 7, "full node serde (camelCase)");
    }

    #[test]
    fn jsonl_is_one_typed_object_per_line() {
        let (nodes, edges) = fixture();
        let out = to_jsonl(&nodes, &edges);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 4);
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["type"], "node");
        let last: serde_json::Value = serde_json::from_str(lines[3]).unwrap();
        assert_eq!(last["type"], "edge");
    }

    #[test]
    fn hostile_names_survive_graphml_and_dot() {
        // The injection test: quotes, angle brackets, ampersand, newline.
        let mut n = node("function:evil", "a\"</node>&<x>\nb");
        n.file_path = "src/we\"ird&.rs".into();
        let nodes = vec![n, node("function:a", "a")];
        let edges = vec![edge("function:evil", "function:a")];

        let gml = to_graphml(&nodes, &edges);
        // must not contain the raw breakout sequence...
        assert!(!gml.contains("</node>&<x>"), "raw XML injection");
        // ...and must re-parse cleanly with the name intact. quick-xml's reader
        // fragments text at entity references (Text + GeneralRef events), so
        // the round-trip accumulates fragments between <data key="d1">…</data>.
        let mut reader = quick_xml::Reader::from_str(&gml);
        let mut names: Vec<String> = Vec::new();
        let mut in_name_data = false;
        let mut acc = String::new();
        loop {
            match reader.read_event().unwrap() {
                quick_xml::events::Event::Start(e) if e.name().as_ref() == b"data" => {
                    in_name_data = e
                        .attributes()
                        .flatten()
                        .any(|a| a.key.as_ref() == b"key" && a.value.as_ref() == b"d1");
                    acc.clear();
                }
                quick_xml::events::Event::Text(t) if in_name_data => {
                    acc.push_str(&t.xml_content(quick_xml::XmlVersion::Implicit1_0).unwrap());
                }
                quick_xml::events::Event::GeneralRef(r) if in_name_data => {
                    let entity: &[u8] = r.as_ref();
                    acc.push_str(match entity {
                        b"quot" => "\"",
                        b"lt" => "<",
                        b"gt" => ">",
                        b"amp" => "&",
                        b"apos" => "'",
                        other => panic!("unexpected entity {other:?}"),
                    });
                }
                quick_xml::events::Event::End(e) if e.name().as_ref() == b"data" => {
                    if in_name_data {
                        names.push(acc.clone());
                    }
                    in_name_data = false;
                }
                quick_xml::events::Event::Eof => break,
                _ => {}
            }
        }
        assert!(
            names.iter().any(|n| n == "a\"</node>&<x>\nb"),
            "name round-trips through XML: {names:?}"
        );

        let dot = to_dot(&nodes, &edges);
        assert!(
            dot.contains("a\\\"</node>&<x>\\nb"),
            "DOT-escaped label: {dot}"
        );
        assert!(
            !dot.contains("\n\"];"),
            "no raw newline inside a quoted label"
        );
    }

    #[test]
    fn graphml_declares_typed_keys() {
        let (nodes, edges) = fixture();
        let gml = to_graphml(&nodes, &edges);
        assert!(gml.contains(r#"attr.name="line" attr.type="int""#));
        assert!(gml.contains(r#"edgedefault="directed""#));
    }

    #[test]
    fn format_parsing_is_forgiving_on_case_and_strict_on_junk() {
        assert_eq!(ExportFormat::parse("GraphML"), Some(ExportFormat::Graphml));
        assert_eq!(ExportFormat::parse("dot"), Some(ExportFormat::Dot));
        assert_eq!(ExportFormat::parse("csv"), None);
    }
}
