//! Ruby rules — port of `languages/ruby.ts`: mixins (`include`/`extend`/
//! `prepend` on a receiverless `call`) → `implements` refs from the
//! enclosing scope (args of type `constant`/`scope_resolution` only — `extend
//! self` and dynamic args are skipped), `require`/`require_relative` →
//! imports, visibility from preceding modifier calls, and the
//! [`LanguageRules::extract_bare_call`] spec (statement-level bare
//! identifiers; consumed by the Task 6 body walker, unit-tested below).
//!
//! DEFERRED (follow-up once the core chain's `Session::visit` lands on this
//! branch — the module body must re-enter the dispatch ladder, and `Session`
//! lives in core-owned walker/mod.rs): the `module` → [`NodeKind::Module`]
//! visit_node branch, and the class-scope `CONST =` variables gate (a
//! walker-ladder branch by design — walker/mod.rs carries its insertion
//! comment). Tracked in the Task 14 report.

use selene_core::Visibility;
use tree_sitter::Node;

use crate::UnresolvedReference;
use crate::helpers::{get_child_by_field, get_node_text};
use crate::rules::{ImportInfo, LanguageRules, NodeTypeTables};
use crate::walker::Session;

static TABLES: NodeTypeTables = NodeTypeTables {
    function_types: &["method"],
    class_types: &["class"],
    method_types: &["method", "singleton_method"],
    // require/require_relative surface as `call` (see extract_import).
    import_types: &["call"],
    call_types: &["call", "method_call"],
    // Ruby uses assignment like Python (top-level; the class-scope `CONST =`
    // gate is the deferred walker branch — module docs).
    variable_types: &["assignment"],
    name_field: "name",
    body_field: "body",
    params_field: "parameters",
    ..NodeTypeTables::EMPTY
};

/// Statement-level parents a bare identifier can be a call under.
const BLOCK_PARENTS: [&str; 8] = [
    "body_statement",
    "then",
    "else",
    "do",
    "begin",
    "rescue",
    "ensure",
    "when",
];

/// Ruby keywords/literals that are never bare calls.
const BARE_CALL_SKIP: [&str; 8] = [
    "true", "false", "nil", "self", "super", "__FILE__", "__LINE__", "__dir__",
];

pub(crate) struct RubyRules;

impl LanguageRules for RubyRules {
    fn tables(&self) -> &'static NodeTypeTables {
        &TABLES
    }

    /// Mixins: `include Mod`, `extend Mod`, `prepend Mod[, Other]` — the
    /// primary Ruby composition mechanism (ActiveSupport concerns,
    /// Comparable, …). They parse as a bare `call`; without this hook they'd
    /// be mis-extracted as a call to a method named "include" and the module
    /// would record no dependent. Emits `implements` (enclosing scope →
    /// mixed-in module) so editing a concern surfaces every class that
    /// includes it.
    fn visit_node(&self, node: Node<'_>, s: &mut Session<'_>) -> bool {
        if node.kind() != "call" || get_child_by_field(node, "receiver").is_some() {
            return false;
        }
        let Some(method) = get_child_by_field(node, "method") else {
            return false;
        };
        let mname = get_node_text(method, s.source());
        if mname != "include" && mname != "extend" && mname != "prepend" {
            return false;
        }
        let Some(parent_id) = s.node_stack().last().cloned() else {
            return false;
        };
        let args = get_child_by_field(node, "arguments").or_else(|| {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .find(|c| c.kind() == "argument_list")
        });
        let Some(args) = args else {
            return false;
        };
        let mut cursor = args.walk();
        let arg_nodes: Vec<Node<'_>> = args.named_children(&mut cursor).collect();
        for arg in arg_nodes {
            // `Mod` is `constant`, `Foo::Bar` is `scope_resolution`. Skip
            // `extend self` / dynamic args (`include foo()`).
            if arg.kind() == "constant" || arg.kind() == "scope_resolution" {
                s.add_unresolved(UnresolvedReference {
                    from_node_id: parent_id.clone(),
                    reference_name: get_node_text(arg, s.source()).to_string(),
                    reference_kind: "implements".to_string(),
                    line: Some(u32::try_from(node.start_position().row).unwrap_or(0) + 1),
                    column: Some(u32::try_from(node.start_position().column).unwrap_or(0)),
                    file_path: None,
                    language: None,
                });
            }
        }
        true // handled — don't also extract as a call/import named "include"
    }

    /// Bare method calls (no parens, no receiver) parse as plain
    /// `identifier`s — e.g. `reset` alone in a method body. Only
    /// statement-level identifiers (direct children of block/body nodes),
    /// skipping keywords/literals and Constants (class/module refs, not
    /// calls). Consumed by the Task 6 body walker.
    fn extract_bare_call(&self, node: Node<'_>, source: &str) -> Option<String> {
        if node.kind() != "identifier" {
            return None;
        }
        let parent = node.parent()?;
        if !BLOCK_PARENTS.contains(&parent.kind()) {
            return None;
        }
        let name = get_node_text(node, source);
        if BARE_CALL_SKIP.contains(&name) {
            return None;
        }
        // Constants (uppercase start) are class/module refs, not calls.
        if name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            return None;
        }
        Some(name.to_string())
    }

    /// Visibility from preceding modifier calls: a bare `private` /
    /// `protected` / `public` call above the method flips everything below
    /// it (scan back through preceding named siblings).
    fn get_visibility(&self, node: Node<'_>, source: &str) -> Option<Visibility> {
        let mut sibling = node.prev_named_sibling();
        while let Some(sib) = sibling {
            if sib.kind() == "call" || sib.kind() == "identifier" {
                let text = if sib.kind() == "identifier" {
                    get_node_text(sib, source)
                } else {
                    get_child_by_field(sib, "method")
                        .map(|m| get_node_text(m, source))
                        .unwrap_or("")
                };
                match text {
                    "private" => return Some(Visibility::Private),
                    "protected" => return Some(Visibility::Protected),
                    "public" => return Some(Visibility::Public),
                    _ => {}
                }
            }
            sibling = sib.prev_named_sibling();
        }
        Some(Visibility::Public)
    }

    /// `require 'json'` / `require_relative 'lib/helper'` — the string
    /// argument's content. Any other call declines (`None`).
    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        let signature = get_node_text(node, source).trim().to_string();
        let mut cursor = node.walk();
        let identifier = node
            .named_children(&mut cursor)
            .find(|c| c.kind() == "identifier")?;
        let method_name = get_node_text(identifier, source);
        if method_name != "require" && method_name != "require_relative" {
            return None; // not an import
        }
        let mut c2 = node.walk();
        let arg_list = node
            .named_children(&mut c2)
            .find(|c| c.kind() == "argument_list")?;
        let mut c3 = arg_list.walk();
        let string_node = arg_list
            .named_children(&mut c3)
            .find(|c| c.kind() == "string")?;
        let mut c4 = string_node.walk();
        let content = string_node
            .named_children(&mut c4)
            .find(|c| c.kind() == "string_content")?;
        Some(ImportInfo {
            module_name: get_node_text(content, source).to_string(),
            signature,
            handled_refs: false,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::Language;
    use crate::grammars::grammar_for;

    /// Parse `code` with the Ruby grammar and collect
    /// `extract_bare_call` results over every node in the tree.
    fn bare_calls(code: &str) -> Vec<String> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&grammar_for(Language::Ruby).unwrap())
            .unwrap();
        let tree = parser.parse(code, None).unwrap();
        let mut out = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if let Some(name) = RubyRules.extract_bare_call(node, code) {
                out.push(name);
            }
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                stack.push(child);
            }
        }
        out.sort();
        out
    }

    /// The extract_bare_call spec: statement-level identifiers under block
    /// parents only, keywords/literals and Constants skipped.
    #[test]
    fn bare_call_spec() {
        let code = "\ndef work\n  reset\n  if flag?\n    reload\n  else\n    cleanup\n  end\n  begin\n    attempt\n  rescue\n    recover\n  ensure\n    finish\n  end\n  x = compute\n  self\n  nil\n  CONSTANT\nend\n";
        let calls = bare_calls(code);
        for expected in ["reset", "reload", "cleanup", "attempt", "recover", "finish"] {
            assert!(
                calls.contains(&expected.to_string()),
                "missing {expected} in {calls:?}"
            );
        }
        // RHS of an assignment is not statement-level; keywords/literals and
        // Constants are skipped.
        for absent in ["compute", "self", "nil", "CONSTANT", "x"] {
            assert!(
                !calls.contains(&absent.to_string()),
                "unexpected {absent} in {calls:?}"
            );
        }
    }
}
