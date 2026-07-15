#![allow(clippy::unwrap_used)]
//! The exit-code contract, pinned against the REAL binary. Every row here is the `Outcome` table
//! in `src/exit.rs`; a subcommand does not get to invent a code. Later tasks append rows as they
//! fill in stub bodies.

use std::process::Command;

fn selene() -> &'static str {
    env!("CARGO_BIN_EXE_selene")
}

fn code(args: &[&str]) -> i32 {
    Command::new(selene())
        .args(args)
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
    assert_eq!(code(&["sync"]), 1);
    assert_eq!(code(&["daemon"]), 1);
    assert_eq!(code(&["install"]), 1);
}
