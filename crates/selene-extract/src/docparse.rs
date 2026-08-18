//! Document extraction — wave A of the doc-ingestion PRD
//! (`docs/specs/2026-08-14-document-ingestion-design.md`): markdown, plain
//! text, and reStructuredText become `Document` + `Section` nodes with
//! `contains` edges, and every outward pointer (code-span, relative link,
//! wikilink) leaves as an [`UnresolvedReference`] for the resolver to bind —
//! **zero cross-file edges here**, the crate contract (`lib.rs` §"Extraction
//! emits no cross-file edges") holds for documents too.
//!
//! Deterministic by construction: pulldown-cmark's offset iterator gives byte
//! ranges, byte ranges give line numbers, and everything is emitted in source
//! order. Section text (capped) goes into `Node.docstring` so FTS and the
//! embedder see content, not just headings (PRD §4.4: one indexing regime,
//! two render regimes).

use selene_core::{Edge, EdgeKind, Node, NodeKind, Provenance, node_id};

use crate::types::UnresolvedReference;
use crate::{ExtractionResult, Language};

/// Is this a wave-A document language (routes to [`extract_document`])?
pub fn is_document(language: Language) -> bool {
    matches!(
        language,
        Language::Markdown | Language::PlainText | Language::Rst | Language::Pdf | Language::Docx
    )
}

/// Per-section text stored in `docstring`, capped (PRD §4.4: 4 KiB).
const SECTION_TEXT_CAP: usize = 4096;
/// Code-span mention bounds: an identifier-ish span, not a code block.
const MENTION_MIN: usize = 2;
const MENTION_MAX: usize = 128;

/// One extraction pass over a document. Mirrors `extract_from_source`'s
/// contract: errors collected (none are possible for UTF-8 text), never
/// thrown; output is a pure function of `(file_path, source, language)`.
pub fn extract_document(file_path: &str, source: &str, language: Language) -> ExtractionResult {
    let started = std::time::Instant::now();
    let mut result = ExtractionResult::default();

    let total_lines = source.lines().count().max(1) as u32;
    let basename = file_path.rsplit('/').next().unwrap_or(file_path);

    // The Document node — the file's identity in the graph. No `File` node for
    // documents (PRD §4.4): one file, one node, standard hashed id.
    let doc_id = node_id(file_path, NodeKind::Document, basename, 1);
    result.nodes.push(doc_node(
        doc_id.clone(),
        basename,
        file_path,
        language,
        1,
        total_lines,
    ));

    let sections = match language {
        // Docx arrives as markdown-ish text (docbin renders Heading styles as
        // `# ` lines) — the wave-A markdown sectionizer applies as-is.
        Language::Markdown | Language::Docx => md_sections(source),
        Language::Rst => rst_sections(source),
        // A plain-text file is one section named after the file.
        _ => vec![RawSection {
            title: basename.to_string(),
            start_line: 1,
            end_line: total_lines,
            body_start: 0,
            body_end: source.len(),
        }],
    };

    for sec in &sections {
        let sec_id = node_id(file_path, NodeKind::Section, &sec.title, sec.start_line);
        let mut node = doc_node(
            sec_id.clone(),
            &sec.title,
            file_path,
            language,
            sec.start_line,
            sec.end_line,
        );
        node.kind = NodeKind::Section;
        node.docstring = Some(capped(&source[sec.body_start..sec.body_end]));
        result.nodes.push(node);

        // Document contains Section — intra-file, provable by the parse.
        result.edges.push(Edge {
            source: doc_id.clone(),
            target: sec_id.clone(),
            kind: EdgeKind::Contains,
            metadata: None,
            line: Some(sec.start_line),
            column: None,
            provenance: Some(Provenance::Parser),
        });

        // Outward pointers leave as unresolved refs (the resolver's channels).
        if language == Language::Markdown {
            collect_md_refs(
                &source[sec.body_start..sec.body_end],
                line_of(source, sec.body_start),
                &sec_id,
                file_path,
                &mut result.unresolved,
            );
        }
    }

    result.duration_ms = started.elapsed().as_millis() as u64;
    result
}

fn doc_node(
    id: String,
    name: &str,
    file_path: &str,
    language: Language,
    start_line: u32,
    end_line: u32,
) -> Node {
    Node {
        id,
        kind: NodeKind::Document,
        name: name.to_string(),
        qualified_name: name.to_string(),
        file_path: file_path.to_string(),
        language,
        start_line,
        end_line,
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

fn capped(s: &str) -> String {
    if s.len() <= SECTION_TEXT_CAP {
        return s.trim().to_string();
    }
    let mut cut = SECTION_TEXT_CAP;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s[..cut].trim().to_string()
}

/// 1-based line of a byte offset.
fn line_of(source: &str, offset: usize) -> u32 {
    (source[..offset.min(source.len())]
        .bytes()
        .filter(|&b| b == b'\n')
        .count()
        + 1) as u32
}

struct RawSection {
    title: String,
    start_line: u32,
    end_line: u32,
    body_start: usize,
    body_end: usize,
}

/// Markdown sections: one per heading (any level), spanning to the next
/// heading or EOF. Pre-heading prose becomes a "(préambule)"-free implicit
/// section only when the file has NO headings at all (then: whole file).
fn md_sections(source: &str) -> Vec<RawSection> {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

    // (heading_start_byte, title)
    let mut headings: Vec<(usize, String)> = Vec::new();
    let mut in_heading = false;
    let mut title = String::new();
    let mut start = 0usize;
    for (ev, range) in Parser::new_ext(source, Options::ENABLE_WIKILINKS).into_offset_iter() {
        match ev {
            Event::Start(Tag::Heading { .. }) => {
                in_heading = true;
                title.clear();
                start = range.start;
            }
            Event::End(TagEnd::Heading(_)) => {
                in_heading = false;
                headings.push((start, title.trim().to_string()));
            }
            Event::Text(t) | Event::Code(t) if in_heading => title.push_str(&t),
            _ => {}
        }
    }

    if headings.is_empty() {
        let total = source.lines().count().max(1) as u32;
        return vec![RawSection {
            title: "(document)".to_string(),
            start_line: 1,
            end_line: total,
            body_start: 0,
            body_end: source.len(),
        }];
    }

    let mut out = Vec::with_capacity(headings.len());
    for (i, (h_start, h_title)) in headings.iter().enumerate() {
        let body_end = headings
            .get(i + 1)
            .map(|(next, _)| *next)
            .unwrap_or(source.len());
        let end_line = if body_end == source.len() {
            source.lines().count().max(1) as u32
        } else {
            line_of(source, body_end).saturating_sub(1).max(1)
        };
        out.push(RawSection {
            title: if h_title.is_empty() {
                "(untitled)".to_string()
            } else {
                h_title.clone()
            },
            start_line: line_of(source, *h_start),
            end_line,
            body_start: *h_start,
            body_end,
        });
    }
    out
}

/// reStructuredText sections, best-effort: a non-empty line followed by an
/// underline of one repeated punctuation character at least as long.
fn rst_sections(source: &str) -> Vec<RawSection> {
    const ADORNMENTS: &str = "=-~^\"'`#*+.:_";
    let lines: Vec<&str> = source.lines().collect();
    // byte offset of each line start
    let mut offsets = Vec::with_capacity(lines.len() + 1);
    let mut acc = 0usize;
    for l in &lines {
        offsets.push(acc);
        acc += l.len() + 1;
    }
    offsets.push(source.len());

    let mut heads: Vec<(usize, String)> = Vec::new(); // (line index, title)
    for i in 0..lines.len().saturating_sub(1) {
        let t = lines[i].trim();
        let u = lines[i + 1].trim_end();
        if !t.is_empty()
            && u.len() >= t.len()
            && u.len() >= 3
            && u.chars().next().is_some_and(|c| ADORNMENTS.contains(c))
            && u.chars().all(|c| c == u.chars().next().unwrap_or(' '))
        {
            heads.push((i, t.to_string()));
        }
    }
    if heads.is_empty() {
        return vec![RawSection {
            title: "(document)".to_string(),
            start_line: 1,
            end_line: lines.len().max(1) as u32,
            body_start: 0,
            body_end: source.len(),
        }];
    }
    let mut out = Vec::with_capacity(heads.len());
    for (i, (line_idx, title)) in heads.iter().enumerate() {
        let end_line_idx = heads
            .get(i + 1)
            .map(|(next, _)| next.saturating_sub(1))
            .unwrap_or(lines.len().saturating_sub(1));
        out.push(RawSection {
            title: title.clone(),
            start_line: (*line_idx + 1) as u32,
            end_line: (end_line_idx + 1).max(*line_idx + 1) as u32,
            body_start: offsets[*line_idx],
            body_end: offsets[end_line_idx + 1].min(source.len()),
        });
    }
    out
}

/// The outward pointers of one markdown section, in source order:
/// - inline code-spans that look like identifiers → `doc_mention`
/// - relative link targets `[x](./path)` → `doc_path`
/// - wikilinks `[[Target]]` → `doc_link`
fn collect_md_refs(
    body: &str,
    body_first_line: u32,
    section_id: &str,
    file_path: &str,
    out: &mut Vec<UnresolvedReference>,
) {
    use pulldown_cmark::{Event, LinkType, Options, Parser, Tag};

    let mut push = |name: &str, kind: &str, line: u32| {
        out.push(UnresolvedReference {
            from_node_id: section_id.to_string(),
            reference_name: name.to_string(),
            reference_kind: kind.to_string(),
            line: Some(line),
            column: None,
            file_path: Some(file_path.to_string()),
            language: Some(Language::Markdown),
        });
    };

    for (ev, range) in Parser::new_ext(body, Options::ENABLE_WIKILINKS).into_offset_iter() {
        let line = body_first_line + line_of(body, range.start) - 1;
        match ev {
            Event::Code(code) => {
                let c = code.trim();
                // identifier-ish: no whitespace, sane length — `let x = 1` is
                // a code EXAMPLE, `resolve_one` is a mention.
                if c.len() >= MENTION_MIN
                    && c.len() <= MENTION_MAX
                    && !c.chars().any(char::is_whitespace)
                {
                    push(c, "doc_mention", line);
                }
            }
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                ..
            }) => {
                let dest = dest_url.as_ref();
                match link_type {
                    LinkType::WikiLink { .. } => push(dest, "doc_link", line),
                    _ => {
                        // Relative paths only — http(s)/mailto/# anchors are not
                        // graph edges. Strip a leading ./ and a #fragment tail.
                        if !dest.contains("://")
                            && !dest.starts_with('#')
                            && !dest.starts_with("mailto:")
                            && !dest.is_empty()
                        {
                            let clean = dest
                                .trim_start_matches("./")
                                .split('#')
                                .next()
                                .unwrap_or(dest);
                            if !clean.is_empty() {
                                push(clean, "doc_path", line);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    const MD: &str = "# Design\n\nThe `resolve_one` ladder binds refs. See [the roadmap](./docs/roadmap.md).\n\n## Deferred\n\nUses [[UnresolvedRef]] rows; `x y z` is an example, not a mention.\n";

    #[test]
    fn markdown_yields_document_plus_one_section_per_heading() {
        let r = extract_document("README.md", MD, Language::Markdown);
        let kinds: Vec<_> = r.nodes.iter().map(|n| n.kind).collect();
        assert_eq!(
            kinds,
            vec![NodeKind::Document, NodeKind::Section, NodeKind::Section]
        );
        assert_eq!(r.nodes[1].name, "Design");
        assert_eq!(r.nodes[1].start_line, 1);
        assert_eq!(r.nodes[2].name, "Deferred");
        assert_eq!(r.nodes[2].start_line, 5);
        // contains: Document -> each Section, Provenance::Parser
        assert_eq!(r.edges.len(), 2);
        assert!(r.edges.iter().all(|e| e.kind == EdgeKind::Contains
            && e.provenance == Some(Provenance::Parser)
            && e.source == r.nodes[0].id));
        // section text is carried in docstring for FTS/embeddings
        assert!(r.nodes[1].docstring.as_deref().unwrap().contains("ladder"));
        assert!(r.errors.is_empty());
    }

    #[test]
    fn markdown_refs_split_into_the_three_channels() {
        let r = extract_document("README.md", MD, Language::Markdown);
        let by_kind = |k: &str| -> Vec<&str> {
            r.unresolved
                .iter()
                .filter(|u| u.reference_kind == k)
                .map(|u| u.reference_name.as_str())
                .collect()
        };
        assert_eq!(
            by_kind("doc_mention"),
            vec!["resolve_one"],
            "x y z filtered"
        );
        assert_eq!(by_kind("doc_path"), vec!["docs/roadmap.md"]);
        assert_eq!(by_kind("doc_link"), vec!["UnresolvedRef"]);
        // every ref originates from a Section node, never the Document
        let section_ids: Vec<&str> = r.nodes[1..].iter().map(|n| n.id.as_str()).collect();
        assert!(
            r.unresolved
                .iter()
                .all(|u| section_ids.contains(&u.from_node_id.as_str()))
        );
    }

    #[test]
    fn extraction_is_deterministic() {
        let a = extract_document("README.md", MD, Language::Markdown);
        let b = extract_document("README.md", MD, Language::Markdown);
        let strip = |r: &ExtractionResult| {
            (
                r.nodes
                    .iter()
                    .map(|n| (n.id.clone(), n.name.clone(), n.start_line, n.end_line))
                    .collect::<Vec<_>>(),
                r.edges.len(),
                r.unresolved.len(),
            )
        };
        assert_eq!(strip(&a), strip(&b));
    }

    #[test]
    fn plaintext_is_one_section_named_after_the_file() {
        let r = extract_document(
            "notes/todo.txt",
            "line one\nline two\n",
            Language::PlainText,
        );
        assert_eq!(r.nodes.len(), 2);
        assert_eq!(r.nodes[1].kind, NodeKind::Section);
        assert_eq!(r.nodes[1].name, "todo.txt");
        assert_eq!(r.nodes[1].end_line, 2);
    }

    #[test]
    fn rst_sections_split_on_underlined_titles() {
        let rst = "Intro\n=====\n\nbody a\n\nDetails\n-------\n\nbody b\n";
        let r = extract_document("doc.rst", rst, Language::Rst);
        let names: Vec<&str> = r.nodes[1..].iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["Intro", "Details"]);
        assert_eq!(r.nodes[1].start_line, 1);
        assert_eq!(r.nodes[2].start_line, 6);
    }

    #[test]
    fn a_headingless_markdown_file_is_one_document_section() {
        let r = extract_document("x.md", "just prose, no headings\n", Language::Markdown);
        assert_eq!(r.nodes.len(), 2);
        assert_eq!(r.nodes[1].name, "(document)");
    }

    #[test]
    fn urls_and_anchors_are_not_graph_edges() {
        let md = "# T\n\n[a](https://x.com) [b](#frag) [c](mailto:x@y.z) [d](sub/file.md#sec)\n";
        let r = extract_document("x.md", md, Language::Markdown);
        let paths: Vec<&str> = r
            .unresolved
            .iter()
            .filter(|u| u.reference_kind == "doc_path")
            .map(|u| u.reference_name.as_str())
            .collect();
        assert_eq!(paths, vec!["sub/file.md"], "anchor stripped, urls dropped");
    }
}
