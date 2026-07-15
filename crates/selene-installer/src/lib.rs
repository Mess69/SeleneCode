//! `selene-installer` — wire SeleneCode into any of eight agents' MCP configs, and remove it again.
//!
//! # Surgical, not re-serialized
//!
//! A config like `~/.claude.json` is the user's — their other MCP servers, their key order, their
//! comments. We touch **exactly** the selene entry and leave every neighbor **byte-identical**. That
//! is why the JSON/JSONC writer is the jsonc-parser CST (not `serde_json`, which would reformat and
//! strip comments), codex is `toml_edit`, and hermes is a line-based YAML patcher. Before any
//! mutation each writer proves the file round-trips losslessly and **refuses to touch it** if not —
//! a destroyed `~/.claude.json` is a user who never comes back. See [`format`].
//!
//! # The absolute path is load-bearing
//!
//! The MCP entry names `current_exe()`'s **absolute** path, not the bare `selene`. A static binary
//! is not guaranteed on `PATH`, and a config that names an unrunnable command fails **silently**.
//!
//! # Eight targets, four formats
//!
//! `claude, cursor, gemini, antigravity, kiro` (JSON `mcpServers`), `codex` (TOML), `opencode`
//! (JSONC `mcp`), `hermes` (YAML `mcp_servers` + `platform_toolsets.cli`). See [`targets`] for the
//! per-agent paths and entry shapes, and the deferred-scope note (secondary instruction/cleanup
//! files are not ported — this registers the MCP server, which is the feature).

mod format;
mod targets;

pub use targets::{
    ALL_TARGETS, Action, Ctx, Location, TargetResult, install, print_config, resolve_target_flag,
    uninstall,
};
