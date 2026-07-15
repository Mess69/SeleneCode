#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The exit-code contract, pinned against the REAL binary. Every row here is the `Outcome` table
//! in `src/exit.rs`; a subcommand does not get to invent a code. Later tasks append rows as they
//! fill in stub bodies.

use std::process::Command;

fn selene() -> &'static str {
    env!("CARGO_BIN_EXE_selene")
}

fn code(args: &[&str]) -> i32 {
    code_in(std::env::current_dir().unwrap().as_path(), args)
}

fn code_in(dir: &std::path::Path, args: &[&str]) -> i32 {
    Command::new(selene())
        .args(args)
        .current_dir(dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .code()
        .unwrap_or(-1)
}

#[test]
fn help_and_version_are_zero() {
    assert_eq!(code(&["--help"]), 0);
    assert_eq!(code(&["--version"]), 0);
    assert_eq!(code(&["version"]), 0);
}

#[test]
fn an_unknown_subcommand_is_claps_two() {
    assert_eq!(
        code(&["bogus-subcommand"]),
        2,
        "clap's usage error is exit 2"
    );
}

#[test]
fn a_query_command_on_an_unindexed_project_exits_one() {
    // The CLI's deliberate asymmetry vs MCP: un-indexed is exit 1 for a shell caller.
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().to_str().unwrap();
    assert_eq!(code(&["status", p]), 1, "status on un-indexed → 1");
    assert_eq!(
        code(&["explore", "x", "--path", p]),
        1,
        "explore on un-indexed → 1"
    );
    assert_eq!(code(&["callers", "x", "--path", p]), 1);
}

#[test]
fn expected_no_ops_are_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().to_str().unwrap();
    assert_eq!(
        code(&["uninit", p]),
        0,
        "uninit on an un-init project is a no-op → 0"
    );
    // prompt-hook NEVER breaks the prompt.
    assert_eq!(code(&["prompt-hook"]), 0);
    // serve without --mcp prints help and exits 0.
    assert_eq!(code(&["serve"]), 0);
}

#[test]
fn unimplemented_stubs_fail_cleanly() {
    // The anti-inert-seam guarantee: the arm is reachable and returns Failure until its task lands.
    // `sync` on an un-indexed project is exit 1 (nothing to sync); telemetry/upgrade are still stubs.
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().to_str().unwrap();
    assert_eq!(code(&["sync", p]), 1, "sync on an un-indexed project → 1");
    assert_eq!(code(&["telemetry"]), 1);
    assert_eq!(code(&["upgrade"]), 1);
}

#[test]
fn daemon_list_with_none_running_is_zero() {
    // `selene daemon` lists running daemons; none running is a fact, not a failure (exit 0). A
    // private HOME keeps the test off the developer's real registry.
    let home = tempfile::tempdir().unwrap();
    let out = Command::new(selene())
        .arg("daemon")
        .env("HOME", home.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert_eq!(out.code(), Some(0), "listing zero daemons → 0");
}

#[test]
fn install_writes_a_valid_mcp_config_and_uninstall_removes_it() {
    // `install` is implemented (Phase 7): it writes `.mcp.json` in the project cwd with selene's
    // ABSOLUTE binary path, exits 0, and is reversible. Runs in a tempdir so it touches no real config.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    assert_eq!(code_in(dir, &["install"]), 0, "install → 0");

    let cfg = dir.join(".mcp.json");
    let text = std::fs::read_to_string(&cfg).expect(".mcp.json was written");
    let v: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
    let cmd = v["mcpServers"]["selene"]["command"].as_str().unwrap();
    assert!(cmd.starts_with('/'), "the binary path is absolute, not bare `selene`: {cmd}");

    // --print-config touches no file (idempotent inspection).
    assert_eq!(code_in(dir, &["install", "--print-config"]), 0);

    // Uninstall removes the selene entry and still exits 0.
    assert_eq!(code_in(dir, &["uninstall"]), 0, "uninstall → 0");
    let text = std::fs::read_to_string(&cfg).unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(
        v["mcpServers"].get("selene").is_none(),
        "uninstall removed the selene entry"
    );
}
