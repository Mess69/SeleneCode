//! `selene-installer` — wire SeleneCode into an agent's MCP config, and remove it again.
//!
//! # Surgical, not re-serialized
//!
//! A config like `~/.claude.json` is the user's, with their other MCP servers, their key order, and
//! sometimes their comments. We touch **exactly** `mcpServers.selene` and leave every neighbor
//! byte-for-byte where it was. `serde_json`'s `preserve_order` feature keeps the object key order on
//! a parse→edit→serialize round trip, so an install that only sets one nested key does not reorder
//! the user's file. (JSONC comments are the one thing a value round trip cannot preserve; the
//! JSON-family agents here use plain JSON. TOML/YAML agents — codex, hermes — need their own writers
//! and are not handled yet.)
//!
//! # The absolute path is load-bearing
//!
//! The MCP entry names `current_exe()`'s **absolute** path, not the bare `selene`. A static binary
//! is not guaranteed on `PATH`, and a config that names an unrunnable command fails **silently** —
//! the agent just never sees the server start. (map Q8.)

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Map, Value, json};

/// The agents whose MCP config is a plain-JSON `mcpServers.<name>` object. One writer serves them
/// all; only the file path differs. (codex/TOML and hermes/YAML are separate formats — not here.)
pub const JSON_TARGETS: &[&str] = &["claude", "cursor", "gemini", "kiro", "antigravity"];

/// Where an install writes: the project's `.mcp.json` (default), or the user's global agent config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    Local,
    Global,
}

impl Location {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "local" => Ok(Location::Local),
            "global" => Ok(Location::Global),
            other => anyhow::bail!("invalid --location '{other}' (use local|global)"),
        }
    }
}

/// What an install/uninstall did to one target — for the CLI to report.
#[derive(Debug, Clone)]
pub struct TargetResult {
    pub target: String,
    pub path: PathBuf,
    pub changed: bool,
}

/// The MCP config file for `target` at `location`. `project_root` anchors a local `.mcp.json`.
fn config_path(target: &str, location: Location, project_root: &Path) -> Result<PathBuf> {
    Ok(match location {
        // Local: every JSON agent reads a project-level `.mcp.json` (Claude Code's convention; the
        // others accept it too). One file, so multiple targets at `local` share it.
        Location::Local => project_root.join(".mcp.json"),
        Location::Global => {
            let home = dirs_home().context("could not find the home directory")?;
            match target {
                "claude" => home.join(".claude.json"),
                "cursor" => home.join(".cursor").join("mcp.json"),
                "gemini" => home.join(".gemini").join("settings.json"),
                "kiro" => home.join(".kiro").join("settings").join("mcp.json"),
                "antigravity" => home.join(".antigravity").join("mcp.json"),
                other => anyhow::bail!("unknown JSON target '{other}'"),
            }
        }
    })
}

/// `$HOME`, without pulling in the `dirs` crate for one lookup.
fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// The MCP server entry SeleneCode writes: the absolute binary path + `serve --mcp --path <root>`.
fn selene_entry(binary: &Path, project_root: &Path) -> Value {
    json!({
        "command": binary.to_string_lossy(),
        "args": ["serve", "--mcp", "--path", project_root.to_string_lossy()],
    })
}

/// Parse an existing config, or start from `{}` if it is absent. A present-but-invalid file is an
/// error — we do not silently clobber a config we could not read. The top level must be a JSON
/// object (an agent config always is); an array/scalar is refused rather than clobbered.
fn read_config(path: &Path) -> Result<Value> {
    let value: Value = match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => json!({}),
        Ok(text) => serde_json::from_str(&text)
            .with_context(|| format!("{} is not valid JSON — refusing to overwrite it", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(e).with_context(|| format!("could not read {}", path.display())),
    };
    anyhow::ensure!(
        value.is_object(),
        "{} is not a JSON object — refusing to overwrite it",
        path.display()
    );
    Ok(value)
}

/// Set `mcpServers.selene` to `entry`, creating the `mcpServers` object if needed, touching nothing
/// else. Returns whether the file's content actually changed. Errors if `mcpServers` exists but is
/// not an object (a malformed config we will not silently reshape).
fn upsert(config: &mut Value, entry: Value) -> Result<bool> {
    let root = config
        .as_object_mut()
        .context("config is not a JSON object")?;
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .context("`mcpServers` exists but is not an object — refusing to overwrite it")?;
    let before = servers.get("selene").cloned();
    servers.insert("selene".to_string(), entry.clone());
    Ok(before.as_ref() != Some(&entry))
}

/// Remove `mcpServers.selene`. Returns whether anything was removed.
fn remove(config: &mut Value) -> bool {
    config
        .as_object_mut()
        .and_then(|r| r.get_mut("mcpServers"))
        .and_then(|s| s.as_object_mut())
        .map(|servers| servers.remove("selene").is_some())
        .unwrap_or(false)
}

/// Write `config` back, pretty-printed with a trailing newline, atomically (temp file + rename).
fn write_config(path: &Path, config: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let mut text = serde_json::to_string_pretty(config).context("serialize config")?;
    text.push('\n');
    let tmp = path.with_extension("json.selene-tmp");
    std::fs::write(&tmp, text).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename into {}", path.display()))?;
    Ok(())
}

/// Install SeleneCode as an MCP server into each `target`'s config at `location`. `binary` is the
/// absolute path to write; `project_root` anchors a local `.mcp.json` and the `--path` arg.
pub fn install(
    targets: &[String],
    location: Location,
    binary: &Path,
    project_root: &Path,
) -> Result<Vec<TargetResult>> {
    let mut out = Vec::new();
    for target in targets {
        let path = config_path(target, location, project_root)?;
        let mut config = read_config(&path)?;
        let changed = upsert(&mut config, selene_entry(binary, project_root))?;
        if changed {
            write_config(&path, &config)?;
        }
        out.push(TargetResult { target: target.clone(), path, changed });
    }
    Ok(out)
}

/// Remove SeleneCode from each target's config.
pub fn uninstall(
    targets: &[String],
    location: Location,
    project_root: &Path,
) -> Result<Vec<TargetResult>> {
    let mut out = Vec::new();
    for target in targets {
        let path = config_path(target, location, project_root)?;
        let changed = if path.exists() {
            let mut config = read_config(&path)?;
            let removed = remove(&mut config);
            if removed {
                write_config(&path, &config)?;
            }
            removed
        } else {
            false
        };
        out.push(TargetResult { target: target.clone(), path, changed });
    }
    Ok(out)
}

/// The JSON snippet an install would write, for `--print-config` (no file touched).
pub fn print_config(binary: &Path, project_root: &Path) -> String {
    serde_json::to_string_pretty(&json!({ "mcpServers": { "selene": selene_entry(binary, project_root) } }))
        .unwrap_or_default()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn upsert_creates_the_server_and_reports_change() {
        let mut c = json!({});
        assert!(upsert(&mut c, json!({"command": "x"})).unwrap());
        assert_eq!(c["mcpServers"]["selene"]["command"], "x");
        // Idempotent: a second identical upsert is not a change.
        assert!(!upsert(&mut c, json!({"command": "x"})).unwrap());
    }

    #[test]
    fn upsert_preserves_the_users_other_servers_and_key_order() {
        let mut c: Value = serde_json::from_str(
            r#"{"other":1,"mcpServers":{"zeta":{"command":"z"},"alpha":{"command":"a"}}}"#,
        )
        .unwrap();
        upsert(&mut c, json!({"command": "selene"})).unwrap();
        // The neighbor servers survive, in order, and the top-level "other" key too.
        let servers = c["mcpServers"].as_object().unwrap();
        let keys: Vec<&String> = servers.keys().collect();
        assert_eq!(keys, ["zeta", "alpha", "selene"], "neighbors kept, in order, selene appended");
        assert_eq!(c["other"], 1);
    }

    #[test]
    fn remove_takes_only_selene() {
        let mut c: Value = serde_json::from_str(
            r#"{"mcpServers":{"selene":{"command":"s"},"other":{"command":"o"}}}"#,
        )
        .unwrap();
        assert!(remove(&mut c));
        assert!(c["mcpServers"]["other"].is_object(), "the other server survives");
        assert!(c["mcpServers"].get("selene").is_none());
        // Removing again is not a change.
        assert!(!remove(&mut c));
    }

    #[test]
    fn a_present_but_invalid_config_is_an_error_not_a_clobber() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".mcp.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(read_config(&path).is_err(), "we refuse to overwrite a file we could not parse");
    }

    #[test]
    fn a_malformed_mcpservers_errors_instead_of_panicking() {
        // The user typo'd `mcpServers` as a string. We error, not panic, and touch nothing.
        let mut c: Value = serde_json::from_str(r#"{"mcpServers":"oops"}"#).unwrap();
        assert!(upsert(&mut c, json!({"command": "x"})).is_err());
        // A non-object top level is refused at read time.
        let arr: Value = serde_json::from_str("[]").unwrap();
        assert!(!arr.is_object(), "read_config's is_object guard rejects this");
    }
}
