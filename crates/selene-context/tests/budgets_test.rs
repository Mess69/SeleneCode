#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_arguments)]
//! Task 8 — the budget tiers, the boundaries, and **the monotonicity invariant**.
//!
//! A "shape" test here would be worthless: the tiers are a contract copied verbatim from the
//! map, so the test asserts all 5 tiers × 11 fields **value by value**. A wrong number is the
//! entire failure mode — the code path still runs, the output still renders, and the only
//! symptom is an agent that starts reaching for `Read` again.

use selene_context::{ExploreBudget, budget_for, explore_budget};

fn tier(
    max_output_chars: usize,
    default_max_files: usize,
    max_chars_per_file: usize,
    gap_threshold: usize,
    max_symbols_in_file_header: usize,
    max_edges_per_relationship_kind: usize,
    rich: bool,
    exclude_low_value_files: bool,
) -> ExploreBudget {
    ExploreBudget {
        max_output_chars,
        default_max_files,
        max_chars_per_file,
        gap_threshold,
        max_symbols_in_file_header,
        max_edges_per_relationship_kind,
        include_relationships: rich,
        include_additional_files: rich,
        include_completeness_signal: rich,
        include_budget_note: rich,
        exclude_low_value_files,
    }
}

/// All five tiers, every field — the contract, value by value.
#[test]
fn the_tier_table_is_verbatim() {
    let table: &[(u64, ExploreBudget)] = &[
        (149, tier(13_000, 4, 3_800, 7, 5, 4, false, true)),
        (499, tier(18_000, 5, 3_800, 8, 6, 6, false, true)),
        (4_999, tier(24_000, 8, 6_500, 12, 10, 10, true, false)),
        (14_999, tier(24_000, 8, 7_000, 15, 15, 15, true, false)),
        (15_000, tier(24_000, 8, 7_000, 15, 15, 15, true, false)),
    ];

    for (files, want) in table {
        assert_eq!(
            budget_for(*files),
            *want,
            "the tier at {files} files drifted from the map — these numbers were tuned \
             against real agent behavior, and a well-meant adjustment loses that tuning \
             silently, with every test still green"
        );
    }
}

/// The bounds are `<`, never `≤`. An off-by-one moves a whole class of repo into the wrong
/// tier.
#[test]
fn the_tier_boundaries_are_exclusive() {
    for (below, at) in [(149, 150), (499, 500), (4_999, 5_000)] {
        assert_ne!(
            budget_for(below),
            budget_for(at),
            "{below} and {at} must land in DIFFERENT tiers — the bound is `<`, not `≤`"
        );
    }

    // ⚠ 14 999 vs 15 000 is the exception, and it is deliberate: the map's `<15000` and
    // `≥15000` rows are IDENTICAL in all eleven fields. The tier boundary exists (the
    // *call* budget steps from 3 to 4 there), but the OUTPUT budget does not change —
    // because 24 000 is already at the host's externalization cliff and there is nowhere
    // left to grow. Asserting they differ would be asserting a bug into the contract.
    assert_eq!(
        budget_for(14_999),
        budget_for(15_000),
        "the map's last two tiers are identical — the output budget has hit the ceiling"
    );
    assert_ne!(
        explore_budget(14_999),
        explore_budget(15_000),
        "…but the CALL budget does step here: 3 → 4"
    );
    for (below, at) in [
        (499, 500),
        (4_999, 5_000),
        (14_999, 15_000),
        (24_999, 25_000),
    ] {
        assert_ne!(
            explore_budget(below),
            explore_budget(at),
            "explore_budget: {below} vs {at}"
        );
    }
}

/// **THE INVARIANT.** A larger repo must NEVER get a smaller per-file budget.
///
/// A violation is *invisible*: the tool still works, the tests still pass, and the only
/// symptom is that explore gets less useful on exactly the repos where the agent cannot fall
/// back on reading a handful of files by hand.
#[test]
fn monotonicity_sweep_over_thirty_thousand_file_counts() {
    let mut prev = budget_for(0);

    for files in 0..30_000u64 {
        let b = budget_for(files);
        assert!(
            b.max_chars_per_file >= prev.max_chars_per_file,
            "MONOTONICITY VIOLATED at {files} files: max_chars_per_file dropped {} → {}. A \
             bigger repo just got a SMALLER per-file budget.",
            prev.max_chars_per_file,
            b.max_chars_per_file
        );
        assert!(
            b.max_output_chars >= prev.max_output_chars,
            "MONOTONICITY VIOLATED at {files} files: max_output_chars dropped {} → {}",
            prev.max_output_chars,
            b.max_output_chars
        );
        prev = b;
    }
}

/// The call budget is monotonic too — an agent on a bigger repo is never told to make FEWER
/// calls.
#[test]
fn the_call_budget_is_monotonic() {
    let mut prev = explore_budget(0);
    for files in 0..30_000u64 {
        let b = explore_budget(files);
        assert!(b >= prev, "call budget dropped at {files}: {prev} → {b}");
        prev = b;
    }
}

/// Every tier stays under the host's ~25 000 externalization cliff — past which a "bigger"
/// budget delivers a FILE THE AGENT MUST OPEN, which is the Read we exist to prevent.
#[test]
fn no_tier_exceeds_the_externalization_cliff() {
    for files in [
        0u64, 149, 150, 499, 500, 4_999, 5_000, 14_999, 15_000, 100_000,
    ] {
        let b = budget_for(files);
        assert!(
            b.max_output_chars <= 24_000,
            "{files} files: max_output_chars {} would be externalized by the host into a file \
             the agent has to open — the anti-Read invariant, biting from the other side",
            b.max_output_chars
        );
    }
}
