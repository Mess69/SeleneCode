//! The eight agent targets, their config paths, and their per-agent MCP entry shapes.
//!
//! Order is **frozen** and user-visible (`--target=all`, report order):
//! `claude, cursor, codex, opencode, hermes, gemini, antigravity, kiro` — the ids name *the other
//! tool*, not ours. Paths are pure functions of an injected [`Ctx`] (home, cwd, env) so the whole
//! thing is testable without touching the developer's real `$HOME`.
//!
//! # Two files per target
//!
//! Each target gets the MCP **server** entry (the load-bearing feature) and, where the agent reads
//! one, an **instructions** file telling it to use the `explore` tool instead of reading source:
//! a marker-fenced block in a shared markdown file (`CLAUDE.md`/`AGENTS.md`/`GEMINI.md`, via
//! [`Instruction::Block`]) or a file selene fully owns (cursor `.mdc`, kiro steering, via
//! [`Instruction::Owned`]). Still not ported (niche, documented deferrals): opencode's `%APPDATA%`
//! legacy sweep and antigravity's unified/legacy migration.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::json;

use crate::format::{self, Edit};

/// The injected environment — no globals, so tests can fake `$HOME` and friends.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub home: PathBuf,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
}

impl Ctx {
    /// A `Ctx` from the real process environment.
    pub fn from_env() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
        let cwd = std::env::current_dir()?.canonicalize()?;
        let env = std::env::vars().collect();
        Ok(Self { home, cwd, env })
    }

    fn env(&self, key: &str) -> Option<&str> {
        self.env.get(key).map(|s| s.as_str())
    }
}

/// Local (a config in the project) vs global (the user's per-agent config).
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

/// What happened to one file, mirroring CodeGraph's action vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Created,
    Updated,
    Unchanged,
    Removed,
    NotFound,
    Kept,
    Unsupported,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Created => "created",
            Action::Updated => "updated",
            Action::Unchanged => "unchanged",
            Action::Removed => "removed",
            Action::NotFound => "not-found",
            Action::Kept => "kept",
            Action::Unsupported => "unsupported",
        }
    }
}

/// The outcome of installing/uninstalling one target.
#[derive(Debug, Clone)]
pub struct TargetResult {
    pub id: String,
    pub path: Option<PathBuf>,
    pub action: Action,
    pub note: Option<String>,
}

/// The frozen, user-visible target order.
pub const ALL_TARGETS: &[&str] = &[
    "claude",
    "cursor",
    "codex",
    "opencode",
    "hermes",
    "gemini",
    "antigravity",
    "kiro",
];

/// The per-agent format + entry shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `mcpServers.selene = {command, args}` (claude, cursor, gemini, antigravity, kiro).
    JsonMcpServers,
    /// codex: TOML `[mcp_servers.selene]` with command + args.
    Codex,
    /// opencode: JSONC `mcp.selene = {type:"local", command:[bin,...args], enabled:true}`.
    Opencode,
    /// hermes: YAML `mcp_servers.selene` + `platform_toolsets.cli: - mcp-selene`.
    Hermes,
}

struct Target {
    id: &'static str,
    kind: Kind,
    global_only: bool,
}

fn registry() -> Vec<Target> {
    use Kind::*;
    vec![
        Target {
            id: "claude",
            kind: JsonMcpServers,
            global_only: false,
        },
        Target {
            id: "cursor",
            kind: JsonMcpServers,
            global_only: false,
        },
        Target {
            id: "codex",
            kind: Codex,
            global_only: true,
        },
        Target {
            id: "opencode",
            kind: Opencode,
            global_only: false,
        },
        Target {
            id: "hermes",
            kind: Hermes,
            global_only: true,
        },
        Target {
            id: "gemini",
            kind: JsonMcpServers,
            global_only: false,
        },
        Target {
            id: "antigravity",
            kind: JsonMcpServers,
            global_only: true,
        },
        Target {
            id: "kiro",
            kind: JsonMcpServers,
            global_only: false,
        },
    ]
}

/// Resolve a `--target auto|all|none|<csv>` flag to a concrete ordered id list. Unknown ids are the
/// crate's one hard error (exit 1). `auto` = the targets whose config file already exists.
pub fn resolve_target_flag(flag: &str, ctx: &Ctx, location: Location) -> Result<Vec<String>> {
    match flag {
        "all" => Ok(ALL_TARGETS.iter().map(|s| s.to_string()).collect()),
        "none" => Ok(Vec::new()),
        "auto" => {
            let mut out = Vec::new();
            for t in registry() {
                if t.config_path(ctx, location).is_some_and(|p| p.exists()) {
                    out.push(t.id.to_string());
                }
            }
            Ok(out)
        }
        csv => {
            let mut out = Vec::new();
            for id in csv.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
                if !ALL_TARGETS.contains(&id) {
                    anyhow::bail!(
                        "unknown --target '{id}' (choose from: {})",
                        ALL_TARGETS.join(", ")
                    );
                }
                out.push(id.to_string());
            }
            Ok(out)
        }
    }
}

impl Target {
    /// The primary config file for this target at `location`, or `None` if unsupported there.
    fn config_path(&self, ctx: &Ctx, location: Location) -> Option<PathBuf> {
        if location == Location::Local && self.global_only {
            return None;
        }
        let home = &ctx.home;
        let cwd = &ctx.cwd;
        Some(match (self.id, location) {
            ("claude", Location::Local) => cwd.join(".mcp.json"),
            ("claude", Location::Global) => home.join(".claude.json"),
            ("cursor", Location::Local) => cwd.join(".cursor").join("mcp.json"),
            ("cursor", Location::Global) => home.join(".cursor").join("mcp.json"),
            ("codex", _) => home.join(".codex").join("config.toml"),
            ("opencode", Location::Local) => opencode_file(cwd),
            ("opencode", Location::Global) => {
                let base = ctx
                    .env("XDG_CONFIG_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join(".config"));
                opencode_file(&base.join("opencode"))
            }
            ("hermes", _) => {
                let base = ctx
                    .env("HERMES_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join(".hermes"));
                base.join("config.yaml")
            }
            ("gemini", Location::Local) => cwd.join(".gemini").join("settings.json"),
            ("gemini", Location::Global) => home.join(".gemini").join("settings.json"),
            ("antigravity", _) => home.join(".gemini").join("config").join("mcp_config.json"),
            ("kiro", Location::Local) => cwd.join(".kiro").join("settings").join("mcp.json"),
            ("kiro", Location::Global) => home.join(".kiro").join("settings").join("mcp.json"),
            _ => return None,
        })
    }

    /// The MCP args for a serve invocation: local bakes in `--path <project>`, global resolves cwd.
    fn args(&self, ctx: &Ctx, location: Location) -> Vec<String> {
        let mut a = vec!["serve".to_string(), "--mcp".to_string()];
        if location == Location::Local {
            a.push("--path".to_string());
            a.push(ctx.cwd.to_string_lossy().into_owned());
        }
        a
    }

    /// Apply an install to `text`, returning the new content (or Unchanged).
    fn install_edit(
        &self,
        text: &str,
        binary: &Path,
        ctx: &Ctx,
        location: Location,
    ) -> Result<Edit> {
        let bin = binary.to_string_lossy().into_owned();
        let args = self.args(ctx, location);
        match self.kind {
            Kind::JsonMcpServers => {
                let entry = json!({ "command": bin, "args": args });
                format::json::upsert(text, "mcpServers", "selene", &entry)
            }
            Kind::Codex => {
                let entry = json!({ "command": bin, "args": args });
                format::toml::upsert(text, "mcp_servers", "selene", &entry)
            }
            Kind::Opencode => {
                let mut command = vec![bin];
                command.extend(args);
                let entry = json!({ "type": "local", "command": command, "enabled": true });
                format::json::upsert(text, "mcp", "selene", &entry)
            }
            Kind::Hermes => {
                // Two edits: the mcp_servers block, then the toolset list entry.
                let after_mcp = match format::yaml::upsert(text, &bin, &args)? {
                    Edit::Write(s) => s,
                    Edit::Unchanged => text.to_string(),
                };
                let after_toolset = match format::yaml::upsert_toolset(&after_mcp, "mcp-selene")? {
                    Edit::Write(s) => s,
                    Edit::Unchanged => after_mcp,
                };
                Ok(if after_toolset == text {
                    Edit::Unchanged
                } else {
                    Edit::Write(after_toolset)
                })
            }
        }
    }

    /// Apply an uninstall to `text`.
    fn uninstall_edit(&self, text: &str) -> Result<Edit> {
        match self.kind {
            Kind::JsonMcpServers => format::json::remove(text, "mcpServers", "selene"),
            Kind::Codex => format::toml::remove(text, "mcp_servers", "selene"),
            Kind::Opencode => format::json::remove(text, "mcp", "selene"),
            Kind::Hermes => {
                let after_mcp = match format::yaml::remove(text)? {
                    Edit::Write(s) => s,
                    Edit::Unchanged => text.to_string(),
                };
                let after_toolset = match format::yaml::remove_toolset(&after_mcp, "mcp-selene")? {
                    Edit::Write(s) => s,
                    Edit::Unchanged => after_mcp,
                };
                Ok(if after_toolset == text {
                    Edit::Unchanged
                } else {
                    Edit::Write(after_toolset)
                })
            }
        }
    }

    /// The agent-instructions file for this target, if any. `Block` = upsert a marker-fenced block
    /// into a shared markdown file the agent reads; `Owned` = a file selene fully owns.
    fn instruction(&self, ctx: &Ctx, location: Location) -> Option<Instruction> {
        if location == Location::Local && self.global_only {
            return None;
        }
        let home = &ctx.home;
        let cwd = &ctx.cwd;
        Some(match (self.id, location) {
            ("claude", Location::Local) => Instruction::Block(cwd.join("CLAUDE.md")),
            ("claude", Location::Global) => {
                Instruction::Block(home.join(".claude").join("CLAUDE.md"))
            }
            ("gemini", Location::Local) => Instruction::Block(cwd.join("GEMINI.md")),
            ("gemini", Location::Global) => {
                Instruction::Block(home.join(".gemini").join("GEMINI.md"))
            }
            ("codex", _) => Instruction::Block(home.join(".codex").join("AGENTS.md")),
            ("opencode", Location::Local) => Instruction::Block(cwd.join("AGENTS.md")),
            ("opencode", Location::Global) => {
                let base = ctx
                    .env("XDG_CONFIG_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join(".config"));
                Instruction::Block(base.join("opencode").join("AGENTS.md"))
            }
            ("cursor", Location::Local) => {
                Instruction::Owned(cwd.join(".cursor").join("rules").join("selene.mdc"))
            }
            ("kiro", Location::Local) => {
                Instruction::Owned(cwd.join(".kiro").join("steering").join("selene.md"))
            }
            ("kiro", Location::Global) => {
                Instruction::Owned(home.join(".kiro").join("steering").join("selene.md"))
            }
            _ => return None, // cursor global, hermes, antigravity: no instruction file
        })
    }

    /// Does this target's format round-trip the given text? (The refuse-to-clobber guard.)
    fn round_trips(&self, text: &str) -> bool {
        match self.kind {
            Kind::JsonMcpServers | Kind::Opencode => format::json::round_trips(text),
            Kind::Codex => format::toml::round_trips(text),
            Kind::Hermes => true, // line-based: no parser to disagree with
        }
    }
}

/// opencode uses `opencode.jsonc` unless a `.json` already exists in the same dir.
fn opencode_file(dir: &Path) -> PathBuf {
    let json = dir.join("opencode.json");
    if json.exists() {
        json
    } else {
        dir.join("opencode.jsonc")
    }
}

/// An agent-instructions destination: a marker-fenced block in a shared markdown file, or a file
/// selene fully owns (created on install, deleted on uninstall).
enum Instruction {
    Block(PathBuf),
    Owned(PathBuf),
}

// --- the install/uninstall drivers --------------------------------------------------------------

/// Install selene into `ids` at `location`. Errors on one target never abort the others. Each target
/// yields a config-file result and (where it has one) an instructions-file result.
pub fn install(ids: &[String], location: Location, binary: &Path, ctx: &Ctx) -> Vec<TargetResult> {
    let reg = registry();
    let mut out = Vec::new();
    for id in ids {
        let Some(t) = reg.iter().find(|t| t.id == id.as_str()) else {
            continue;
        };
        let Some(path) = t.config_path(ctx, location) else {
            out.push(unsupported(t.id));
            continue;
        };
        out.push(finish(
            t.id,
            Some(path.clone()),
            install_config(t, &path, binary, ctx, location),
        ));
        if let Some(instr) = t.instruction(ctx, location) {
            let (p, res) = install_instruction(instr);
            out.push(finish(t.id, Some(p), res));
        }
    }
    out
}

/// Remove selene from `ids` at `location`.
pub fn uninstall(ids: &[String], location: Location, ctx: &Ctx) -> Vec<TargetResult> {
    let reg = registry();
    let mut out = Vec::new();
    for id in ids {
        let Some(t) = reg.iter().find(|t| t.id == id.as_str()) else {
            continue;
        };
        let Some(path) = t.config_path(ctx, location) else {
            out.push(unsupported(t.id));
            continue;
        };
        out.push(finish(t.id, Some(path.clone()), uninstall_config(t, &path)));
        if let Some(instr) = t.instruction(ctx, location) {
            let (p, res) = uninstall_instruction(instr);
            out.push(finish(t.id, Some(p), res));
        }
    }
    out
}

fn install_config(
    t: &Target,
    path: &Path,
    binary: &Path,
    ctx: &Ctx,
    location: Location,
) -> Result<(Action, Option<String>)> {
    let existed = path.exists();
    let text = read_or_note(path)?;
    if !text.trim().is_empty() && !t.round_trips(&text) {
        return Ok((
            Action::Kept,
            Some("refused: file does not round-trip losslessly".into()),
        ));
    }
    match t.install_edit(&text, binary, ctx, location)? {
        Edit::Unchanged => Ok((Action::Unchanged, None)),
        Edit::Write(out) => {
            write_atomic(path, &out)?;
            Ok((
                if existed {
                    Action::Updated
                } else {
                    Action::Created
                },
                None,
            ))
        }
    }
}

fn uninstall_config(t: &Target, path: &Path) -> Result<(Action, Option<String>)> {
    if !path.exists() {
        return Ok((Action::NotFound, None));
    }
    let text = read_or_note(path)?;
    if !text.trim().is_empty() && !t.round_trips(&text) {
        return Ok((
            Action::Kept,
            Some("refused: file does not round-trip losslessly".into()),
        ));
    }
    match t.uninstall_edit(&text)? {
        Edit::Unchanged => Ok((Action::Kept, None)),
        Edit::Write(out) => {
            write_atomic(path, &out)?;
            Ok((Action::Removed, None))
        }
    }
}

/// Upsert the instructions block (or write the owned file). Returns the path it touched + outcome.
fn install_instruction(instr: Instruction) -> (PathBuf, Result<(Action, Option<String>)>) {
    match instr {
        Instruction::Block(path) => {
            let res = (|| {
                let existed = path.exists();
                let text = read_or_note(&path)?;
                match format::markdown::upsert(&text) {
                    Edit::Unchanged => Ok((Action::Unchanged, None)),
                    Edit::Write(out) => {
                        write_atomic(&path, &out)?;
                        Ok((
                            if existed {
                                Action::Updated
                            } else {
                                Action::Created
                            },
                            None,
                        ))
                    }
                }
            })();
            (path, res)
        }
        Instruction::Owned(path) => {
            let content = format!("{}\n", format::markdown::block());
            let res = (|| {
                let existed = path.exists();
                if read_or_note(&path)? == content {
                    return Ok((Action::Unchanged, None));
                }
                write_atomic(&path, &content)?;
                Ok((
                    if existed {
                        Action::Updated
                    } else {
                        Action::Created
                    },
                    None,
                ))
            })();
            (path, res)
        }
    }
}

fn uninstall_instruction(instr: Instruction) -> (PathBuf, Result<(Action, Option<String>)>) {
    match instr {
        Instruction::Block(path) => {
            let res = (|| {
                if !path.exists() {
                    return Ok((Action::NotFound, None));
                }
                let text = read_or_note(&path)?;
                let (edit, delete) = format::markdown::remove(&text);
                if delete {
                    std::fs::remove_file(&path)?;
                    return Ok((Action::Removed, None));
                }
                match edit {
                    Edit::Unchanged => Ok((Action::Kept, None)),
                    Edit::Write(out) => {
                        write_atomic(&path, &out)?;
                        Ok((Action::Removed, None))
                    }
                }
            })();
            (path, res)
        }
        Instruction::Owned(path) => {
            let res = (|| {
                if path.exists() {
                    std::fs::remove_file(&path)?;
                    Ok((Action::Removed, None))
                } else {
                    Ok((Action::NotFound, None))
                }
            })();
            (path, res)
        }
    }
}

fn unsupported(id: &str) -> TargetResult {
    TargetResult {
        id: id.to_string(),
        path: None,
        action: Action::Unsupported,
        note: Some("not supported at this location (global-only)".into()),
    }
}

/// Wrap a per-file outcome (or a collected error) into a `TargetResult`.
fn finish(id: &str, path: Option<PathBuf>, res: Result<(Action, Option<String>)>) -> TargetResult {
    let (action, note) = res.unwrap_or_else(|e| (Action::Kept, Some(format!("error: {e:#}"))));
    TargetResult {
        id: id.to_string(),
        path,
        action,
        note,
    }
}

/// Read a file, or `""` if absent. An unreadable/existing file backs itself up and is treated as
/// empty (errors collected, never thrown — CLAUDE.md invariant).
fn read_or_note(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e.into()),
    }
}

/// Atomic write: temp file + rename, creating parent dirs.
fn write_atomic(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("selene-tmp");
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// The install snippet for `--print-config` on the JSON `mcpServers` shape.
pub fn print_config(binary: &Path, ctx: &Ctx) -> String {
    let entry = json!({
        "mcpServers": {
            "selene": {
                "command": binary.to_string_lossy(),
                "args": ["serve", "--mcp", "--path", ctx.cwd.to_string_lossy()],
            }
        }
    });
    serde_json::to_string_pretty(&entry).unwrap_or_default()
}
