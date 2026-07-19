//! Import extraction and the require/use ref emitters (Ruby, PHP, Python).

use selene_core::{EdgeKind, NodeKind};
use tree_sitter::Node;

use crate::helpers::{get_child_by_field, get_node_text};
use crate::rules::LanguageRules;
use crate::{Language, UnresolvedReference};

use super::{NodeExtra, Session, is_ts_js_language};

/// `path.posix.normalize` for the `require_relative` join — collapses `.` and
/// resolves `..` segments, keeping the result relative (tree-sitter.ts:3485).
fn normalize_posix(p: &str) -> String {
    let absolute = p.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                // Pop a real segment; keep a leading `..` (can't escape a
                // relative root), and drop it entirely on an absolute path.
                if out.last().is_some_and(|l| *l != "..") {
                    out.pop();
                } else if !absolute {
                    out.push("..");
                }
            }
            s => out.push(s),
        }
    }
    let joined = out.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

/// Ruby `require`/`require_relative` → an `imports` ref to the required FILE.
///
/// `require "sidekiq/fetch"` is load-path-relative (the resolver suffix-matches
/// it against file paths); `require_relative "../foo"` resolves against THIS
/// file's directory. Bare gem/stdlib requires (`require "json"` — no slash) are
/// skipped: they are external, and there is no file to point at. The path form
/// (a `/` plus `.rb`) is what makes the ref resolve to a file node, so a file
/// pulled in ONLY by `require` — not by a resolved constant or call — still
/// records its cross-file dependency. Port of `emitRubyRequireRefs`
/// (tree-sitter.ts:3470-3498).
fn emit_ruby_require_refs(s: &mut Session<'_>, node: Node<'_>, from_id: &str) {
    let mut cursor = node.walk();
    let method = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "identifier");
    let mname = method.map_or("", |m| get_node_text(m, s.source()));
    if mname != "require" && mname != "require_relative" {
        return;
    }

    let mut c2 = node.walk();
    let Some(arg_list) = node
        .named_children(&mut c2)
        .find(|c| c.kind() == "argument_list")
    else {
        return;
    };
    let mut c3 = arg_list.walk();
    let Some(str_node) = arg_list
        .named_children(&mut c3)
        .find(|c| c.kind() == "string")
    else {
        return;
    };
    let mut c4 = str_node.walk();
    let Some(content) = str_node
        .named_children(&mut c4)
        .find(|c| c.kind() == "string_content")
    else {
        return;
    };
    let req = get_node_text(content, s.source()).trim();
    if req.is_empty() {
        return;
    }

    let mut ref_path = if mname == "require_relative" {
        let dir = s.file_path().rsplit_once('/').map_or("", |(d, _)| d);
        if dir.is_empty() {
            normalize_posix(req)
        } else {
            normalize_posix(&format!("{dir}/{req}"))
        }
    } else {
        // Load-path require — suffix-matched against the file path as-is.
        req.to_string()
    };

    if !ref_path.contains('/') {
        return; // bare gem/stdlib require — external, nothing to point at
    }
    if !ref_path.ends_with(".rb") {
        ref_path.push_str(".rb");
    }

    s.add_unresolved(UnresolvedReference {
        from_node_id: from_id.to_string(),
        reference_name: ref_path,
        reference_kind: EdgeKind::Imports.as_str().to_string(),
        line: Some(u32::try_from(node.start_position().row).unwrap_or(0) + 1),
        column: Some(u32::try_from(node.start_position().column).unwrap_or(0)),
        file_path: None,
        language: None,
    });
}

/// A PHP FQN `Foo\Bar\Baz` → the stored `Foo\Bar::Baz` spelling, as an `imports`
/// ref.
///
/// PHP classes are STORED namespace-qualified (`Foo\Bar::Baz` — see the PHP
/// `namespace` capture), so this is the spelling that resolves to the RIGHT
/// definition. It matters because Laravel-style codebases carry many same-named
/// contracts (`Factory`, `Dispatcher`, `Guard`) in different namespaces, and a
/// bare-name match cannot disambiguate them. A global-namespace class (no `\`)
/// already matches by simple name, so it emits nothing.
///
/// Port of `pushPhpUseRef` (tree-sitter.ts:3500-3512).
pub(crate) fn push_php_use_ref(s: &mut Session<'_>, fqn: &str, from_id: &str, node: Node<'_>) {
    let clean = fqn.trim_start_matches('\\');
    let Some(last_sep) = clean.rfind('\\') else {
        return; // global-namespace class — the simple name already matches
    };
    let qualified = format!("{}::{}", &clean[..last_sep], &clean[last_sep + 1..]);
    s.add_unresolved(UnresolvedReference {
        from_node_id: from_id.to_string(),
        reference_name: qualified,
        reference_kind: EdgeKind::Imports.as_str().to_string(),
        line: Some(u32::try_from(node.start_position().row).unwrap_or(0) + 1),
        column: Some(u32::try_from(node.start_position().column).unwrap_or(0)),
        file_path: None,
        language: None,
    });
}

/// Single `use Foo\Bar\Baz;` → the namespace-qualified `imports` ref.
/// Port of `emitPhpUseRefs` (tree-sitter.ts:3453-3458).
fn emit_php_use_refs(s: &mut Session<'_>, node: Node<'_>, from_id: &str) {
    let mut cursor = node.walk();
    let Some(clause) = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "namespace_use_clause")
    else {
        return;
    };
    let mut c2 = clause.walk();
    let Some(qn) = clause
        .named_children(&mut c2)
        .find(|c| c.kind() == "qualified_name")
    else {
        return; // bare `use Mockery;` — no namespace to qualify
    };
    let fqn = get_node_text(qn, s.source()).to_string();
    push_php_use_ref(s, &fqn, from_id, node);
}

/// Imports: hook first (single-module languages); Python inline
/// multi-import + from-import per-name refs are core machinery (map §11).
pub(super) fn extract_import(
    rules: &'static dyn LanguageRules,
    s: &mut Session<'_>,
    node: Node<'_>,
) {
    let import_text = get_node_text(node, s.source()).trim().to_string();

    if let Some(info) = rules.extract_import(node, s.source()) {
        let extra = NodeExtra {
            signature: Some(info.signature.clone()),
            ..NodeExtra::default()
        };
        s.create_node(NodeKind::Import, &info.module_name, node, extra);
        if !info.handled_refs
            && !info.module_name.is_empty()
            && let Some(parent_id) = s.scope_id().cloned()
        {
            s.add_unresolved(UnresolvedReference {
                from_node_id: parent_id,
                reference_name: info.module_name.clone(),
                reference_kind: EdgeKind::Imports.as_str().to_string(),
                line: Some(u32::try_from(node.start_position().row).unwrap_or(0) + 1),
                column: Some(u32::try_from(node.start_position().column).unwrap_or(0)),
                file_path: None,
                language: None,
            });
        }
        // TS/JS import-binding refs: each imported LOCAL binding records a
        // dependency (Task 8).
        if is_ts_js_language(s.language())
            && let Some(parent_id) = s.scope_id().cloned()
        {
            s.emit_import_binding_refs(node, &parent_id);
        }
        // Python `from m import X, Y` per-name refs:
        if s.language == Language::Python
            && node.kind() == "import_from_statement"
            && let Some(parent_id) = s.scope_id().cloned()
        {
            emit_py_from_import_refs(s, node, &parent_id);
        }
        // Ruby `require_relative "helper"` → a SECOND `imports` ref carrying the
        // resolved FILE path — that is the one the resolver can match to a file
        // node (the bare name above cannot).
        if s.language == Language::Ruby
            && let Some(parent_id) = s.scope_id().cloned()
        {
            emit_ruby_require_refs(s, node, &parent_id);
        }
        // PHP `use Foo\Bar\Baz;` → a SECOND `imports` ref in the namespace-
        // qualified `Foo\Bar::Baz` spelling. (Grouped `use Foo\{A, B}` never
        // reaches here — `PhpRules::visit_node` owns it and emits its own.)
        if s.language == Language::Php
            && let Some(parent_id) = s.scope_id().cloned()
        {
            emit_php_use_refs(s, node, &parent_id);
        }
        // INSERTION POINT (Task 9): Rust use-binding refs.
        // INSERTION POINT (Task 14): PHP use refs, Ruby require refs.
        return;
        // Hook returning None means "I didn't handle this" — fall through to
        // the inline multi-import handlers only, never a generic fallback.
    }

    // Python `import a, b` / `import numpy as np`: one import node +
    // `imports` ref per dotted_name / aliased_import.
    if s.language == Language::Python && node.kind() == "import_statement" {
        let parent_id = s.scope_id().cloned();
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
        for child in children {
            let dotted = match child.kind() {
                "dotted_name" => Some(child),
                "aliased_import" => {
                    let mut c2 = child.walk();
                    child
                        .named_children(&mut c2)
                        .find(|c| c.kind() == "dotted_name")
                }
                _ => None,
            };
            let Some(dotted) = dotted else { continue };
            let module = get_node_text(dotted, s.source()).to_string();
            let extra = NodeExtra {
                signature: Some(import_text.clone()),
                ..NodeExtra::default()
            };
            s.create_node(NodeKind::Import, &module, node, extra);
            if let Some(pid) = &parent_id {
                s.add_unresolved(UnresolvedReference {
                    from_node_id: pid.clone(),
                    reference_name: module,
                    reference_kind: EdgeKind::Imports.as_str().to_string(),
                    line: Some(u32::try_from(dotted.start_position().row).unwrap_or(0) + 1),
                    column: Some(u32::try_from(dotted.start_position().column).unwrap_or(0)),
                    file_path: None,
                    language: None,
                });
            }
        }
    }
    // INSERTION POINT (Task 9): Go grouped-import specs.
}

/// `from m import X, Y as Z` → one `imports` ref per imported name (alias
/// wins; wildcard + the module part skipped; last dotted segment).
fn emit_py_from_import_refs(s: &mut Session<'_>, node: Node<'_>, from_id: &str) {
    let module = get_child_by_field(node, "module_name");
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    for child in children {
        if let Some(m) = module
            && child.byte_range() == m.byte_range()
        {
            continue;
        }
        if child.kind() == "wildcard_import" {
            continue;
        }
        let name_node = match child.kind() {
            "aliased_import" => get_child_by_field(child, "alias")
                .or_else(|| get_child_by_field(child, "name"))
                .or_else(|| child.named_child(0)),
            "dotted_name" => Some(child),
            _ => None,
        };
        let Some(name_node) = name_node else { continue };
        let raw = get_node_text(name_node, s.source());
        let local = raw.rsplit('.').next().unwrap_or(raw);
        if local.is_empty() {
            continue;
        }
        s.add_unresolved(UnresolvedReference {
            from_node_id: from_id.to_string(),
            reference_name: local.to_string(),
            reference_kind: EdgeKind::Imports.as_str().to_string(),
            line: Some(u32::try_from(name_node.start_position().row).unwrap_or(0) + 1),
            column: Some(u32::try_from(name_node.start_position().column).unwrap_or(0)),
            file_path: None,
            language: None,
        });
    }
}
