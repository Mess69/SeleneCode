//! The explore **budgets** — and the monotonicity invariant.
//!
//! # These numbers are a CONTRACT, not parameters to tune
//!
//! Every value in [`TIERS`] is copied verbatim from `maps/mcp-context.md` §Budgets. They were
//! tuned against real agent behavior on real repos; a well-meant adjustment is how that
//! tuning is lost — silently, with every test still green, and the only symptom is an agent
//! that starts reaching for `Read` again.
//!
//! # Why the output caps sit at ~24 000 — the anti-Read invariant, biting from the other side
//!
//! The host **externalizes any inline tool result over ~25 000 characters into a file the
//! agent must open**. So a budget above that ceiling does not deliver *more* context — it
//! delivers *a file to Read*. We would have spent the whole product's premise (answer without
//! opening a file) on a bigger number that forces the very Read it exists to prevent.
//!
//! 24 000 is under that cliff with room for the wrapper. It is not a coincidence and it is
//! not a starting point for negotiation.
//!
//! # Monotonicity: a bigger repo NEVER gets a smaller per-file budget
//!
//! [`budget_for`] is a step function over file count, and it must be **non-decreasing** in
//! `max_chars_per_file` and `max_output_chars`. The reason is that a monotonicity violation
//! is invisible: the tool still works, the tests still pass, and the only symptom is that
//! explore gets *less useful on exactly the repos where it matters most* — the big ones,
//! where the agent cannot fall back on reading a handful of files. `monotonicity_sweep` in
//! `tests/budgets_test.rs` walks 0..30 000 and asserts it, and that test **is** the
//! invariant.

/// The generic output cap (non-explore surfaces).
pub const MAX_OUTPUT_LENGTH: usize = 15_000;

/// The unique, greppable boundary a file section starts with. Truncation prefers to cut here.
pub const FILE_SECTION_PREFIX: &str = "**`";

/// The absolute ceiling, whatever the tier says. See the module docs: past ~25 000 the host
/// externalizes the result into a file, which is the Read we exist to prevent.
pub const HARD_CEILING: usize = 25_000;

/// The sentence appended when output is cut.
///
/// **Verbatim.** Read what it does: it truncates *without sending the agent to `Read`* — it
/// says the opposite, explicitly. Any rewording that suggests Read violates a Global
/// Constraint, and this is the one string most likely to be "improved" by someone who has not
/// read them.
pub const TRUNCATION_NOTE: &str = "\n\n... (output truncated to budget; the source above is complete and verbatim — treat it as already Read. For any area not covered, run another selene_explore with the specific names — do NOT Read these files.)";

/// The per-tier output budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExploreBudget {
    /// The tier's total output cap.
    pub max_output_chars: usize,
    /// How many files to render.
    pub default_max_files: usize,
    /// The per-file source budget. **Never decreases with repo size** (see the module docs).
    pub max_chars_per_file: usize,
    /// How many blank lines collapse a gap between rendered clusters.
    pub gap_threshold: usize,
    /// How many symbols to list in a file's header.
    pub max_symbols_in_file_header: usize,
    /// How many edges to list per relationship kind.
    pub max_edges_per_relationship_kind: usize,
    /// Render the relationships section at all.
    pub include_relationships: bool,
    /// Render the "other files touched" list.
    pub include_additional_files: bool,
    /// Render the completeness signal ("this is all of it" / "there is more").
    pub include_completeness_signal: bool,
    /// Render the budget note.
    pub include_budget_note: bool,
    /// Drop low-value files (generated, vendored) from the render.
    pub exclude_low_value_files: bool,
}

/// `(upper_bound_exclusive, budget)` — **verbatim from the map**. The bounds are `<`, never
/// `≤`; the boundary tests assert 149/150, 499/500, 4999/5000, 14999/15000.
const TIERS: &[(u64, ExploreBudget)] = &[
    (
        150,
        ExploreBudget {
            max_output_chars: 13_000,
            default_max_files: 4,
            max_chars_per_file: 3_800,
            gap_threshold: 7,
            max_symbols_in_file_header: 5,
            max_edges_per_relationship_kind: 4,
            include_relationships: false,
            include_additional_files: false,
            include_completeness_signal: false,
            include_budget_note: false,
            exclude_low_value_files: true,
        },
    ),
    (
        500,
        ExploreBudget {
            max_output_chars: 18_000,
            default_max_files: 5,
            max_chars_per_file: 3_800,
            gap_threshold: 8,
            max_symbols_in_file_header: 6,
            max_edges_per_relationship_kind: 6,
            include_relationships: false,
            include_additional_files: false,
            include_completeness_signal: false,
            include_budget_note: false,
            exclude_low_value_files: true,
        },
    ),
    (
        5_000,
        ExploreBudget {
            max_output_chars: 24_000,
            default_max_files: 8,
            max_chars_per_file: 6_500,
            gap_threshold: 12,
            max_symbols_in_file_header: 10,
            max_edges_per_relationship_kind: 10,
            include_relationships: true,
            include_additional_files: true,
            include_completeness_signal: true,
            include_budget_note: true,
            exclude_low_value_files: false,
        },
    ),
    (
        15_000,
        ExploreBudget {
            max_output_chars: 24_000,
            default_max_files: 8,
            max_chars_per_file: 7_000,
            gap_threshold: 15,
            max_symbols_in_file_header: 15,
            max_edges_per_relationship_kind: 15,
            include_relationships: true,
            include_additional_files: true,
            include_completeness_signal: true,
            include_budget_note: true,
            exclude_low_value_files: false,
        },
    ),
];

/// The `≥15 000` tier — identical to `<15 000`.
const LARGEST: ExploreBudget = ExploreBudget {
    max_output_chars: 24_000,
    default_max_files: 8,
    max_chars_per_file: 7_000,
    gap_threshold: 15,
    max_symbols_in_file_header: 15,
    max_edges_per_relationship_kind: 15,
    include_relationships: true,
    include_additional_files: true,
    include_completeness_signal: true,
    include_budget_note: true,
    exclude_low_value_files: false,
};

/// The output budget for a repo of `file_count` files.
pub fn budget_for(file_count: u64) -> ExploreBudget {
    for (upper, budget) in TIERS {
        if file_count < *upper {
            return *budget;
        }
    }
    LARGEST
}

/// **The CALL budget** — how many `explore` calls an agent should expect to need.
///
/// `<500 → 1`, `<5000 → 2`, `<15000 → 3`, `<25000 → 4`, else `5`. This is the "1 call on a
/// small repo, 3–5 on a large one" half of the sufficiency invariant, expressed as a number
/// the instructions can quote.
pub fn explore_budget(file_count: u64) -> u32 {
    match file_count {
        c if c < 500 => 1,
        c if c < 5_000 => 2,
        c if c < 15_000 => 3,
        c if c < 25_000 => 4,
        _ => 5,
    }
}

/// The final hard cap: `min(round(max_output_chars * 1.5), 25_000)`.
///
/// Cuts at the **last file-section boundary** (`\n**\``) when that boundary lies past **50%**
/// of the ceiling — a partial file section is worse than one fewer file — and hard-cuts
/// otherwise. Always appends [`TRUNCATION_NOTE`].
pub fn truncate_to_ceiling(text: &str, budget: &ExploreBudget) -> String {
    let ceiling = ((budget.max_output_chars as f64 * 1.5).round() as usize).min(HARD_CEILING);
    if text.len() <= ceiling {
        return text.to_string();
    }

    let cut = char_boundary_at_or_below(text, ceiling);
    let head = &text[..cut];

    let boundary = head.rfind(&format!("\n{FILE_SECTION_PREFIX}"));
    let end = match boundary {
        // Past the halfway mark: cutting at the file boundary loses less than a torn section.
        Some(at) if at > ceiling / 2 => at,
        _ => cut,
    };

    format!("{}{TRUNCATION_NOTE}", &text[..end])
}

/// The generic cap ([`MAX_OUTPUT_LENGTH`]) — cut at the last newline **if it lies past 80%**
/// of the cap, else a hard cut.
pub fn truncate_output(text: &str) -> String {
    if text.len() <= MAX_OUTPUT_LENGTH {
        return text.to_string();
    }
    let cut = char_boundary_at_or_below(text, MAX_OUTPUT_LENGTH);
    let head = &text[..cut];

    let end = match head.rfind('\n') {
        Some(at) if at > MAX_OUTPUT_LENGTH * 4 / 5 => at,
        _ => cut,
    };
    format!("{}{TRUNCATION_NOTE}", &text[..end])
}

fn char_boundary_at_or_below(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_call_budget_is_one_on_a_small_repo_and_five_on_a_huge_one() {
        assert_eq!(explore_budget(0), 1);
        assert_eq!(explore_budget(499), 1);
        assert_eq!(explore_budget(500), 2);
        assert_eq!(explore_budget(4_999), 2);
        assert_eq!(explore_budget(5_000), 3);
        assert_eq!(explore_budget(14_999), 3);
        assert_eq!(explore_budget(15_000), 4);
        assert_eq!(explore_budget(24_999), 4);
        assert_eq!(explore_budget(25_000), 5);
    }

    /// The note must never send the agent to Read — it says the opposite, on purpose.
    #[test]
    fn the_truncation_note_forbids_read_rather_than_suggesting_it() {
        assert!(TRUNCATION_NOTE.contains("do NOT Read these files"));
        assert!(TRUNCATION_NOTE.contains("treat it as already Read"));
    }

    #[test]
    fn the_ceiling_prefers_a_file_boundary_past_the_halfway_mark() {
        let budget = budget_for(1_000); // 18_000 → ceiling 25_000 (capped)
        let ceiling = ((budget.max_output_chars as f64 * 1.5).round() as usize).min(HARD_CEILING);

        // A file section starting well past 50% of the ceiling.
        let mut text = "x".repeat(ceiling * 3 / 4);
        text.push_str("\n**`src/late.rs`**\n");
        text.push_str(&"y".repeat(ceiling));

        let out = truncate_to_ceiling(&text, &budget);
        assert!(
            !out.contains("src/late.rs"),
            "cut AT the boundary — a torn file section is worse than one fewer file"
        );
        assert!(out.ends_with(TRUNCATION_NOTE));
    }

    #[test]
    fn the_ceiling_hard_cuts_when_the_only_boundary_is_too_early() {
        let budget = budget_for(1_000);
        let ceiling = ((budget.max_output_chars as f64 * 1.5).round() as usize).min(HARD_CEILING);

        // The only boundary sits at 10% — cutting there would throw away 90% of a good answer.
        let mut text = "a".repeat(ceiling / 10);
        text.push_str("\n**`src/early.rs`**\n");
        text.push_str(&"b".repeat(ceiling * 2));

        let out = truncate_to_ceiling(&text, &budget);
        assert!(
            out.contains("src/early.rs"),
            "an early boundary is NOT a cut point — we keep the content and hard-cut"
        );
        assert!(out.len() > ceiling / 2);
    }

    #[test]
    fn text_under_the_ceiling_is_untouched() {
        let budget = budget_for(100);
        let text = "short output";
        assert_eq!(truncate_to_ceiling(text, &budget), text);
        assert_eq!(truncate_output(text), text);
    }
}
