#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 4 — source access, against real files.
//!
//! **The positive control comes first, always.** `get_code` returning `""` is the
//! inert-seam signature: it passes every "no crash" test and hands the agent nothing. So the
//! first assertion in this file is that a known function's ACTUAL BODY TEXT comes back.

mod common;

use common::{index_fixture, write_3_file_fixture};
use selene_db::SurrealStore;
use selene_graph::{QueryManager, number_lines, validate_path_within_root};

async fn manager() -> (QueryManager<SurrealStore>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    write_3_file_fixture(tmp.path());
    let store = index_fixture(tmp.path()).await;
    (QueryManager::new(store, tmp.path().to_path_buf()), tmp)
}

/// **The positive control.** Real body text, not an empty string.
#[tokio::test(flavor = "multi_thread")]
async fn get_code_returns_the_actual_body_of_a_real_function() {
    let (qm, _tmp) = manager().await;

    let node = qm
        .store()
        .get_nodes_by_name("hashPassword")
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("hashPassword is indexed");

    let code = qm
        .get_code(&node.id)
        .await
        .unwrap()
        .expect("the file exists, so the code does");

    assert!(
        code.contains("hashPassword"),
        "the signature is in the slice: {code:?}"
    );
    assert!(
        code.contains("input.length"),
        "THE BODY IS IN THE SLICE — `end_line` is inclusive. An empty or signature-only \
         string here is the inert-seam signature: every 'no crash' test passes and the \
         agent gets nothing: {code:?}"
    );
}

/// A node whose file is gone is `Ok(None)` — a fact, not an error.
#[tokio::test(flavor = "multi_thread")]
async fn a_deleted_file_is_none_not_an_error() {
    let (qm, tmp) = manager().await;

    let node = qm
        .store()
        .get_nodes_by_name("login")
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap();

    std::fs::remove_file(tmp.path().join("src/service.ts")).unwrap();

    assert!(
        qm.get_code(&node.id)
            .await
            .expect(
                "a deleted file is a RECOVERABLE fact — an Err here would be an isError \
                    at the tool layer, and one of those early ends the session"
            )
            .is_none()
    );
}

/// **Read parity, byte for byte — against an INDEPENDENT re-numbering of the file.**
///
/// Pinning the first line would let a future renderer satisfy this assertion by coincidence
/// (get line 1 right, pad line 1000, still pass). So the expectation is derived from the file
/// itself, by a numbering written *here* from the `Read` contract — not by calling the code
/// under test. If the two ever disagree, one of them is wrong about what `Read` does, and
/// that is precisely the disagreement worth failing on.
#[tokio::test(flavor = "multi_thread")]
async fn read_file_slice_reproduces_an_independent_renumbering_of_the_whole_file() {
    let (qm, tmp) = manager().await;

    // Long enough that a PADDED renderer would diverge: line 1000 is four digits, line 1 is
    // one. `Read` does not right-align them, and neither may we.
    let long: String = (1..=1200).map(|i| format!("const v{i} = {i};\n")).collect();
    std::fs::write(tmp.path().join("src/long.ts"), &long).unwrap();

    let slice = qm.read_file_slice("src/long.ts", 1, 2000).await.unwrap();

    // The independent expectation: `<n>\t<line>`, no padding, trailing empty line KEPT.
    let raw = std::fs::read_to_string(tmp.path().join("src/long.ts")).unwrap();
    let expected: String = raw
        .split('\n')
        .enumerate()
        .map(|(i, line)| format!("{}\t{}\n", i + 1, line))
        .collect();

    assert_eq!(
        slice.text, expected,
        "the rendered slice must equal an INDEPENDENT re-numbering of the file. If these \
         disagree, an agent citing `file:line` from our output cites the WRONG line — and its \
         next tool call is Read, to check us. That is the whole bet, lost on whitespace."
    );

    // …and the property that makes the equality meaningful.
    assert!(
        slice.text.contains("\n1000\tconst v1000 = 1000;\n"),
        "line 1000 is not right-aligned against the one-digit lines"
    );
    assert!(!slice.text.contains(" 1000\t"), "no padding, anywhere");
    assert_eq!(slice.path, "src/long.ts");
    assert!(!slice.truncated);
}

/// An offset past the end is **success-shaped guidance**, never an error.
#[tokio::test(flavor = "multi_thread")]
async fn an_offset_past_the_end_is_success_shaped() {
    let (qm, _tmp) = manager().await;

    let slice = qm
        .read_file_slice("src/crypto.ts", 10_000, 100)
        .await
        .expect("'you asked past the end' is GUIDANCE, not a malfunction");

    assert!(slice.text.is_empty());
    assert!(
        slice.total_lines > 0,
        "…and we still tell the agent how long the file is"
    );
}

/// #527 — the one `isError` source in Phase 4, driven through the real API.
#[tokio::test(flavor = "multi_thread")]
async fn a_path_escaping_the_root_is_refused() {
    let (qm, _tmp) = manager().await;

    for escape in ["../../etc/passwd", "/etc/passwd"] {
        assert!(
            qm.read_file_slice(escape, 1, 10).await.is_err(),
            "{escape} must be REFUSED — every disk read in this workspace funnels through \
             validate_path_within_root"
        );
    }

    // The positive control: the same call shape WORKS on a legitimate path.
    assert!(qm.read_file_slice("src/crypto.ts", 1, 10).await.is_ok());
}

/// #383 — a config leaf renders keys, never values, through the real pipeline.
#[tokio::test(flavor = "multi_thread")]
async fn a_config_file_never_leaks_its_values() {
    let tmp = tempfile::tempdir().unwrap();
    write_3_file_fixture(tmp.path());
    std::fs::write(
        tmp.path().join("config.json"),
        "{\n  \"API_KEY\": \"sk-live-abc\"\n}\n",
    )
    .unwrap();

    let store = index_fixture(tmp.path()).await;
    let qm = QueryManager::new(store, tmp.path().to_path_buf());

    // Whatever nodes the config file produced, none of their code may carry the secret.
    for node in qm.store().get_nodes_by_file("config.json").await.unwrap() {
        if let Some(code) = qm.code_of(&node).unwrap() {
            assert!(
                !code.contains("sk-live-abc"),
                "a secret reached the render path — once it is in an agent's context \
                 window it has left the machine (#383): {code:?}"
            );
        }
    }
}

#[test]
fn the_numbering_helper_is_public_because_selene_context_renders_with_it() {
    assert_eq!(number_lines("a\n", 1), "1\ta\n2\t\n");
    assert!(validate_path_within_root(std::path::Path::new("/tmp"), "../x").is_err());
}
