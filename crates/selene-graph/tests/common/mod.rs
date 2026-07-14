#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]
//! **The fixture rig** — lifted verbatim from Task 1's spike, as the plan directs.
//!
//! Every `selene-graph` test (and Tasks 13 and 20's gates) stands on this. It runs the
//! **real** `Indexer` and the **real** `resolve_and_persist_batched`, because a rig that
//! indexes without resolving produces a graph where symbols exist and **no flow does** —
//! green tests, dead product. That is the shape of the four inert seams this project has
//! already paid for, and it is why [`index_fixture`] asserts a cross-file path exists.

use std::path::Path;

use selene_core::EdgeKind;
use selene_db::SurrealStore;
use selene_extract::Indexer;

/// Index + resolve a directory with the production pipeline.
pub async fn index_fixture(dir: &Path) -> SurrealStore {
    let store = SurrealStore::in_memory().await.expect("in-memory store");
    store.apply_schema().await.expect("schema");

    let indexer = Indexer::new(dir.to_path_buf(), store);
    let __ix = indexer.index_all(None).await;
    let result = &__ix;
    assert!(
        result.files_indexed > 0,
        "{dir:?} indexed ZERO files — the test would be asserting against an empty graph"
    );
    let store = indexer.into_store();

    selene_resolve::resolve_and_persist_in_memory(&store, dir, __ix.unresolved.clone(), None)
        .await
        .expect("resolution must never fail an index");

    store
}

/// The canonical 3-file TypeScript project: `handleLogin` → `login` → `hashPassword`,
/// one function per file, so **every** interesting edge is cross-file.
pub fn write_3_file_fixture(root: &Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/app.ts"),
        "import { login } from './service';\n\
         \n\
         export function handleLogin(user: string) {\n\
         \x20 return login(user);\n\
         }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/service.ts"),
        "import { hashPassword } from './crypto';\n\
         \n\
         export function login(user: string) {\n\
         \x20 const hashed = hashPassword(user);\n\
         \x20 return hashed;\n\
         }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/crypto.ts"),
        "export function hashPassword(input: string) {\n\
         \x20 return input.length;\n\
         }\n",
    )
    .unwrap();
}

/// The positive control every "nothing found" assertion must be paired with: proves the
/// rig can produce a non-empty answer at all on this fixture.
pub async fn assert_rig_resolved(store: &SurrealStore) {
    let from = store
        .get_nodes_by_name("handleLogin")
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("handleLogin");
    let to = store
        .get_nodes_by_name("login")
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("login");
    assert!(
        store
            .find_path(&from.id, &to.id, &[EdgeKind::Calls, EdgeKind::References])
            .await
            .unwrap()
            .is_some(),
        "the rig indexed but did NOT resolve — symbols with no flow is the dead-product \
         shape, and every assertion built on this store would be vacuous"
    );
}

/// A project with the **same method name in three classes** — the overload case that FTS's
/// relevance cut silently drops, plus one generated file (whose symbol must sort last).
pub fn write_overload_fixture(root: &Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("src/generated")).unwrap();

    std::fs::write(
        root.join("src/alpha.ts"),
        "export class Alpha {\n\
         \x20 handle(req: string) {\n\
         \x20   return req.length;\n\
         \x20 }\n\
         }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/beta.ts"),
        "export class Beta {\n\
         \x20 handle(req: string) {\n\
         \x20   return req.trim();\n\
         \x20 }\n\
         }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/gamma.ts"),
        "export class Gamma {\n\
         \x20 handle(req: string) {\n\
         \x20   return req;\n\
         \x20 }\n\
         }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/generated/delta.ts"),
        "export class Delta {\n\
         \x20 handle(req: string) {\n\
         \x20   return req;\n\
         \x20 }\n\
         }\n",
    )
    .unwrap();
}
