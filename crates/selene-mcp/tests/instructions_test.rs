#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The server-instructions are a **verbatim port**. This test is what keeps them one.

use selene_mcp::SERVER_INSTRUCTIONS;

/// The TS original lives in a fixture. Applying **only** the sanctioned rename pairs must
/// reproduce our text byte for byte.
///
/// The instructions were tuned against real agent behavior. A well-meant improvement is how
/// that tuning is lost — silently, with every test green, and the only symptom an agent that
/// starts reaching for `Read` again. So a rewrite fails the build, and the diff against TS
/// stays reviewable line by line, forever.
#[test]
fn instructions_are_the_ts_text_with_only_the_rename_table_applied() {
    let ts = include_str!("fixtures/ts-server-instructions.txt");

    // The table. Order matters: the factual fix runs before the brand rename, and the tool
    // names before the bare word.
    let pairs: &[(&str, &str)] = &[
        ("a SQLite knowledge graph", "an embedded knowledge graph"), // the ONE factual fix
        ("codegraph_explore", "selene_explore"),
        ("codegraph init", "selene index"),
        (".codegraph/", ".selene/"),
        (
            "[[codegraph-explore-summary]]",
            "[[selene-explore-summary]]",
        ),
        ("CodeGraph", "Selene"),
        ("Codegraph", "Selene"),
        ("codegraph", "selene"),
    ];

    let mut renamed = ts.to_string();
    for (from, to) in pairs {
        renamed = renamed.replace(from, to);
    }

    assert_eq!(
        renamed, SERVER_INSTRUCTIONS,
        "the instructions have drifted from the TS text by something OTHER than the rename \
         table. Not one word of guidance may be rewritten."
    );
}

/// The "single tool" sentence is only TRUE under the visibility gate — the two move together.
#[test]
fn the_instructions_promise_a_single_tool_which_the_gate_must_keep_true() {
    assert!(SERVER_INSTRUCTIONS.contains("There is a single tool, `selene_explore`"));
    assert_eq!(
        selene_mcp::visible_tools(None).len(),
        1,
        "the text says 'a single tool'. Unhide the other six and the instructions become a lie."
    );
}

/// No stale branding, and the factual fix applied.
#[test]
fn nothing_says_codegraph_or_sqlite() {
    let lower = SERVER_INSTRUCTIONS.to_lowercase();
    assert!(!lower.contains("codegraph"));
    assert!(
        !lower.contains("sqlite"),
        "the one FACTUAL fix: we are not SQLite"
    );
    assert!(SERVER_INSTRUCTIONS.contains(".selene/"));
    assert!(SERVER_INSTRUCTIONS.contains("selene index"));
}
