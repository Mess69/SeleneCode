#![allow(clippy::unwrap_used)]
//! `selene status` — the real binary, on a real (tiny) index.

use std::process::Command;

fn selene() -> &'static str {
    env!("CARGO_BIN_EXE_selene")
}

#[test]
fn status_of_an_unindexed_dir_guides_and_exits_nonzero() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(selene())
        .arg("status")
        .arg(tmp.path())
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "not-indexed must exit non-zero for a shell caller"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("not indexed"),
        "guidance names the condition: {err}"
    );
    assert!(err.contains("selene index"), "…and the fix: {err}");
}

#[test]
fn status_of_an_indexed_project_reports_counts() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(
        tmp.path().join("src/a.ts"),
        "export function greet(n: string) { return n.length; }\n",
    )
    .unwrap();

    let idx = Command::new(selene())
        .arg("index")
        .arg(tmp.path())
        .output()
        .unwrap();
    assert!(
        idx.status.success(),
        "index failed: {}",
        String::from_utf8_lossy(&idx.stderr)
    );

    let out = Command::new(selene())
        .arg("status")
        .arg(tmp.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("files:"), "reports a file count: {text}");
    assert!(text.contains("nodes:"), "reports a node count: {text}");
    assert!(text.contains("typescript"), "reports the language: {text}");
}
