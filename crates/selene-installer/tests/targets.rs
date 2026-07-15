#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The eight targets, driven through the public API with an injected `Ctx` over temp dirs — real
//! files, real formats, no globals. Asserts the two things that matter: selene lands in the right
//! place, and every neighbour survives byte-for-byte.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use selene_installer::{Action, Ctx, Location, install, resolve_target_flag, uninstall};

fn ctx(home: &Path, cwd: &Path) -> Ctx {
    Ctx { home: home.to_path_buf(), cwd: cwd.to_path_buf(), env: BTreeMap::new() }
}

fn bin() -> PathBuf {
    PathBuf::from("/abs/selene")
}

fn action_for<'a>(
    results: &'a [selene_installer::TargetResult],
    id: &str,
) -> &'a selene_installer::TargetResult {
    results.iter().find(|r| r.id == id).expect("target in results")
}

#[test]
fn codex_toml_install_preserves_siblings_and_uninstall_takes_only_selene() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let cfg = home.path().join(".codex/config.toml");
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    std::fs::write(&cfg, "# mine\nmodel = \"x\"\n\n[mcp_servers.other]\ncommand = \"o\"\n").unwrap();

    let c = ctx(home.path(), cwd.path());
    let r = install(&["codex".into()], Location::Global, &bin(), &c);
    assert_eq!(action_for(&r, "codex").action, Action::Updated);

    let after = std::fs::read_to_string(&cfg).unwrap();
    assert!(after.contains("# mine"), "comment kept");
    assert!(after.contains("model = \"x\""), "sibling key kept");
    assert!(after.contains("[mcp_servers.other]"), "neighbor server kept");
    assert!(after.contains("[mcp_servers.selene]"), "selene added");

    // Idempotent.
    let again = install(&["codex".into()], Location::Global, &bin(), &c);
    assert_eq!(action_for(&again, "codex").action, Action::Unchanged);

    // Uninstall removes only selene.
    let u = uninstall(&["codex".into()], Location::Global, &c);
    assert_eq!(action_for(&u, "codex").action, Action::Removed);
    let after = std::fs::read_to_string(&cfg).unwrap();
    assert!(after.contains("[mcp_servers.other]"), "neighbor survives uninstall");
    assert!(!after.contains("selene"), "selene gone");
}

#[test]
fn hermes_yaml_install_adds_server_and_toolset() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let cfg = home.path().join(".hermes/config.yaml");
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    std::fs::write(&cfg, "version: 1\nmcp_servers:\n  other:\n    command: o\n").unwrap();

    let c = ctx(home.path(), cwd.path());
    let r = install(&["hermes".into()], Location::Global, &bin(), &c);
    assert_eq!(action_for(&r, "hermes").action, Action::Updated);

    let after = std::fs::read_to_string(&cfg).unwrap();
    assert!(after.contains("  selene:"), "mcp_servers.selene added");
    assert!(after.contains("  other:"), "neighbor server kept");
    assert!(after.contains("- mcp-selene"), "toolset entry added");

    let u = uninstall(&["hermes".into()], Location::Global, &c);
    assert_eq!(action_for(&u, "hermes").action, Action::Removed);
    let after = std::fs::read_to_string(&cfg).unwrap();
    assert!(after.contains("  other:"), "neighbor survives");
    assert!(!after.contains("selene"), "selene gone from both server and toolset");
}

#[test]
fn opencode_jsonc_install_preserves_comments() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    std::fs::write(
        cwd.path().join("opencode.jsonc"),
        "{\n  // keep me\n  \"theme\": \"dark\"\n}\n",
    )
    .unwrap();

    let c = ctx(home.path(), cwd.path());
    let r = install(&["opencode".into()], Location::Local, &bin(), &c);
    assert_eq!(action_for(&r, "opencode").action, Action::Updated);

    let after = std::fs::read_to_string(cwd.path().join("opencode.jsonc")).unwrap();
    assert!(after.contains("// keep me"), "comment preserved: {after}");
    assert!(after.contains("\"theme\": \"dark\""), "neighbor key preserved");
    assert!(after.contains("\"mcp\""), "the opencode `mcp` container is used, not mcpServers");
    assert!(after.contains("\"selene\""), "selene added");
    assert!(after.contains("\"type\": \"local\""), "opencode entry shape");
}

#[test]
fn claude_json_local_install_and_uninstall() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let c = ctx(home.path(), cwd.path());

    let r = install(&["claude".into()], Location::Local, &bin(), &c);
    assert_eq!(action_for(&r, "claude").action, Action::Created);
    let mcp = cwd.path().join(".mcp.json");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&mcp).unwrap()).unwrap();
    assert_eq!(v["mcpServers"]["selene"]["command"], "/abs/selene");
    // local bakes in --path
    assert!(v["mcpServers"]["selene"]["args"].to_string().contains("--path"));

    let u = uninstall(&["claude".into()], Location::Local, &c);
    assert_eq!(action_for(&u, "claude").action, Action::Removed);
}

#[test]
fn global_only_targets_are_unsupported_locally() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let c = ctx(home.path(), cwd.path());
    for id in ["codex", "hermes", "antigravity"] {
        let r = install(&[id.into()], Location::Local, &bin(), &c);
        assert_eq!(action_for(&r, id).action, Action::Unsupported, "{id} is global-only");
    }
}

#[test]
fn target_flag_resolution() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let c = ctx(home.path(), cwd.path());

    assert_eq!(resolve_target_flag("all", &c, Location::Global).unwrap().len(), 8);
    assert!(resolve_target_flag("none", &c, Location::Global).unwrap().is_empty());
    assert_eq!(
        resolve_target_flag("claude,cursor", &c, Location::Local).unwrap(),
        vec!["claude", "cursor"]
    );
    // auto with no configs present → empty.
    assert!(resolve_target_flag("auto", &c, Location::Global).unwrap().is_empty());
    // unknown id is the one hard error.
    assert!(resolve_target_flag("bogus", &c, Location::Global).is_err());
}
