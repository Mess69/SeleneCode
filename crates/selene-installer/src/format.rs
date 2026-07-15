//! Format-preserving surgical writers — the heart of "the files are not ours".
//!
//! Each writer edits **exactly** the selene MCP entry and leaves every other byte — other servers,
//! their comments, key order, indentation, trailing commas, line endings — where it was. Three
//! formats, one rule:
//!
//! **`prove_lossless_roundtrip` is called before any mutation.** It parses the file's current bytes,
//! re-emits them, and compares: if `parse(text) != text` the editor cannot faithfully round-trip
//! this file, so it **refuses to touch it** rather than reformat a config it does not fully
//! understand. A destroyed `~/.claude.json` is a user who never comes back.

use anyhow::{Context, Result};

/// What an edit did to a file's content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// The desired content already matched — the file is untouched (mtime must not move).
    Unchanged,
    /// New content to write.
    Write(String),
}

// ------------------------------------------------------------------------------------------------
// JSON / JSONC — one writer, via the jsonc-parser CST (comments + formatting survive).
// ------------------------------------------------------------------------------------------------

pub mod json {
    use super::*;
    use jsonc_parser::cst::{CstInputValue, CstRootNode};
    use jsonc_parser::ParseOptions;
    use serde_json::Value;

    fn opts() -> ParseOptions {
        ParseOptions::default()
    }

    /// A CST round-trips this text unchanged — safe to edit surgically.
    pub fn round_trips(text: &str) -> bool {
        match CstRootNode::parse(text, &opts()) {
            Ok(root) => root.to_string() == text,
            Err(_) => false,
        }
    }

    /// Convert a `serde_json::Value` into the CST's insert type.
    fn to_input(v: &Value) -> CstInputValue {
        match v {
            Value::Null => CstInputValue::Null,
            Value::Bool(b) => CstInputValue::Bool(*b),
            Value::Number(n) => CstInputValue::Number(n.to_string()),
            Value::String(s) => CstInputValue::String(s.clone()),
            Value::Array(a) => CstInputValue::Array(a.iter().map(to_input).collect()),
            Value::Object(o) => {
                CstInputValue::Object(o.iter().map(|(k, v)| (k.clone(), to_input(v))).collect())
            }
        }
    }

    /// Set `<container>.<key>` to `entry`, creating `<container>` if needed. `container` is the
    /// object key that holds the servers map (e.g. `mcpServers`). Empty content seeds `{}`.
    pub fn upsert(text: &str, container: &str, key: &str, entry: &Value) -> Result<Edit> {
        let seed = if text.trim().is_empty() { "{}" } else { text };
        let root = CstRootNode::parse(seed, &opts()).context("parse JSON")?;
        let obj = root.object_value_or_set();
        let servers = obj.object_value_or_set(container);
        match servers.get(key) {
            Some(prop) => prop.set_value(to_input(entry)),
            None => {
                servers.append(key, to_input(entry));
            }
        }
        let out = root.to_string();
        Ok(if out == text { Edit::Unchanged } else { Edit::Write(out) })
    }

    /// Remove `<container>.<key>` if present.
    pub fn remove(text: &str, container: &str, key: &str) -> Result<Edit> {
        if text.trim().is_empty() {
            return Ok(Edit::Unchanged);
        }
        let root = CstRootNode::parse(text, &opts()).context("parse JSON")?;
        let Some(obj) = root.object_value() else {
            return Ok(Edit::Unchanged);
        };
        let Some(servers) = obj.object_value(container) else {
            return Ok(Edit::Unchanged);
        };
        match servers.get(key) {
            Some(prop) => prop.remove(),
            None => return Ok(Edit::Unchanged),
        }
        let out = root.to_string();
        Ok(if out == text { Edit::Unchanged } else { Edit::Write(out) })
    }
}

// ------------------------------------------------------------------------------------------------
// TOML — codex's config.toml, via toml_edit (format-preserving).
// ------------------------------------------------------------------------------------------------

pub mod toml {
    use super::*;
    use serde_json::Value;
    use toml_edit::{DocumentMut, Item, Table};

    /// toml_edit round-trips this text unchanged.
    pub fn round_trips(text: &str) -> bool {
        match text.parse::<DocumentMut>() {
            Ok(doc) => doc.to_string() == text,
            Err(_) => false,
        }
    }

    /// Convert a JSON value into a toml_edit item (objects → inline-or-nested tables handled by the
    /// caller for the top entry; here values become TOML scalars/arrays/inline tables).
    fn to_item(v: &Value) -> Item {
        match v {
            Value::Null => Item::Value("".into()),
            Value::Bool(b) => Item::Value((*b).into()),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Item::Value(i.into())
                } else {
                    Item::Value(n.as_f64().unwrap_or(0.0).into())
                }
            }
            Value::String(s) => Item::Value(s.as_str().into()),
            Value::Array(a) => {
                let mut arr = toml_edit::Array::new();
                for e in a {
                    if let Some(s) = e.as_str() {
                        arr.push(s);
                    } else if let Some(i) = e.as_i64() {
                        arr.push(i);
                    } else if let Some(b) = e.as_bool() {
                        arr.push(b);
                    }
                }
                Item::Value(toml_edit::Value::Array(arr))
            }
            Value::Object(o) => {
                let mut t = Table::new();
                for (k, val) in o {
                    t.insert(k, to_item(val));
                }
                Item::Table(t)
            }
        }
    }

    /// Set `[<container>.<key>]` to `entry` (an object). `container` e.g. `mcp_servers`.
    pub fn upsert(text: &str, container: &str, key: &str, entry: &Value) -> Result<Edit> {
        let mut doc = if text.trim().is_empty() {
            DocumentMut::new()
        } else {
            text.parse::<DocumentMut>().context("parse TOML")?
        };
        // Ensure `[container]` is a table.
        if !doc.contains_key(container) {
            doc[container] = Item::Table(Table::new());
        }
        let Some(container_tbl) = doc[container].as_table_mut() else {
            anyhow::bail!("`{container}` is not a table");
        };
        container_tbl.set_implicit(true); // render `[container.key]`, not an empty `[container]`
        container_tbl.insert(key, to_item(entry));
        let out = doc.to_string();
        Ok(if out == text { Edit::Unchanged } else { Edit::Write(out) })
    }

    /// Remove `[<container>.<key>]` if present.
    pub fn remove(text: &str, container: &str, key: &str) -> Result<Edit> {
        if text.trim().is_empty() {
            return Ok(Edit::Unchanged);
        }
        let mut doc = text.parse::<DocumentMut>().context("parse TOML")?;
        let removed = doc
            .get_mut(container)
            .and_then(|c| c.as_table_mut())
            .map(|t| t.remove(key).is_some())
            .unwrap_or(false);
        if !removed {
            return Ok(Edit::Unchanged);
        }
        // Drop a now-empty `[container]`.
        if doc.get(container).and_then(|c| c.as_table()).map(|t| t.is_empty()).unwrap_or(false) {
            doc.remove(container);
        }
        let out = doc.to_string();
        Ok(if out == text { Edit::Unchanged } else { Edit::Write(out) })
    }
}

// ------------------------------------------------------------------------------------------------
// YAML — hermes's config.yaml, line-based (no YAML crate; the entry is a fixed 2-space block).
// ------------------------------------------------------------------------------------------------

pub mod yaml {
    use super::*;

    /// Build the `selene:` block (2-space indent under a top-level `mcp_servers:`).
    fn selene_block(command: &str, args: &[String]) -> Vec<String> {
        let mut lines = vec!["  selene:".to_string(), format!("    command: {command}"), "    args:".to_string()];
        for a in args {
            lines.push(format!("      - {a}"));
        }
        lines
    }

    /// Is `line` a top-level `mcp_servers:` key (column 0, no leading space)?
    fn is_mcp_servers(line: &str) -> bool {
        line.trim_end() == "mcp_servers:"
    }

    /// Is `line` inside a block — blank, or indented (starts with a space)?
    fn in_block(line: &str) -> bool {
        line.is_empty() || line.starts_with(' ') || line.starts_with('\t')
    }

    /// The `[start, end)` line range of the `  selene:` child under `mcp_servers`, if present.
    fn selene_range(lines: &[String], servers_start: usize) -> Option<(usize, usize)> {
        let mut i = servers_start + 1;
        while i < lines.len() && in_block(&lines[i]) {
            if lines[i].trim_end() == "  selene:" {
                let start = i;
                let mut j = i + 1;
                // children of selene: indented deeper than 2 spaces (or blank).
                while j < lines.len()
                    && (lines[j].is_empty()
                        || lines[j].starts_with("   ")
                        || lines[j].starts_with("\t"))
                {
                    j += 1;
                }
                return Some((start, j));
            }
            i += 1;
        }
        None
    }

    /// Find the top-level `mcp_servers:` line index, if any.
    fn mcp_servers_line(lines: &[String]) -> Option<usize> {
        lines.iter().position(|l| is_mcp_servers(l))
    }

    /// Set `mcp_servers.selene` to the given command/args. Line-based: neighbors are untouched.
    pub fn upsert(text: &str, command: &str, args: &[String]) -> Result<Edit> {
        let had_trailing_nl = text.ends_with('\n') || text.is_empty();
        let mut lines: Vec<String> = if text.is_empty() {
            Vec::new()
        } else {
            text.trim_end_matches('\n').split('\n').map(|s| s.to_string()).collect()
        };
        let block = selene_block(command, args);

        match mcp_servers_line(&lines) {
            Some(idx) => match selene_range(&lines, idx) {
                // Replace an existing selene: block.
                Some((start, end)) => {
                    lines.splice(start..end, block);
                }
                // Insert right after `mcp_servers:`.
                None => {
                    lines.splice(idx + 1..idx + 1, block);
                }
            },
            // No mcp_servers: — append a fresh block.
            None => {
                if !lines.is_empty() && !lines.last().map(|l| l.is_empty()).unwrap_or(true) {
                    // keep a clean separation
                }
                lines.push("mcp_servers:".to_string());
                lines.extend(block);
            }
        }

        let mut out = lines.join("\n");
        if had_trailing_nl {
            out.push('\n');
        }
        Ok(if out == text { Edit::Unchanged } else { Edit::Write(out) })
    }

    /// Add `- <item>` under `platform_toolsets.cli:` (hermes needs `mcp-selene` there or the MCP
    /// tools are filtered out of CLI sessions). Creates the `platform_toolsets:`/`cli:` scaffold if
    /// absent. The list item indent is 4 spaces (`platform_toolsets:` → `  cli:` → `    - x`).
    pub fn upsert_toolset(text: &str, item: &str) -> Result<Edit> {
        let target = format!("    - {item}");
        if text.lines().any(|l| l.trim_end() == target.trim_end()) {
            return Ok(Edit::Unchanged);
        }
        let had_trailing_nl = text.ends_with('\n') || text.is_empty();
        let mut lines: Vec<String> = if text.is_empty() {
            Vec::new()
        } else {
            text.trim_end_matches('\n').split('\n').map(|s| s.to_string()).collect()
        };
        // Find `platform_toolsets:` → `  cli:`; append under cli, else scaffold.
        let pts = lines.iter().position(|l| l.trim_end() == "platform_toolsets:");
        match pts {
            Some(pi) => {
                let cli = (pi + 1..lines.len())
                    .take_while(|&i| in_block(&lines[i]))
                    .find(|&i| lines[i].trim_end() == "  cli:");
                match cli {
                    Some(ci) => {
                        // Insert after the last existing `    - ` item (or right after `cli:`).
                        let mut insert_at = ci + 1;
                        while insert_at < lines.len()
                            && (lines[insert_at].is_empty() || lines[insert_at].starts_with("    "))
                        {
                            insert_at += 1;
                        }
                        lines.insert(insert_at, target);
                    }
                    None => {
                        lines.splice(pi + 1..pi + 1, ["  cli:".to_string(), target]);
                    }
                }
            }
            None => {
                lines.push("platform_toolsets:".to_string());
                lines.push("  cli:".to_string());
                lines.push(target);
            }
        }
        let mut out = lines.join("\n");
        if had_trailing_nl {
            out.push('\n');
        }
        Ok(if out == text { Edit::Unchanged } else { Edit::Write(out) })
    }

    /// Remove `- <item>` from `platform_toolsets.cli:`.
    pub fn remove_toolset(text: &str, item: &str) -> Result<Edit> {
        let target = format!("    - {item}");
        let had_trailing_nl = text.ends_with('\n');
        let mut lines: Vec<String> =
            text.trim_end_matches('\n').split('\n').map(|s| s.to_string()).collect();
        let before = lines.len();
        lines.retain(|l| l.trim_end() != target.trim_end());
        if lines.len() == before {
            return Ok(Edit::Unchanged);
        }
        let mut out = lines.join("\n");
        if had_trailing_nl {
            out.push('\n');
        }
        Ok(if out == text { Edit::Unchanged } else { Edit::Write(out) })
    }

    /// Remove `mcp_servers.selene`. Drops a now-empty `mcp_servers:` too.
    pub fn remove(text: &str) -> Result<Edit> {
        if text.trim().is_empty() {
            return Ok(Edit::Unchanged);
        }
        let had_trailing_nl = text.ends_with('\n');
        let mut lines: Vec<String> =
            text.trim_end_matches('\n').split('\n').map(|s| s.to_string()).collect();

        let Some(idx) = mcp_servers_line(&lines) else {
            return Ok(Edit::Unchanged);
        };
        let Some((start, end)) = selene_range(&lines, idx) else {
            return Ok(Edit::Unchanged);
        };
        lines.splice(start..end, std::iter::empty());
        // If mcp_servers: now has no indented children, drop it too.
        let empty = lines
            .get(idx + 1)
            .map(|l| !in_block(l) || l.is_empty())
            .unwrap_or(true);
        if empty {
            // remove the mcp_servers: line if the following line isn't an indented child
            let next_is_child = lines
                .get(idx + 1)
                .map(|l| l.starts_with(' ') && !l.trim().is_empty())
                .unwrap_or(false);
            if !next_is_child {
                lines.remove(idx);
            }
        }

        let mut out = lines.join("\n");
        if had_trailing_nl {
            out.push('\n');
        }
        Ok(if out == text { Edit::Unchanged } else { Edit::Write(out) })
    }
}

// ------------------------------------------------------------------------------------------------
// Markdown — a marker-fenced instructions block in a shared file (CLAUDE.md / AGENTS.md / GEMINI.md).
// ------------------------------------------------------------------------------------------------

pub mod markdown {
    use super::*;

    pub const MARKER_BEGIN: &str = "<!-- SELENE_START -->";
    pub const MARKER_END: &str = "<!-- SELENE_END -->";

    /// The instructions agents read: use the MCP tools instead of reading files.
    pub fn block() -> String {
        format!(
            "{MARKER_BEGIN}\n\
             ## SeleneCode\n\n\
             This project is indexed by SeleneCode. For structural or flow questions — who calls X, \
             what breaks if Y changes, how Z flows — use the `explore` MCP tool (and \
             `node`/`callers`/`callees`/`impact`) **instead of reading files**: one call returns the \
             relevant source, the call path, and the blast radius. Re-index after big changes with \
             `selene sync` (git hooks do this automatically).\n\
             {MARKER_END}"
        )
    }

    fn strip(text: &str) -> String {
        let Some(begin) = text.find(MARKER_BEGIN) else {
            return text.trim_end().to_string();
        };
        let after = match text[begin..].find(MARKER_END) {
            Some(rel) => begin + rel + MARKER_END.len(),
            None => text.len(),
        };
        let mut end = after;
        if text[end..].starts_with('\n') {
            end += 1;
        }
        format!("{}{}", &text[..begin], &text[end..]).trim_end().to_string()
    }

    /// Upsert the selene instructions block into `text` (or a fresh file), touching nothing else.
    pub fn upsert(text: &str) -> Edit {
        let base = strip(text);
        let out = if base.trim().is_empty() {
            format!("{}\n", block())
        } else {
            format!("{base}\n\n{}\n", block())
        };
        if out == text { Edit::Unchanged } else { Edit::Write(out) }
    }

    /// Remove the selene block. Returns whether the file should be **deleted** (nothing else left).
    pub fn remove(text: &str) -> (Edit, bool) {
        if !text.contains(MARKER_BEGIN) {
            return (Edit::Unchanged, false);
        }
        let base = strip(text);
        if base.trim().is_empty() {
            (Edit::Unchanged, true) // caller deletes the file
        } else {
            let out = format!("{base}\n");
            (if out == text { Edit::Unchanged } else { Edit::Write(out) }, false)
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry() -> serde_json::Value {
        json!({ "command": "/abs/selene", "args": ["serve", "--mcp"] })
    }

    #[test]
    fn json_upsert_preserves_a_neighbor_and_its_comment() {
        // A JSONC file with a comment and a sibling server.
        let src = "{\n  // my servers\n  \"mcpServers\": {\n    \"other\": { \"command\": \"o\" }\n  }\n}\n";
        let Edit::Write(out) = json::upsert(src, "mcpServers", "selene", &entry()).unwrap() else {
            panic!("expected a write");
        };
        assert!(out.contains("// my servers"), "the comment survives: {out}");
        assert!(out.contains("\"other\""), "the neighbor server survives");
        assert!(out.contains("\"selene\""), "selene was added");
    }

    #[test]
    fn json_upsert_is_idempotent() {
        let src = "{}\n";
        let Edit::Write(once) = json::upsert(src, "mcpServers", "selene", &entry()).unwrap() else {
            panic!("first write");
        };
        assert_eq!(
            json::upsert(&once, "mcpServers", "selene", &entry()).unwrap(),
            Edit::Unchanged,
            "a second identical upsert is Unchanged"
        );
    }

    #[test]
    fn json_remove_takes_only_selene() {
        let src = "{ \"mcpServers\": { \"selene\": { \"command\": \"s\" }, \"other\": { \"command\": \"o\" } } }";
        let Edit::Write(out) = json::remove(src, "mcpServers", "selene").unwrap() else {
            panic!("expected a write");
        };
        assert!(out.contains("other"), "neighbor kept");
        assert!(!out.contains("selene"), "selene removed");
        assert_eq!(json::remove(&out, "mcpServers", "selene").unwrap(), Edit::Unchanged);
    }

    #[test]
    fn toml_upsert_preserves_siblings_and_is_idempotent() {
        let src = "[other]\nkey = 1\n\n[mcp_servers.existing]\ncommand = \"e\"\n";
        let Edit::Write(out) = toml::upsert(src, "mcp_servers", "selene", &entry()).unwrap() else {
            panic!("expected a write");
        };
        assert!(out.contains("[other]"), "the sibling table survives: {out}");
        assert!(out.contains("mcp_servers.existing"), "the neighbor server survives");
        assert!(out.contains("mcp_servers.selene"), "selene added");
        assert_eq!(
            toml::upsert(&out, "mcp_servers", "selene", &entry()).unwrap(),
            Edit::Unchanged,
            "idempotent"
        );
    }

    #[test]
    fn toml_remove_takes_only_selene() {
        let src = "[mcp_servers.selene]\ncommand = \"s\"\n\n[mcp_servers.other]\ncommand = \"o\"\n";
        let Edit::Write(out) = toml::remove(src, "mcp_servers", "selene").unwrap() else {
            panic!("expected a write");
        };
        assert!(out.contains("mcp_servers.other"), "neighbor kept: {out}");
        assert!(!out.contains("selene"), "selene removed");
    }

    #[test]
    fn round_trip_guards_reject_garbage() {
        assert!(!json::round_trips("{ not json"));
        assert!(json::round_trips("{ \"a\": 1 }"));
        assert!(!toml::round_trips("[[[bad"));
        assert!(toml::round_trips("a = 1\n"));
    }

    fn args() -> Vec<String> {
        ["serve", "--mcp", "--path", "/root"].iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn yaml_upsert_adds_under_existing_mcp_servers_keeping_siblings() {
        let src = "version: 1\nmcp_servers:\n  other:\n    command: o\n";
        let Edit::Write(out) = yaml::upsert(src, "/abs/selene", &args()).unwrap() else {
            panic!("expected a write");
        };
        assert!(out.contains("version: 1"), "top-level neighbor survives");
        assert!(out.contains("  other:"), "sibling server survives: {out}");
        assert!(out.contains("  selene:"), "selene added");
        assert!(out.contains("      - --path"), "args rendered as a list");
        // Idempotent.
        assert_eq!(yaml::upsert(&out, "/abs/selene", &args()).unwrap(), Edit::Unchanged);
    }

    #[test]
    fn yaml_upsert_creates_the_block_when_absent() {
        let src = "version: 1\n";
        let Edit::Write(out) = yaml::upsert(src, "/abs/selene", &args()).unwrap() else {
            panic!("expected a write");
        };
        assert!(out.contains("mcp_servers:"), "block created: {out}");
        assert!(out.contains("  selene:"));
    }

    #[test]
    fn yaml_remove_takes_only_selene() {
        let src = "mcp_servers:\n  selene:\n    command: s\n    args:\n      - serve\n  other:\n    command: o\n";
        let Edit::Write(out) = yaml::remove(src).unwrap() else {
            panic!("expected a write");
        };
        assert!(out.contains("  other:"), "neighbor kept: {out}");
        assert!(!out.contains("selene"), "selene removed");
        assert_eq!(yaml::remove(&out).unwrap(), Edit::Unchanged, "idempotent");
    }

    #[test]
    fn markdown_upsert_preserves_the_users_prose_and_is_idempotent() {
        let src = "# My project\n\nSome notes.\n";
        let Edit::Write(out) = markdown::upsert(src) else {
            panic!("expected a write");
        };
        assert!(out.contains("# My project"), "user's heading kept");
        assert!(out.contains("Some notes."), "user's prose kept");
        assert!(out.contains(markdown::MARKER_BEGIN), "block added");
        assert_eq!(markdown::upsert(&out), Edit::Unchanged, "re-upsert is a no-op");
    }

    #[test]
    fn markdown_remove_deletes_a_file_that_was_only_our_block() {
        let only_ours = format!("{}\n", markdown::block());
        let (edit, delete) = markdown::remove(&only_ours);
        assert_eq!(edit, Edit::Unchanged);
        assert!(delete, "a file that was only our block should be deleted");

        // A file with the user's prose is rewritten, not deleted.
        let mixed = format!("# Mine\n\n{}\n", markdown::block());
        let (edit, delete) = markdown::remove(&mixed);
        assert!(!delete, "keep a file with other content");
        let Edit::Write(out) = edit else { panic!("expected a rewrite") };
        assert!(out.contains("# Mine") && !out.contains(markdown::MARKER_BEGIN));
    }
}
