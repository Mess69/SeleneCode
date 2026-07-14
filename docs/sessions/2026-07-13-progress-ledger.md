# Phase 1 (selene-db) — SDD progress ledger
# plan: docs/plans/2026-07-12-phase1-selene-db.md · branch: feat/phase1-selene-db · base: 9263196
Task 1: complete (commits f612a2a..1cbfe3c, review clean after 1 fix round)
  Minor carried to final review: tokio under [dev-dependencies] in selene-db — move to [dependencies] when lib code goes async (Task 3+).
  Spike knowledge: .superpowers/sdd/task-1-report.md (syntax table: type::record, +collect inside braces, FULLTEXT single-column, @N@ per-statement, take(idx) error surface, class-tokenizer no camelCase split)
Task 2: complete (commit 03e7f5c, review clean round 1). Trait = 50 methods, desugared impl Future+Send (not dyn-safe), error.rs w/ Surreal+Core+Serde+Decode variants.
Task 3: complete (commit 8e970a7, review clean round 1). Decisions: ENFORCED relations (Task 5 must pre-filter endpoints), nameLower VALUE field, analyzer class+camel/lowercase+ascii, lineKey/colKey ?? -1 unique fold, RETURN meta:schema_version.value.
  Minors carried to final review: (a) db() escape hatch pub #[doc(hidden)] — narrow or feature-gate later; (b) schema types errors/candidates array<object> + edge metadata option<object> stricter than Vec<Value>/Option<Value> — Tasks 4-6 verify inserts don't break; (c) lock-detect substring heuristic fragile; (d) RELATE-level coverage only via calls table — add 12-table RELATE loop cheaply somewhere; (e) FileRecord.size u64→int.
Task 4: complete (commits 5ca3e54..555f1af, review clean after 1 fix round). record::id(id) AS id bridge, UPSERT CONTENT, FOR $batch loop per 500-chunk, DEFAULT [] only-on-create workaround (send [] always). Chunk failure = whole-chunk atomic, no cross-chunk rollback.
Task 5: complete (commit 81dfc6d, review clean round 1). RELATE CONTENT per-kind tables, dup-detect via "already contains" text match (safe: 1 unique index/table; harden w/ _unique substring if ever a 2nd index), batch adjacency = 2 queries total, contains excluded at FROM layer, SELECT DISTINCT in.field unparseable -> Rust sort+dedup.
  Minor carried: string-match hardening (_unique); 500-stmt chunk perf unexercised >500.
Task 6: complete (commits 9bed7c2..f7a7b62, review clean after 1 fix round). replace_file_extraction protocol a-g w/ file-row-last crash safety; SurrealDB 3.2 AUTO-CASCADES RELATE edges on node delete (guarded by raw edge-table count tests); ReplaceStats in store.rs (not on trait yet).
  Minors carried: nodes_inserted = valid input count not DB-confirmed (Task 10); insert_nodes returns () vs trait Result<()> — fine; delete_file 2 round trips.
Task 7: complete (commit 9bff9b4, review clean round 1). FTS weighted additive (20/5/1/2) w/ ?? 0 coalesce; term-joining deviation vs TS prefix-OR documented; languages = FILE count; clear() keeps whole meta table.
  Minors carried to final review: (a) search_fts swallows ALL query errors not just parse (narrow later); (b) unresolved_pending_batch ORDER BY not total (add record id tiebreak before resolver paging relies on it); (c) name-length tiebreak bytes vs chars.
Task 8: complete (commit 43edd19, review clean round 1, fable-reviewed). Frontier-batched hybrid traversals; 34 ported contract tests (all #-regressions verified vs TS source line-by-line). DELIBERATE DIVERGENCE: type_hierarchy returns ancestors+descendants union (TS had latent dead code returning ancestors only; trait doc + map mandate union; test-pinned; 2-line revert possible) — FLAG for Phase 9 parity diffs.
  Minors carried: mid-level cap wastes one batch round trip; per-step NeighborEntry clones; find_path path-clone (parent-pointer later); impact prefetch over-fetch on fan-in heavy graphs.
Task 9b: implemented (commit b937e07) — bulk INSERT RELATION + record-anchored adjacency; measured: edges 10.5k/s, frontier 1.5s→25ms, get_nodes(500) 43→5ms, nodes 0.8k/s WITH FTS idx / 4.9k/s without (FTS dominates node writes; prior-session 21k/30k figures unreproducible). Review: APPROVED w/ 1 Important pending: no test pins Some→None clear on ON DUPLICATE KEY UPDATE (nodes.rs) + doc misattribution "pinned by insert_nodes_upsert_replaces_same_id". Fix queued BEHIND bench-9c run (avoid target/ lock + CPU skew).
  Minors carried to final review: NODE_CONTENT_FIELDS/NODE_FIELDS sync-guard test; edge_record_id computed 2x per edge; .superpowers citations in traverse.rs/schema.rs; RELATE vocabulary leftovers files.rs/store.rs; edges.rs ~900 lines.
Task 9c: bench-9c agent measuring §5.3 gate (mem/surrealkv/rocksdb, 100k corpus) + deferred-FTS remediation evidence; draft results → scratchpad/db-gate-results.md; commit doc at docs/benchmarks/2026-07-phase1-db-gate.md AFTER review fix lands.
Phase 2 plan: drafted (20 tasks) at scratchpad/phase2-selene-extract-plan.md; decisions: replace_file_extraction lifted onto trait in Task 10; EXTRACTION_VERSION=1; SELENE_* env vars; kotlin-ng deviations budgeted. Plan review in flight.
Task 9c: MEASURED (no commit yet — doc lands in 9d). Gate verdict: FAILS as written. bulk 46-706 nodes/s per backend (target 20k); deferred-FTS remediation: 803→4,703 nodes/s, CONCURRENTLY index build 7.6s, total 2.15x; callers_d3 hub-rooted 2.0-2.4s (algorithmic re-visits; callers_d1 48ms incl fetch); FTS 26ms mem/52ms rocksdb vs 20ms; corridor traversals PASS 1.2-4.4ms all backends. surrealkv PATHOLOGICAL on writes (36min/load) → rocksdb default. Draft: scratchpad/db-gate-results.md.
DECISION (user, 2026-07-12, AskUserQuestion): Fix + recalibrate — Task 9d created (rocksdb default + deferred-FTS trait API + frontier pruning + FTS probe + re-measure + finalize benchmark doc w/ recalibrated targets: bulk ≥4k nodes/s deferred-FTS disk). Brief: task-9d-brief.md.
Task 9b review-fix: dispatched to impl-9b (Some→None clear test + doc cite + NODE_FIELDS sync-guard; moved sync-guard OUT of task-10 brief). Sequencing: fix-9b → re-review → 9d → review → 10 → final.
Phase 2 plan: review fixes applied, 21 tasks final, READY. Still in scratchpad (commit to docs/plans/ at Phase 2 kickoff).
Task 9b: COMPLETE (commits b937e07 + fix e9dca38, review clean after 1 fix round). Some→None clear semantics CONFIRMED against engine (green first run). 104 tests.
  Residual minor (reviewer, carried to final review): sync guard pins the two field lists to each other, not to Node's serde shape.
Task 9d: implemented (commits 855445b, 94456cd, 5a5e66f) DONE_WITH_CONCERNS — review in flight. rocksdb default + bulk_load_begin/finish (CONCURRENTLY, polled) + traverse rework (real waste = cross-level payload re-fetch + per-kind pointer scans; brief's re-expansion diagnosis WRONG, pinned by test) + FTS probe adopted nothing (contract-breaking shapes only). Post-fix: bulk deferred 5,875 nodes/s rocksdb (≥4k target MET), callers_d3 1.93s rocksdb (O(result-size) floor: 12,743 entries × ~70-110µs; recalibrated <50ms @ ≲500 entries; ~2x lever deferred: id-derived edges + lazy hydration), FTS 26ms documented. docs/benchmarks/2026-07-phase1-db-gate.md committed. CI rocksdb cold build ~7.5min noted-not-fixed. Bulk numbers 1-repeat.
Task 9d: COMPLETE (commits 855445b..a723cb8, review clean after 1 fix round). Gate recalibrated + met: bulk deferred 5,875 nodes/s rocksdb (target ≥4k), corridor 1.2-4.4ms, hub-rooted <50ms up to ~330-390 entries (extrapolated, attributed), FTS 26ms documented. wait_index_ready bounded 10min/index + pure classifier. docs/benchmarks/2026-07-phase1-db-gate.md final.
  Minors carried: commit 94456cd title carries disproven "re-expansion" diagnosis (history, not worth rebase); worst-case bulk_load_finish 4x10min bound; CI rocksdb cold build ~7.5min unaddressed.
Task 10: dispatching (brief .superpowers/sdd/task-10-brief.md — facade + trait lift of replace_file_extraction + citation sweeps + gitignore).
Task 10: COMPLETE (commit 17a7cf0, review approved round 1). Facade + trait lift + impl GraphStore for SurrealStore wired (53/53 pure delegation, verified) + generic S: GraphStore e2e test + citation sweep (0 .superpowers refs left in src) + gitignore *.iml. 116 tests, doc 0 warnings.
  Minor carried: lib.rs deferral list omits deleteUnresolvedByNode + getUnresolvedReferences names.
PHASE 1 IMPLEMENTATION COMPLETE. Final whole-branch review dispatching.
FINAL WHOLE-BRANCH REVIEW (review-10, 9263196..17a7cf0): READY TO MERGE — 0 Critical, 0 Important. 4 new Minors + reco: (1) schema-version mismatch guard; (2) CHUNK/clamp_i64 dedup; (3) RefStatus restrung in 5 queries; (4) deferral-list names + BFS/DFS note. Triage: #3 RELATE-coverage + #5 _unique + #15 type_hierarchy docs = RESOLVED; #6 #9 #10 #13 = fix-in-Phase-2 (10+13+6 folded into pre-merge wave); rest accepted w/ rationale. Reco: move FileRecord/UnresolvedRef/RefStatus to selene-core pre-Phase-2 (doing now); lazy-hydrate lever → Phase 4 note.
Pre-merge hygiene wave dispatched to impl-10 (2 commits: hygiene + type move). After: re-review → merge to main (--no-ff) → Phase 2 kickoff.
NOTE: pty exhaustion blocks NEW agent spawns — reuse idle agents via SendMessage (works).

=== PHASE 1 MERGED to main: acca72b (2026-07-12). Branch feat/phase1-selene-db kept. 121 tests green. ===

# Phase 2 (selene-extract) — SDD progress
# plan: docs/plans/2026-07-12-phase2-selene-extract.md (21 tasks) · branch: feat/phase2-selene-extract
# Controller decisions baked in: EXTRACTION_VERSION=1; SELENE_* env vars; kotlin-ng deviations budgeted; types FileRecord/UnresolvedRef/RefStatus ALREADY in selene-core (93929f4) — plan's either-way hedge resolved to selene-core side.
# Phase-2-carried debt from final review: narrow search_fts error swallow (w/ error taxonomy); Phase-4 note: lazy-hydrate edge metadata lever (db-gate.md:222-225).
P2 Task 1: implemented (commit 9b0c5b3) — spike 18 tests, 12 grammar pins OK on core 0.26.11. Discoveries for later briefs: kotlin identifiers = `identifier` NOT `simple_identifier` (Task 11 nameField); `fun interface` parses CLEAN (Task 11 drops TS ERROR-recovery); cancellation = parse_with_options + progress_callback + reset() (SELENE_PARSE_TIMEOUT_MS keeps); BOM = KEEP no-strip (line-0 cols +3, ids safe); C# preproc_if native but blanking still needed (Task 12 unchanged). Review in flight.
P2 Task 1: COMPLETE (commit 9b0c5b3, review approved round 1). Spike = permanent grammar-parity fence.
  Minors carried: (a) add assert!(!root.has_error()) to named_kinds/probes (covers MISSING; Task 11 brief — the fun-interface drop decision rests on it); (b) assert C# raw-parse preproc_if/no-ERROR instead of eprintln (Task 12 brief).
P2 Task 2: dispatching to impl-9b (brief task-2-brief.md).
P2 Task 2: COMPLETE (commit 77cf526, review approved round 1 — golden digests independently recomputed by reviewer). ids module: node_id/hash_content/EXTRACTION_VERSION=1 + doc fixes. Minor → rider on Task 3: pin hash_content("\u{FEFF}a\r\n") BOM/CRLF passthrough vector.
P2 Task 3: dispatching to impl-9b (brief task-3-brief.md + rider).
P2 Task 3: COMPLETE (rider bca7b0d + task 1f60f6b, review approved round 1 — EXTENSION_MAP/patterns verified byte-faithful vs TS ground truth by reviewer). 171 tests.
  HOUSE-STYLE DECISION (controller): #[allow(clippy::unwrap_used)] on Regex::new(compile-time literal) in non-test code = ACCEPTED idiom IFF justification comment + regex exercised by a test. Applies to Task 12 blankers etc. Zero-exception alternative rejected (churn > value).
  Notes: #906 overrides + cfquery → wave 2 (documented); regex="1" house pin.
P2 Task 4: dispatching to impl-9b (brief task-4-brief.md).
P2 Task 4: COMPLETE (commit b667e60, review approved round 1). AST helpers; byte-slicing panic-free proof; \s parity verified (FEFF/NEL = complete set diff). 179 tests.
  Riders on Task 5: (a) soften helpers.rs:46 "exactly one declaration" claim (multi-declarator); (b) add U+0085 NEL to the divergence record.
  Carried to Task 7 brief: multi-declarator docstring test when arrow-const lands.
P2 Task 5: dispatching to impl-9b (brief task-5-brief.md, 80 lines — walker core + LanguageRules + Python config + create_node 0→1 line pin).
MODE CHANGE (user directive 2026-07-12 ~20:15): lighter process for remaining tasks — batch dispatches (several tasks per agent, 1 commit/task), PARALLEL implementers in self-created git worktrees for file-disjoint clusters (iterate w/ cargo test -p selene-extract; workspace gate at merge points, merges coordinated by controller), batched reviews per cluster; deep review only for systemic-risk tasks (8 TS machinery, 18 orchestrator, 19 parity gate). walker/mod.rs chain stays sequential: 5→6→7→8→13→15a.
Parallel plan: impl-9b = core chain (5 in flight, then 6→7→8 batch). impl-9d = worktree {16 ScopeIgnore, 17 scan} NOW. impl-10 = worktree {9,10,11} after 5 lands, then {12,14}. 15b after 15a. 18 after 6+17. 19 after all langs. 20 last.
P2 Task 5: implemented (e0ae460 riders + 5fb90cd) — review NEEDS FIXES: 2 Important interface gaps (extract_variables hook absent; Session::visit absent — root cause Session doesn't hold rules). Fix dispatched to impl-9b PRIORITY before its 6-8 batch; impl-10 (p2-langs) told to mirror exact brief signatures if needed. Controller rulings: synthesize_members keeps class_node form (documented deviation); ellipsis >= → > parity fix in same commit.
  Carried: stacked-decorator is_static → Task 19; recursion depth guard → Task 18; Minor python is_static immediate-sibling → verify at gate.
P2 Tasks 16+17: implemented on p2-scan worktree (a54de64 + df65cf4, base e0ae460), 76 crate tests. Outside-brief touchpoints flagged: root Cargo.toml (ignore=0.4, wait-timeout=0.2), extract Cargo.toml, lib.rs re-exports, Cargo.lock, scan/mod.rs stub in commit 1. Deviations: force-include collection → Phase 8 w/ config loader; sorted discovery (determinism rule) vs TS order; data-dir skip = .selene. MERGE DEFERRED until impl-9b's core chain is between commits. Review dispatching now (parallel, pre-merge).
P2 Tasks 12+14: dispatched to impl-9d on NEW worktree p2-blank (base 5fb90cd) — blankers (+C# spike-probe rider) then C#/PHP/Ruby configs. Remaining unassigned: 13, 15a (walker chain, after 8), 15b (after 15a), 18 (after 6+17 merge), 19 (after all langs), 20 (last).
P2 Tasks 16+17: COMPLETE (review approved both, round 1 — DEFAULT_IGNORE_DIRS re-extracted from TS source and confirmed verbatim). p2-scan READY TO MERGE.
  Merge-wave riders (fold at merge + workspace gate): (1) one-line is_whole_cwd_entry guard in collect_git_files untracked pass (latent self-recursion); (2) document-or-align gitignored-root fallback in discover_embedded_repo_roots; (3) GIT_CONFIG_GLOBAL=/dev/null in test git() helper for hermeticity.
P2 Tasks 6-8: implemented (255dcae, f9e798e, f45d3d2 — 236 workspace tests) BUT the Task-5 interface fix was RACED PAST (message crossing) — fix re-ordered to impl-9b post-batch, adapted to evolved walker. Deep review of 6-8 waits for the fix commit (one package). Disclosed concerns for that review: ladder call/inst branches landed in T7 commit not T6; byte-range reader-scope redesign (tree-lifetime-free Session); duplicate method return-type refs + IIFE whole-text callee kept as TS-parity warts; T7/8 tests passed first run (no red).
  LESSON (recurring): idle-notification races — after EVERY dispatch to a busy/just-idle agent, verify uptake before assuming; check git for expected artifacts.
P2 Tasks 9-11: implemented on p2-langs (a831144, 43c4417, ceff677 — 136 crate tests, 6 #[ignore] pending Task-6 merge). Flags: hook-hosted TS-core branches instead of walker insertion points (observably identical — core chain may relocate); rules/mod.rs scope_is_class_like utility (conflict watch); kotlin-ng REAL name field discovery (supersedes identifier hunts); Rust supertrait predicted-id emission; Go embedding + Java field decorators await T7 passes (in-code flags). Review dispatching. MERGE UNLOCKS: un-ignore the 6 tests (T6 on main), revisit T7-dependent flags.
P2 Tasks 12+14: implemented on p2-blank (eb41a6c, ecfca84 — 116 crate tests). Ruby partial per ratified option (b): module hook + CONST gate deferred to post-fix-merge follow-up. Brief count discrepancies (source wins, disclosed): CPP_NON_CLASS_RETURN=28 (brief 27), PHP=18 (brief 19). Staging dead_code allow on cpp_preparse until Task 13. Two C# walker-gap workarounds hook-hosted. Review dispatching.
P2 interface fix: LANDED 7ba47e3 (extract_variables+VariableInfo verbatim TS incl. its no-consumer state; Session::visit + 4-arg create_node, ~20 call sites; synthesize_members deviation doc'd). PUSHBACK ACCEPTED (controller): ellipsis stays >= on truncated — implementer cited 6 TS sites (tree-sitter.ts:2522 etc.), reviewer's >100-on-original would diverge at exactly-100. Reviewer Minor 3 overruled by evidence.
MERGE WAVE: impl-9b = merge captain (p2-scan first: approved + 3 riders + workspace gate). impl-10 merges 7ba47e3 → p2-langs. impl-9d merges 7ba47e3 → p2-blank + Ruby follow-up. Deep review 6-8+fix (5fb90cd..7ba47e3) → plan-review-p2 (fresh reviewer, knows the plan).
P2 Tasks 9-11: COMPLETE (review approved all three, round 1). Hook-hosting verified observably identical (visit_node-first + short-circuit semantics); predicted-id proven end-to-end; kotlin drift ledger gate-ready.
  Merge notes: ALL create_node call sites in the 3 configs are 5-arg → will break loudly at fix-merge (impl-10 handling now, mechanical). Parity-gate confirmations for Task 19: Lombok log field Private vs map self-contradiction; true-returning hooks suppress initializer-subtree descent (re-confirm post-body-walker merge).
P2 Tasks 12+14: COMPLETE (review approved both, round 1 — both count disputes resolved FOR implementer by source recount: plan typos).
QUEUED (after merge captain frees the index): plan correction commit — docs/plans/2026-07-12-phase2-selene-extract.md: CPP_NON_CLASS_RETURN 27→28, PHP_NON_CLASS_RETURN 19→18.
TASK 19 PARITY CHECKLIST (accumulating): Lombok log field Private vs map contradiction; Ruby import_types=["call"] suppresses DSL block-body declaration walks (pin vs TS); PHP class-const visibility unstamped (TS default-public?); true-returning hooks suppress initializer descent; stacked-decorator is_static; python is_static immediate-sibling; kotlin drift ledger entries.
WATCH: Task 13 must remove cpp_preparse staging dead_code allow; C# top-level field degenerate emission (Minor, accepted).
MERGE WAVE progress: p2-scan MERGED (c8c205f + riders 4cd7cb2) — 260 workspace tests, first combined scan+walker build green. Plan typo fix committed (19eda03). p2-blank complete (00c7130 + ec958fc, Ruby follow-up under quick review). p2-langs: fix-merge in progress (impl-10, incl. 5→4-arg create_node sweep + 6 un-ignores). Briefs 13 + 15 extracted, ready.
P2 Tasks 6-8 + fix: COMPLETE (deep review approved all + ratified ellipsis pushback). 1 Important ARMED BY MERGES: body.rs:186-210 object+name branch partial — missing field_access(this,fld) receiver unwrap (Java this.x.m() → must emit x.m) + parent/static in skip set (PHP parent::m()). Port ordered in merge wave. Minors: ts_core v0-gate subsetting convention; cap/env tests → Task 19 note; sequencing logged.
Ruby follow-up ec958fc: APPROVED. Trivia → T19 checklist: module docstrings NodeExtra::default parity; containment assert >=2 not ==2.
p2-langs: fix-merged (6ce0b1b, 184 tests 0 ignored; Java anon-classes wired by filling Task-10 insertion markers — NEW UNREVIEWED code, quick review after mega-merge). extract_inheritance insertion point STILL UNFILLED (marker says Task 7?) — Go embedding + Java field decorator refs pending; verify owner (13/15a?) at pre-gate sweep.
MEGA-MERGE dispatching: captain merges p2-blank then p2-langs + ports body.rs Important + workspace gate.
INTERRUPTION (session limits, reset 00:10): impl-9b killed mid-p2-langs-merge (rules/mod.rs UU, rest staged; p2-blank merge e7fd651 COMMITTED before). impl-10 killed before creating p2-orch. Both resumed post-reset with precise state briefs (2026-07-13 ~00:15).
MEGA-MERGE COMPLETE: e7fd651 (p2-blank) + 429dfcb (p2-langs, rules/mod.rs union 10 modules) + 5dbeaa9 (this-unwrap + parent/static skips w/ TS two-distinct-skip-sets detail honored). Workspace 389 tests 0 failed — first all-ten-languages build. Branches p2-scan/p2-blank/p2-langs fully merged.
  STILL ARMED (rider on Task 13): PHP scoped-call + Java method_invocation chained-factory inner().method re-encodes (TS 4073-4160) at marked insertion point.
  PENDING QUICK REVIEW: anon-class wiring in 6ce0b1b (impl-10's manual merge work) — dispatching to review-10 via git show combined diff.
Remaining: 13 (+riders), 15a, 15b, 19, 20. Task 18 in flight (p2-orch).
Anon-class wiring (6ce0b1b): APPROVED — TS 0-based-row quirk confirmed at tree-sitter.ts:4733, ported+documented; gate/sweep/no-weakening all verified. ENTIRE merge wave now review-clean.
In flight: Task 13 (impl-9b, nudged after idle race), Task 18 (impl-10, p2-orch).
P2 Task 13: implemented (ffac60f + both riders — 404 workspace tests). Notes: extractNameRaw/extractVariable C tails ported (genuine red); CPP set = 28 verified vs source; C++ getVisibility first-specifier coarseness kept+doc'd; C++ stack-construction #1035 + local fn-pointer rewrite = marked insertion points → GATE DEVIATIONS LEDGER. Review dispatching. Task 15a dispatching to impl-9b (walker chain).

=== MODEL SWITCH (2026-07-13 ~00:20): Fable 5 quota exhausted → fleet relaunched on OPUS 4.8. Old agents (impl-9b/9d/10, review-9d/10, plan-review-p2) are DEAD (fable limit). New agents: fix-18, rev13, impl-15a (opus). ===
P2 Task 13: implemented ffac60f (404 workspace tests) — review in flight (rev13).
P2 Task 18: implemented 8422dce on p2-orch — review: NEEDS FIXES. Important: scan-failure early return skips bulk_load_finish → store left in bulk mode w/ FTS indexes DROPPED = silent empty search. Minors: Indexer::new expect (try_new); ParserError store-mapping doc at definition; progress arithmetic drift w/ read failures. Fix in flight (fix-18, p2-orch worktree). Review verdict otherwise strong (concurrency structurally sound, contracts exact, depth guard pins the guard not stack luck).
P2 Task 15a: dispatched (impl-15a, main checkout — fnref machinery + gate C/C++/TS/Python).
REMAINING: 15b (after 15a), 19 parity gate (brief ready), 20 facade (brief ready), merge p2-orch, final branch review.
P2 Task 13 REVIEW: NEEDS FIXES — CRITICAL: extract_type_alias insertion point never implemented (marker still live at walker/mod.rs:1049-1088): anonymous typedef struct/enum mints phantom <anonymous> node + members get unmatchable QNs (<anonymous>::OK vs TS status_t::OK); DIVERGENCE FROZEN IN ACCEPTED SNAPSHOT (lang_c_test snap:1267-1310). TS: ts:2861-2885 (enum)/2840-2859 (struct) push scope, walk body, return true.
  Minors: Java rider assertion half-vacuous (Foo.getInstance().bar byte-identical pre-rider); C++ file-scope `Foo x;` mints nothing (TS ts:2802-2818 does).
  VERIFIED BY REVIEWER (not defects): 28-entry set correct; C++ getVisibility first-access-specifier IS TS behavior (ts:741-756); cross-language walker-seam regression risk CLEAN (all gates language-scoped; Rust first-identifier fallback = convergence toward parity).
  Fix dispatched: fix-13 on NEW worktree p2-cfix (off ffac60f) — main checkout held by impl-15a. LESSON: "snapshots reviewed" claim missed a divergence baked into the baseline — snapshot acceptance needs content verification, not just diff review.
P2 Task 18: COMPLETE (8422dce + fix e7ac2a0, re-review APPROVED — leak structurally impossible verified path-by-path: zero early returns after bulk_load_begin, all store faults collected; begin-failure restore masks nothing; hoist safe (scan has no store refs); test non-vacuous; 3 minors closed incl. zero expect() left in file). p2-orch READY TO MERGE (after main checkout frees / 15a lands).
P2 Task 15a: COMPLETE (e9134b1, review APPROVED — every gate rule traced to its TS line; visit-exactly-once verified across all 5 scan sites; function_ref-never-an-edge structurally impossible; C++ &-only correct). 2 minors → riders on 15b (C++ rhs/varinit tests; dedup comment).
REAL GAP FOUND BY 15a REVIEW → fix-t7 dispatched (worktree p2-t7fix off e9134b1): extract_property never walks field value → TS/JS class-field initializers emit NO calls/instantiates edges at all (TS ts:1001-1008 walks it). Would have polluted the Task 19 parity gate as a phantom "15a regression".
P2 Task 15b: in flight (impl-15a, main checkout, fnref.rs spec rows only + 2 riders).
PENDING MERGES: p2-cfix (fix-13, Critical typedef), p2-orch (APPROVED), p2-t7fix (in flight).
P2 Task 13: COMPLETE (ffac60f + fix 2e4662b on p2-cfix, re-review APPROVED — Critical closed at root: phantom <anonymous> gone, members re-parented to status_t::OK, no new bug frozen in snapshot; cross-lang verified: TS/Rust/Kotlin take untouched plain-alias path, Go struct_types empty so net-identical; C++ `Foo x;` ported gated-to-Cpp; Java rider test now discriminating). p2-cfix READY TO MERGE.
  Tracked (not blocking): extractInheritance on inner typedef node = pre-existing Task-7 insertion point (Go struct embedding edges unwired) — GATE DEVIATIONS LEDGER.
P2 Task 15b: implemented 3776a72 — review in flight (rev15b). ALL 21 IMPLEMENTATION TASKS NOW WRITTEN.
P2 Task 7 gap fix: implemented a47db95 on p2-t7fix (class-field initializers → calls/instantiates edges).
FINAL MERGE WAVE dispatched (merge-captain, primary checkout): p2-cfix → p2-orch → p2-t7fix, snapshot re-accept w/ content verification, full workspace gate, worktree cleanup.
NEXT: Task 19 parity gate (brief ready, deviations checklist accumulated in this ledger), Task 20 facade (brief ready), final branch review, merge to main.
P2 Task 15b: review NEEDS FIXES. Spec rows parity-exact all 7 langs (HOF list 27/27 exact; Java positions+wrapper right; C# += ; validates excluded). BUT: (1)+(2) both ordered riders LOST IN A RESET — absent from 3776a72 (C++ &-form rhs/varinit tests; walker dedup comment). (3) REAL BUG beyond fnref: Ruby import branch (import_types=["call"], walker matched=true → returns w/o recursing) vs TS ts:1173-1175 which keeps walking ⇒ ALL Ruby DSL block bodies (Rails callbacks, RSpec) lose declarations AND class-scope calls refs; 3 hook tests shipped #[ignore]d. Earlier review had guessed "almost certainly TS-parity" — WRONG, reviewer verified against source. Fix dispatched to impl-15a (worktree p2-rubyfix off merged head).
  Minor: kotlin Foo::class negative test.
MERGE WAVE COMPLETE: 99c231d (p2-cfix) + 1d4acf2 (p2-orch) + 9b007b6 (recovered C++ rider tests) + e3aafe6 (p2-t7fix). Workspace 508 tests green. Merge captain flagged IDE writing buffers + concurrent parity agent (correctly left untouched).
IN FLIGHT: Task 19 parity gate (fix-t7, primary checkout — fixtures + dump-ts-extraction.mjs already appearing); Ruby import-branch fix (impl-15a, p2-rubyfix); Task 20 facade (merge-captain, p2-facade).
Task 19 INCIDENT: fix-t7's parity work (fixtures/parity/ + tools/parity/dump-ts-extraction.mjs, untracked) was DESTROYED — another process cleaned the primary checkout; no commit existed. Agent unresponsive. RE-DISPATCHED as gate19 on ISOLATED worktree p2-gate (off e3aafe6) with instruction to commit early/often. LESSON: never let an agent do multi-hour work untracked in a shared checkout.
P2 Ruby fix: 97fb258 on p2-rubyfix — diagnosis sharper than expected: Ruby import_types==call_types==["call"] + imports tested first ⇒ EVERY class/file-scope Ruby call landed in the import branch and consumed its subtree. Fixture: 3 nodes/0 refs → 4 nodes/2 refs. Re-review in flight (rev15b).
Task 20 facade: in flight (merge-captain, p2-facade).
P2 Task 15b + Ruby fix: COMPLETE (3776a72 + 97fb258/f074135 on p2-rubyfix, re-review APPROVED — 387 tests 0 ignored; cross-lang inert, 7 plausible import carriers hand-checked; riders all present). p2-rubyfix READY TO MERGE.
  T19 NOTE (parity, not a bug — route to gate): class/file-scope Ruby calls still land in the import branch and never reach the call branch (TS shadows identically, same else-if order) ⇒ no calls refs for class-scope `helper_method inner(deep())`. The fix recovered DSL-block-body declarations + their bodies' refs + hook fn-refs, NOT class-scope call refs.
TASK 19 GATE DELIVERED (28065ab on p2-t19gate — fix-t7 was ALIVE, recovered its work into a worktree; gate19's parallel p2-gate is a partial duplicate, halted & repurposed as gate auditor). Gate is REAL and RED ON PURPOSE:
  Baseline: codegraph 9ccf5d022cdc4c5f33f2cd374f23fa70401e62f0 → 140 nodes / 114 edges / 114 refs over 31 fixtures / 13 langs. Vacuous-baseline trap CLOSED + sabotage-tested. Loader deviation: vite-node (npx tsx impossible — web-tree-sitter Emscripten artifact breaks Node ESM lexer).
  RESULT: 7 languages EXACT; 18 diffs / 6 clusters = 4.3% > 2% stop rule → agent correctly STOPPED and surfaced.
  6 REAL BUGS: (1) PHP inheritance entirely unwired (3 refs); (2) PHP typed properties → no field node (1n+1e+2r); (3) C# type-annotation refs missing (4 refs); (4) C# record base lists unwired (2 refs); (5) Java field-type ref missing (1 ref); (6) Ruby require_relative resolved-path ref missing (1 ref). Shared root: walker extract_inheritance insertion point never filled.
  1 JUSTIFIED DEVIATION: C++ case where TS is wrong / Rust right (under audit).
  Dispatched: fix-parity (fixes all 6 on p2-t19gate, gate must go GREEN); gate19 (adversarial audit of the gate itself).
Task 20 facade: 85b92eb merged (629b35b). Ruby fix f074135 merged. Worktrees cleaned except gate ones.
GATE AUDIT (gate19 — the "duplicate" turned out to be the phase's best find): mechanism SOUND (non-vacuity verified in code: real extractor import, initGrammars before extract, exit-1-no-write on 0-node/error, Rust-side non-vacuity re-assert; NOT totals-only — every per-kind counter diffed; synthetic-mismatch test genuine; fixtures byte-checked; vite-node reasoning independently reproduced).
BUT COVERAGE HOLE — would have greened a BROKEN contract:
  CRITICAL: named-class extends/implements refs extracted in NO language (only emitters: rust_lang supertraits, body.rs ANON classes, php/ruby trait-mixin `use`). 31-fixture corpus has ZERO heritage in TS/TSX/Java/Kotlin/C++/Python. Auditor's 51-fixture corpus measures: cpp 5 extends TS/0 Rust; ts implements 1/0; tsx 1/0; java 1/0; kotlin 1/0. Phase 3's interface→impl bridge would have had NO input.
  HIGH: "Rust never over-emits" FALSE — Python @app.route emits bogus decorates:route (TS doesn't).
  +3 bugs: PHP namespaced-use 2nd spelling; Ruby constant refs (JSON.parse → references:JSON); the Python over-emission.
  MEDIUM: harness iterates baseline.files only ⇒ on-disk fixture absent from expected.json is SILENTLY UNGATED (set-equality assert needed — critical as corpus grows).
  C++ deviation REAL but mechanism mis-stated (TS treats ANY type_identifier in class body as base — verified by repro). C# record base name: TS emits malformed `SimplePositional(A)`; count-gate blind to names, correct name still passes.
All relayed to fix-parity; gate19's 51-fixture corpus (worktree p2-gate) to be harvested, not re-invented.
INHERITANCE GAP CLOSED (9bb224d): extract_inheritance wired for 9 language arms (extends family ts:5198-5274; implements family ts:5302-5324; C++ base_class_clause; Kotlin delegation_specifier; Python class argument_list; Go struct+interface embedding; JS class_heritage). Corpus 31→41 fixtures. TS 202n/167e/139r vs Rust 202n/167e/138r — the -1 = C++ deviation, now with ROOT CAUSE FOUND: TS's Go-struct-embedding arm is NOT language-gated and C++ spells members `field_declaration` too, so TS reads a member's RETURN TYPE as an embedded base (phantom extends:Widget). Gated to Go in Rust; "silent beats wrong". Legit no-ops documented (C: no inheritance; TS interface-extends: TS emits nothing either — verified).
PARITY FINAL (ebaa04f): corpus 41→68 fixtures (audit corpus harvested). Gate now 8 assertions: + every_fixture_on_disk_is_gated, + language_detection_agrees, + NAME parity (dumper emits sorted kind:name multisets; deviations.toml gains [[name-deviation]]; both differs self-tested).
  FINAL: TS 380n/325e/299r vs Rust 380n/325e/297r — every counter AND every symbol NAME matches; -2 = C++ phantom bases TS invents.
  NAME GATE PAID FOR ITSELF: found Ruby call truncation the count gate was blind to — `@db.query(id)` emitted `calls:@db` (receiver taken as callee, METHOD DROPPED), counts matched throughout. Ported full TS branch (receiver.method, Foo.new→instantiates, self/super collapse, constant receiver → references:JSON).
  Other bugs: Python decorator OVER-emission (bogus decorates:route — a pre-existing test had PINNED the bug); PHP `use` must also emit namespace-qualified spelling (Foo\Bar::Baz — the only form that resolves).
  2 deviations, both TS-is-wrong: C++ phantom bases (root cause widened: ANY member whose declarator nests its name + bare type_identifier type — return type OR pointer field); C# record base name (count-equal → [[name-deviation]]).
  lib.rs deviation ledger added (CLAUDE.md's claim made true).
GATE MERGED (ec7bacf). Final: 69 fixtures, TS 386n/330e/300r vs Rust 386n/330e/297r (-3 = TS refs that can never resolve: 2 C++ phantom bases + enum "inheriting" byte). 3 deviations + 1 grammar-drift, all machine-checked (stale entry fails build). lib.rs conflict resolved: facade's stale deviation list REPLACED by gate's machine-checked ledger (Go embedding etc. are now WIRED — the old list lied). Workspace 550 tests, 0 failures.
PHASE 2 COMPLETE — final whole-branch review dispatching.

=== PHASE 2 MERGED TO MAIN: ca39a80 (2026-07-13). 30k lines, 550 tests, parity gate green (count + name). ===
Phase-3 inputs (from final review): (a) Ruby class/file-scope calls land in import branch, never reach call branch — TS-identical, but a real coverage hole the resolver will feel; (b) UnresolvedReference shape verified sufficient (name_tail, candidates, status, denormalized file/lang; chained-call markers + ::/. spellings as specified); (c) zero cross-file edges verified branch-wide (exactly 3 Edge sites, all intra-file); (d) function_ref never an EdgeKind (structurally impossible).
Phase-1 debt for later: search_fts swallows all query errors; lib.rs deferral list omits deleteUnresolvedByNode/getUnresolvedReferences.

# Phase 3 (selene-resolve) — planning
Two whole-plan agents DIED to mid-response connection failures (nothing written both times, ~1.5h each). LESSON: a single agent composing a 900-line plan in one response is fragile. Split into 3 parallel part-writers, each: (a) own output file, (b) MANDATORY incremental write (create file early, append per section — never compose long docs in one response).
  p3-partA → phase3-partA-core.md (front-matter + Global Constraints + file structure + sequencing note; spike, pass ordering, imports per ecosystem, name-matcher, chained-call, fn-ref RESOLUTION half)
  p3-partB → phase3-partB-frameworks.md (12 frameworks + 5 synthesizers; end-to-end-or-nothing invariant per task)
  p3-partC → phase3-partC-gate.md (resolution parity gate — edge IDENTITIES not counts, fixture-set assert, language assert, sabotage-tested dumper, machine-checked deviations, dispatch-coverage whole-chain fixtures; facade; self-review checklist)
Controller assembles + renumbers, then dispatches a plan review before execution.
PHASE 3 PLAN DECISIONS (user, 2026-07-13):
1. ROUTE NODE IDS — user asked to rethink & leverage SurrealDB. DECISION: route ids stay HASHED (Phase 2 contract, no new exception). Route SEMANTICS move out of the id string into FIRST-CLASS INDEXED FIELDS on the route node (method, path, file, line, framework). All downstream matching = indexed SurrealQL queries, never id-string parsing/key-matching. Parity gate compares SEMANTIC identity (method+path+file+line) not raw id spelling — it compares what ids mean, not how TS spells them. Consistent with the locked SurrealQL-max decision. Requires: selene-db schema addition for route fields + indexes (a small Phase-3 task, or extend the node table).
2. NEXT.JS — INCLUDE in Phase 3 (~30 lines, React fixture corpus covers it free).
3. SPRING CONFIG — AUTHORIZED: add Yaml/Properties as file-level-only languages to selene-extract's Language enum, so the config-key → @Value bridge is end-to-end (partial coverage would violate the hard invariant).
PHASE 3 PLAN REVIEW: READY WITH FIXES — 8 blocking, ALL seams between the 3 parallel-written parts. Reviewer: "Part B's entire Assumed-from-Part-A block was never reconciled and almost every line is wrong against what Part A defines; a fresh executor stalls on the first impl." Also: Language typing forces a DEPENDENCY CYCLE (Part B types traits with selene-extract's Language while Part A has it as dev-dep only and models language as &str; Task 11 made selene-extract depend on selene-resolve — backwards layering — and punted the cycle to the executor). SynthPass not object-safe as specified (generic method + RPITIT ⇒ no &'static dyn) so its registry can't compile. Two dispatch gates + two corpora shipped side by side (Task 27's find_path+via mechanism is satisfiable by ANY route around the dispatch hop — unsound; Task 32 supersedes).
CONTROLLER DECISIONS: D1 Language + LANGUAGE_FAMILY → selene-core (shared wire concept like NodeKind; kills the cycle at root; selene-extract re-exports). D2 framework registry lives in selene-resolve (selene-extract must NEVER depend on selene-resolve — pipeline is extract→resolve). D3 SynthPass = monomorphized run_synthesis<S> over a fn-pointer table (keeps GraphStore generic).
GATE VERDICT (good news): Task 31/32 CANNOT pass while comparing nothing — two-way multiset diff on (src,dst,kind,provenance,resolvedBy,synthesizedBy) + metadata + unresolved halves; all 4 Phase-2 structural asserts carried; baseline_is_not_vacuous goes FURTHER than Phase 2 (≥1 edge per resolvedBy value); 4-perturbation differ self-test. Route-id decision leaves NO uncomparable edge class (semantic label + a test guarding both failure modes).
Fixes dispatched to p3-partA (owns the core interfaces).
PHASE 3 PLAN: READY (re-review clean — all 8 seam defects closed at root, 0 new defects; final nit Language::from_wire added). 33 tasks, committed 43d820f + from_wire fix. Branch feat/phase3-selene-resolve.
  Sequencing (from the plan, ENFORCE): selene-core/lib.rs → tasks 2→11→16 STRICTLY SEQUENTIAL (wire-contract changes; re-run -p selene-extract AND -p selene-db before each commit). selene-db store/store_impl/schema → 11→21 STRICTLY SEQUENTIAL (a parallel merge silently drops a DEFINE FIELD). selene-extract/language.rs → 2→16.
  EXECUTION ORDER: 1 (spike) → 2 (skeleton + Language move) → 3-10 core (sequential-ish) → 11 (framework registry + route fields + EXTRACTION_VERSION bump — GATES everything downstream) → 12-20 frameworks (parallelizable in worktrees, disjoint files) → 21 (synth harness, after 11) → 22-26 synthesizers (parallelizable) → 27-33 gates + facade.

# Phase 3 execution
P3 Tasks 1-2: implemented (39a4548 spike + 67e301b skeleton). Language moved to selene-core WITHOUT breaking Phase 2: parity gate 9/9 green, workspace 589 tests 0 failures. Review dispatching.
P3 Tasks 1-2: COMPLETE (39a4548 + 67e301b, review APPROVED — 25/26 ResolutionContext signatures identical to plan; the 1 rename was a CORRECTION backed by measurement). Language moved to selene-core; Phase 2 parity gate survived (9/9).
  SPIKE FOUND 2 REAL PHASE-1 BUGS (both silent, both merged to main, both past a full branch review + 550 tests):
  F1 DATA LOSS: unresolved rows keyed by 2-tuple (from, name) but extraction legitimately emits BOTH a calls row AND a function_ref row for one pair (`register(handler); handler();`). Resolving one DELETED the other — and silently: the row was GONE rather than failed, so the pending count still reached 0 and the orphan sweep was satisfied. mark_failed flipped both; retryable_failed's 2-tuple dedup then collapsed them → 2 rows in, 1 out, second kind unreachable through EVERY door.
  F2 DEAD GUARD: the count primitive counts distinct FILES not nodes (2001 nodes named `get` across 201 files → answers 201), so AMBIGUOUS_NAME_CEILING=500 could never fire.
  FIXED + MERGED (bcbf014 key→(from,name,kind); 49840ad count_nodes_named). Workspace 681 tests green.
P3 Tasks 3-6: implemented (21e3d79 ladder, a6d4f51 import inputs, 084ebac import-path, 73734b8 resolve_via_import). Review in flight. Tasks 7-10 dispatched.
P3 Tasks 3-6: COMPLETE (review APPROVED — ladder order + 3 ecosystems spot-checked verbatim vs map incl. tie-breaks a pressured port flattens: Python never admits Method as import leaf while Rust does; JVM tie prefers the `expect`-decorated node). Riders done (store-error counter on the TRAIT, block_on pinned by test, cache unwrap removed).
  Minor carried: all ladder/import tests run against FakeContext; no StoreContext-over-real-SurrealStore test (the Phase-2 failure mode). Rider given to Task 11.
P3 Tasks 7-10: implemented (1f30156 name-matcher, c0a51d5 method-call, acda381 chained-call, aaee2c3 fn-ref resolution). RESOLVER CORE COMPLETE. Workspace 772 tests green. Review in flight.
P3 Task 11 (framework registry — gates 12-20): in flight (p3-partB, worktree). Carries route fields on selene_core::Node (Option + skip_serializing_if so Phase 2's serialized shape stays byte-identical), the selene-db schema+indexes, the phase's ONLY EXTRACTION_VERSION bump, and the E2E-through-real-store rider.
P3 Task 11: COMPLETE + MERGED (c9d96f1 + collision fix). Route fields on selene_core::Node did NOT break Phase 2: parity gate 9/9 green, snapshots unchanged (Option + skip_serializing_if did its job). Workspace 792 tests. Frameworks 12-20 now UNBLOCKED (parallelizable — disjoint files per framework).
  NOTE: parallel Tasks 7-10 vs 11 produced one mechanical collision (Node literal in a test helper). Cheap, but confirms the sequencing table's value.

=== P3 GATES: THE BIGGEST CATCH OF THE PHASE ===
The resolution parity gate (a2d4411) found that ladder STEP 8 (resolve_via_import) was INERT IN PRODUCTION — StoreContext::import_mappings() and re_exports() were STUBS returning empty vectors. An empty mapping list doesn't fail; it silently no-ops the entire import-resolution step for every project, every language: Go cross-package (#388), JVM FQN (#314), barrel/renamed re-exports (#629, and with them the pre-filter's matches_any_import escape — a renamed re-export's ref was DROPPED before any strategy ran), tsconfig aliases, workspace packages, Python module members, Rust crate:: paths, C/C++ includes, whole-module→FILE arm.
EVERY strategy test passed throughout, because FakeContext injects the mappings directly. Tasks 4-6 = three commits of code that had NEVER ONCE RUN in production.
THE LESSON (record it): a seam that returns "nothing found" is indistinguishable from a seam that works and found nothing. Only a gate driving the REAL context through the REAL store can tell them apart. The reviewer flagged the FakeContext-only gap TWICE (Tasks 3-6 and 7-10 reviews) — it was right both times, and only the gate proved it.
Dispatch-coverage gate: GREEN, all 11 flows closed end-to-end; carries an honestly-named test `the_synthesizer_half_of_this_gate_is_not_built` so the missing half is declared, not hidden.
P3 GATES BOTH GREEN (edf25cb + 5ba7bb2): parity 303 edges tolerance 0 over 18 projects (175 contains, 55 imports, 39 calls, 31 references, 3 instantiates; 298 tree-sitter + 5 heuristic — all 5 synthesized channels matching). Coverage gate: all flows closed incl. all 5 synthesizers.
  THIRD INERT SEAM FOUND BY THE GATE: run_synthesis was called from NOTHING but its own tests — the 5 channels existed, unit tests green, no pipeline invoked them. Same as import_mappings (step 8, 3 commits dead) and the 4 project singletons. THE GATE CAUGHT ALL THREE, each the same way: TS emitted an edge, we did not.
  Wiring insight worth keeping: synthesis runs LAST (after base edges persist — every pass correlates nodes with edges the ladder produced) AND with the context's caches dropped first (they predate those edges; a stale cache makes every pass a silent no-op that LOOKS like it ran).
P3 BATCH DRIVER (e8892bf): built after the whole-branch review found the FOURTH inert seam — src/batch.rs did not exist, so run_framework_extract/run_synthesis/both conformance passes/delete_resolved/mark_failed had ZERO production call sites (every caller a test), and the #760 keyed-delete contract had NEVER executed against a store. The gates composed the pipeline themselves → structurally blind to it. CONTROLLER ERROR: my dispatch of "Tasks 27-31 = the two gates" shifted the numbering and dropped Task 27 (the driver).
  BOTH GATES STILL GREEN UNDER THE REAL DRIVER: parity 5/5 (303 edges, tolerance 0), coverage 6/6. Workspace 979 tests, 0 failed. NO fifth seam.
  Driver encodes: fixed pass order (framework-extract → StoreContext::new → ladder → conformance → clear_caches → synthesis LAST — each constraint fails SILENTLY if broken, so each is documented at its site); offset-0 batch loop (processed rows LEAVE the pending set, so an advancing offset steps over rows that shuffled down); non-progress guard (a mutated reference_name makes the keyed delete match nothing → row stays pending → infinite re-resolve; the 5M-edge/1.4GB incident).

=== PHASE 3 MERGED TO MAIN: ba29336 (2026-07-13). 979 tests, both gates green through the driver. ===
Phase-4 inputs: driver (resolve_and_persist_batched) is selene-graph's entry point — it inherits the pass order, offset-0 loop and keyed delete instead of re-deriving them. ResolutionStats carries store_read_errors (a store outage can't masquerade as "nothing resolved").
Phase-4/6 carried: run_post_extract wiring; src/sync.rs (scoped re-resolve, failed-ref retry, orphan sweep) → Phase 6; Spring config-bridge real-store test.
Accepted permanently: Gin group-prefix omission (TS parity); Laravel/Rails class fallback (map's "method first, class fallback").

=== PHASE 4 COMPLETE (Tasks 5-13), branch feat/phase45-graph-context-mcp @ 0346c83. 1073 workspace tests, 0 failed. ===
Tasks 1-4 (selene-graph: query/adjacency/symbols/source) + 5-13 (selene-context: relevance, find_relevant_context,
ContextBuilder, EXPLORE BUDGETS, Flow section, dynamic+polymorphic boundaries, explore pipeline, source rendering, THE GATE).
Phase-4 gate GREEN — 7 tests, and they test the PRODUCT not the code: the_gate_corpus_is_a_real_resolved_graph /
every_file_section_contains_real_numbered_source_lines / a_flow_renders_with_numbered_steps_and_the_dynamic_hop_is_named /
real_output_never_exceeds_the_externalization_ceiling / no_output_string_tells_the_agent_to_read_or_grep /
a_planted_secret_never_reaches_the_output / the_blast_radius_section_exists.
Review of Tasks 5-13 dispatched to rev13 (package .superpowers/sdd/review-96adc2d..0346c83.diff, 10 commits) — its
first charge is "can this gate pass while proving nothing", since Phase 2's snapshot gate once stayed green while a
snapshot had frozen a bug as correct.

PHASE 5 (Tasks 14-19) IN FLIGHT (p3-partA): rmcp server + THE REAL BINARY (index, serve --mcp) + 7 tools + explore/node
handlers + search/callers/callees/impact/files + the isError discipline. The trap is its own spike's finding:
Err(ErrorData) becomes a JSON-RPC -32603 TRANSPORT failure, NOT isError:true. Never let a `?` on a store error escape a handler.

TASK 20 (THE MILESTONE GATE) — facts pre-measured, see .superpowers/sdd/task-20-facts.md:
  LARGE-TIER REPO = **VS CODE, not Django**. Measured 2026-07-13: Django = 2,926 .py — BELOW the 5000 bar, and the plan
  says verbatim "if the chosen repo indexes below 5000 it is the WRONG REPO for this row: swap to VS Code ... Do not
  soften the row to fit the repo". VS Code = 11,938 .ts/.js/.tsx/.jsx. Both cloned depth-1 as siblings (../django, ../vscode).
  VS Code flow question traced by hand in the clone, every hop verified to exist AND connect:
  keypress → AbstractKeybindingService._dispatch(:221) → _doDispatch(:279) → _getResolver().resolve(:311)
  → this._commandService.executeCommand(:367) ⚠ INTERFACE HOP → CommandService.executeCommand(commandService.ts:52)
  → _tryExecuteCommand(:92) → CommandsRegistry.getCommand(commands.ts:129) → handler.
  Chosen BECAUSE of line 367: `_commandService` is typed ICommandService, so that hop exists in our graph ONLY if Phase 3's
  dynamic-dispatch synthesis bridged it. If synthesis missed it, explore renders `_doDispatch → ?` and the agent Reads —
  the exact failure the sufficiency invariant forbids. If the flow comes back broken that is a REAL FINDING about the
  product; do NOT swap in an easier question to get the gate green.

Phases 6+7 detailed plans being written in parallel (plan6 = CLI/daemon/sync, plan7 = installer) so there is no gap at merge.

=== 2026-07-13, LATE: THE BINARY RAN FOR THE FIRST TIME. Two findings, one fixed, one open. ===

**ENVIRONMENT (fixed):** `target/` had reached **149 GB** (111 GB in debug/deps — cargo never GCs stale
artifacts across many agents × feature permutations). Disk hit 100%. macOS swaps to disk, so swap could
not grow → memory looked exhausted → fork() failed ("Device not configured") → agent spawns died and a
build failed ENOSPC. I misdiagnosed it as memory pressure and nearly started killing processes. Deleted
target/debug + target/doc → 150 GB reclaimed. ALSO: the Phase-5 agent was not slow, it was STUCK — its
builds had been failing against the full disk for an hour. Killed 26 dead Phase-1/2/3 agents (2.7 GB).
Recurrence guard: `cargo clean` periodically; the 149 GB will come back.

**PERF (FIXED, 81c2437 — 6x): the binary ran on the backend its own benchmark rejected.**
`selene index`, 328 files: **8m52s → 1m29s**; CPU 36% → 100% (it was STARVED, not busy); output
byte-identical (5,035 nodes / 17,216 edges / 11,875 refs — nothing traded for the speed).
Phase 1 measured SurrealKV at **46 nodes/s vs RocksDB 706** ("pathologically slow on this write load")
and made RocksDB the default. Then `selene-mcp/Cargo.toml` said `features = ["kv-surrealkv"]`, and
**Cargo UNIFIES features across the dep graph** — that one line reached three crates away into the
binary and silently overrode the default for the whole product. `SurrealStore::open` prefers SurrealKV
whenever it is merely COMPILED IN (surreal.rs:93 vs :106) — **compiling it IS choosing it**, so removing
it from `selene`'s own manifest did nothing while `selene-mcp` still asked. Three phases missed it
because every gate runs on kv-mem and NOTHING HAD EVER RUN THE BINARY.
  Remaining perf lever: 100% CPU on ONE core — `resolve_all` is a sequential `for r in refs` with ~9
  idle cores. VS Code extrapolates to ~54 min (acceptable as a one-time Task-20 fixture cost; poor UX).
  Parallelizing must not break edge-order determinism (parity gate = tolerance 0).

**⚠ OPEN — THE ONE THAT MATTERS. The product RUNS and DOES NOT ANSWER.** (agent `relevance` on it)
Full slice works: index → serve --mcp → real MCP stdio → explore → 9,737 chars, isError:false, real
numbered source, blast radius. But asked Task 20's exact question — "how does an unresolved reference
become a graph edge" — it returns **0 of 4 required symbols, 0 of 2 required files, and NO Flow section**.
Seeds: `graph_outcome` (an MCP ERROR HELPER), `match_reference`, `unresolved_content` — matched because
the query contains the words "graph" and "unresolved". An agent would get handlers.rs and immediately
Read batch.rs: the exact failure the sufficiency invariant forbids.
  Probes (via /tmp/ask.sh, drives the real binary): naming the symbol outright DOES find it + batch.rs
  ⇒ **the graph is good; this is not an extraction/resolution bug**. "how are edges created during
  resolution" returns `insert_edges` THREE TIMES ⇒ a seed dedup bug. **Flow NEVER renders — not even
  when the right symbol IS among the roots.**
  Diagnosis: `render_flow_section` (builder.rs:111-133) is CORRECTLY wired and honestly declines to
  fabricate a spine ("A fabricated spine is worse than none" — keep that). It starves because relevance
  feeds it lexically-similar but UNCONNECTED seeds. Fix relevance ⇒ Flow follows. Likely gap: we score
  name-similarity but not GRAPH CONNECTIVITY — a connected call chain must outrank 3 unconnected matches.
  ⚠ **phase4_gate.rs PASSES while this is broken** — it runs on planted fixtures. Its passing is the
  thing to be SUSPICIOUS of, not to trust. This is exactly rev13's "the gate corpus is 2 projects, both
  TS-shaped" finding, now proven in the product instead of argued in a review. rev13 was right.

PLANS 6+7 COMMITTED (78b4972) with all 20 open questions ruled. Best catch: **the daemon's reason for
existing changes on Rust** — the map's "CLI commands open the DB directly" is a *SQLite* fact; SurrealDB
embedded takes an EXCLUSIVE lock, so ported literally `selene status` cannot run while an editor is
attached. Ruled daemon-as-arbiter, but Task 1's spike must RATIFY the premise by measuring what a second
process actually gets; if concurrent reads work, the ruling is VOID.

=== PAUSE (reboot machine). État figé. Lire RESUME.md à la racine du repo. ===
Branche feat/phase45-graph-context-mcp @ faf9b54. main @ ba29336 (fin Phase 3).
Commits de la session: c1d56cd (mcp+binaire) → 81c2437 (backend 6x) → 899aea6 (index composite 2.5x)
→ 78b4972 (plans 6+7 + 20 rulings) → faf9b54 (WIP relevance, VÉRIFIÉ contre le vrai binaire).

BLOCAGE UNIQUE: explore ne répond pas. Le WIP faf9b54 a CORRIGÉ le bug de dédup et amélioré les
seeds (Q2/Q3 atteignent batch.rs/resolve_one), MAIS **la section Flow ne s'affiche toujours jamais,
sur aucune des 3 sondes** — même Q2 dont le 1er root est le bon symbole.
PROCHAINE EXPÉRIENCE (10 min, elle tranche): forcer à la main les roots
[resolve_and_persist_batched, resolve_one, create_edges, insert_edges] — qui SONT une vraie chaîne
d'appels — et voir si Flow s'affiche. Si NON ⇒ second bug dans build_flow_from_named_symbols, pas
seulement dans la sélection des seeds. Si OUI ⇒ le scoring doit préférer des seeds CONNECTÉS
(aujourd'hui Q1 renvoie des TYPES — UnresolvedReference, GraphStore — pas les FONCTIONS du flux;
on n'appelle pas un type, donc aucune chaîne ne peut exister entre eux).

=== 2026-07-14 — LE BLOCAGE EST LEVÉ (c0c7143) ===

`explore` répond à la question exacte du gate Task 20 : **3/3 + Flow juste**, mesuré contre le vrai
binaire (`./scripts/ask.sh`), pas contre la suite.

    1. resolve_and_persist_batched (batch.rs:113) -> 2. insert_edges (store_impl.rs:105) -> 3. Edge

**L'instrument était la DIRECTION, pas le poids.** La session précédente avait raison de conclure
qu'aucune repondération ne pouvait marcher (12 vs 143, plafond additif de 30). Ce qu'elle a manqué :
sa propre contre-preuve — « la couche utilitaire touche tous les concepts » — est un fait
DIRECTIONNEL. Toute la plomberie qui l'avait fait échouer (`collect` in=527, `get_node_text` in=205,
`as_str` in=177) a **out=0**. Un utilitaire est appelé par tout ; un orchestrateur appelle tout. La
passe 12 score `deg_out + deg_in` et ne peut donc pas les distinguer. En ne comptant que les appels
SORTANTS, `resolve_and_persist_batched` passe du rang ~1400 (lexical) au **#2 sur 1 460**. La
plomberie n'est pas dévaluée : elle devient **structurellement inéligible**. Aucun amortissement.
⇒ passe 14 : réservation d'orchestrateur (2 slots de root sur 8, budget de roots inchangé).

**Le Flow : « la plus longue chaîne gagne » était faux depuis toujours** — un petit `named` le
masquait. Sur un vrai sous-graphe, « finir sur un symbole nommé » est vide de sens et la règle
dégénère en « la plus profonde » : la question sur *edge* descendait 8 sauts dans le résolveur et
finissait sur `WorkspacePackages`. Le critère est l'**ARRIVÉE** : partir d'un pôle, finir sur
l'AUTRE ; à égalité, la plus serrée.

**3 approches mesurées et MORTES (ne pas retenter) :**
1. ancrage sur les types + plus court chemin entre leurs handlers → `references` est une lance à
   incendie (2 666) : 155 handlers vs 96, et le plus court chemin trouve la paire de déchets la plus
   proche (`visit_node -> create_node`). Mauvais objectif.
2. élargir la liste des sinks interdits au-delà de celle de TS → supprime le Flow d'un projet à 2
   fichiers dont la seule épine passe par un nœud d'import.
3. (session précédente) les deux correctifs *dans* le multiplicateur — consignés dans `term_groups()`.

**DEUX INSTRUMENTS MENTAIENT, et c'est la leçon récurrente du projet :**
- `scripts/ask.sh` faisait un substring sur TOUT le texte : un fichier *nommé* dans le blast-radius
  comptait comme *livré*. Il annonçait 2/3 quand la vérité était **1/3**. Corrigé : il teste les
  sections rendues. Un fichier nommé mais non affiché est le PIRE cas — il envoie l'agent lire.
- `cargo test --workspace | head -25` a **caché un test en échec** et m'a fait annoncer « tout vert ».
  Compter, ne pas regarder défiler : `grep -c 'test result: FAILED'`.

Non sur-ajusté : vérifié contre un binaire construit depuis HEAD sur 4 requêtes jamais réglées —
4 améliorées, 2 inchangées, 0 régressée. Suite complète : **1 089 tests, 0 échec** (dont un test
selene-mcp qui **échouait déjà sur HEAD** : sa fixture n'avait que 2 nœuds, et « une chaîne à 2
nœuds est juste une arête »).

OUVERT (pré-existant, reproduit, non corrigé) : (1) les tests dans `src/` pilotent les flows —
`is_test_file` teste le CHEMIN, or Rust met ses tests dans le fichier source ; (2) `type_of` et
`returns` sont à ZÉRO dans un index Rust réel (émis comme `references` ?) — seam inerte potentiel.

--- 2026-07-14, plus tard : deux affirmations que la doc laissait passer, corrigées ---

Aucun code changé. Deux questions simples ont révélé que la doc mentait par omission :

1. **« On est meilleurs en perf ? »** — **ON N'EN SAIT RIEN.** RESUME.md §3 s'intitulait « Perf —
   RÉGLÉ » et annonçait **6×** et **2,5×**. Ces chiffres sont **Rust contre son propre passé** : on a
   corrigé DEUX BUGS À NOUS (le mauvais backend DB ; un index composite manquant). Ils ne disent
   **rien** sur CodeGraph TS. **Il n'existe AUCUN benchmark de vitesse Rust vs TS** — les trois docs
   de `docs/benchmarks/` mesurent la *justesse* (identité des arêtes, tolérance 0) et le gate DB
   Phase 1 oppose SurrealKV à RocksDB, deux backends À NOUS. Un lecteur pressé (moi, dans deux
   semaines) aurait cité « 6× » comme si on battait CodeGraph.
   ⇒ §3 réécrit. Le benchmark tête-à-tête devient une tâche explicite (§5.A ter) : `../codegraph` est
   sur le disque, c'est ~30 min. **Et il faut comparer les nœuds/arêtes produits, pas seulement le
   temps** — sinon « plus rapide » peut vouloir dire « il en fait moins », l'erreur que ce projet
   répète.
   ⚠ Contre-indice : persist ≈ 54 % du temps, `resolve_all` sur UN cœur, VS Code extrapolé à ~54 min.
   Ce n'est pas un profil de gagnant évident. Ne suppose pas la victoire.

2. **« C'est prêt ? C'est CodeGraph en Rust ? »** — **Non. Ça marche, ce n'est pas le produit.**
   RESUME.md ne répondait à ça **nulle part**, alors que c'est la première question qu'on pose.
   `selene-cli`, `selene-sync`, `selene-installer` = **3 lignes chacun**. Conséquences concrètes :
   **2 commandes** (`index`, `serve`), **réindexation À LA MAIN**, config MCP écrite à la main.
   (Piège : `selene-resolve` (17 k lignes) et `selene-graph` sont IMPLÉMENTÉS — le mot « stub » traîne
   dans leur doc de module et trompe un `grep`.)
   ⇒ nouveau §1 bis dans RESUME.md, et le bloc « Status » de CLAUDE.md réécrit (il annonçait encore
   « the remaining layer crates are stubs », faux depuis que graph/context/mcp tournent).

**La leçon est la même que celle du §9, sous une autre forme :** l'instrument qui ment n'est pas
toujours un test — ici c'était **le titre d'une section**. « Perf — RÉGLÉ » était vrai (nos bugs sont
réglés) et trompeur (ça ne dit rien de la comparaison qui compte). Écris ce que la mesure prouve,
pas ce qu'elle suggère.
