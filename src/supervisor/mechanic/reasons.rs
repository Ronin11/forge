//! Pure matches on a failed task's `reason` string: the three mechanical
//! kinds that need no re-verification, only the shape of what the engine
//! already wrote (see `src/engine/land.rs`, `src/engine/step.rs`).

/// A landing round rewound on a conflict with the base and ran out of
/// attempts or budget (`engine::land::try_land`'s `Integrate::Rewind` arm):
/// `"landing failed after N attempt(s): main moved to SHA; conflicts in
/// FILES; the verified branch is pushed for a human"`.
pub fn is_landing_conflict(reason: &str) -> bool {
    reason.starts_with("landing failed") && reason.contains("conflicts in")
}

/// The coder ran out of turns with commits on the branch and the checks
/// that followed still failed (`engine::step`'s capped-committed arm):
/// `"ran out of turns after committing; the checks fail: ..."`. The
/// sibling case where the checks passed ends `Unverified`, not `Failed`,
/// so it never reaches here.
pub fn is_turn_cap_with_commits(reason: &str) -> bool {
    reason.starts_with("ran out of turns after committing") && reason.contains("the checks fail:")
}

/// The only L0 row that failed was `clean-tree`
/// (`engine::outcome::l0_failure_reason`): `"L0 failed: clean-tree (after N
/// attempt(s))"`. A clean-tree failure alongside another L0 rule is not
/// this kind: which one made the tree dirty is not obvious enough to guess.
pub fn is_clean_tree_only(reason: &str) -> bool {
    reason
        .strip_prefix("L0 failed: ")
        .and_then(|rest| rest.split(" (after").next())
        .is_some_and(|names| names == "clean-tree")
}

/// Whether this reason is mechanic's to classify at all. An `operation ...`
/// failure (a workflow step that is a command, not an agent — `setup`,
/// `repo-map`, and the like) already has its own owner: an egress or cache
/// refusal is the `environment` module's decision to grant or not
/// (`src/environment.rs`), and every other operation failure is
/// deterministic (the same command run again fails the same way), so
/// nothing here would help. An `error:` reason is a kernel fault
/// (`worker::drive`'s `Fault::Task` arm), not the agent's work, and is
/// already reported as its own `Event::Note`. Both are complete no-ops:
/// mechanic records nothing and takes no action on either.
pub fn in_scope(reason: &str) -> bool {
    !reason.starts_with("operation ") && !reason.starts_with("error:")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn landing_conflict_matches_a_rewound_landing() {
        assert!(is_landing_conflict(
            "landing failed after 2 attempt(s): main moved to a6cb6bda; conflicts in src/successor.rs; the verified branch is pushed for a human"
        ));
        assert!(!is_landing_conflict(
            "landing failed: fast-forward of main rejected"
        ));
        assert!(!is_landing_conflict("L1 failed: test (after 2 attempt(s))"));
    }

    #[test]
    fn turn_cap_matches_only_the_failed_capped_committed_shape() {
        assert!(is_turn_cap_with_commits(
            "ran out of turns after committing; the checks fail: L1 failed: test (after 2 attempt(s))"
        ));
        assert!(!is_turn_cap_with_commits(
            "ran out of turns after committing; the checks pass but no result was returned, so the branch goes to a human"
        ));
    }

    #[test]
    fn clean_tree_matches_only_when_it_is_the_sole_l0_failure() {
        assert!(is_clean_tree_only(
            "L0 failed: clean-tree (after 1 attempt(s))"
        ));
        assert!(!is_clean_tree_only(
            "L0 failed: clean-tree, has-commits (after 1 attempt(s))"
        ));
        assert!(!is_clean_tree_only(
            "L0 failed: has-commits (after 1 attempt(s))"
        ));
    }

    #[test]
    fn operation_and_internal_error_failures_are_out_of_scope() {
        assert!(!in_scope("operation setup failed: exit 1"));
        assert!(!in_scope(
            "operation needs-extra (verifies) failed after 1 attempt(s): extra.txt is missing"
        ));
        assert!(!in_scope("error: some internal fault"));
        assert!(in_scope("L0 failed: clean-tree (after 1 attempt(s))"));
        assert!(in_scope("agent exit 1 (after 1 attempt(s))"));
    }
}
