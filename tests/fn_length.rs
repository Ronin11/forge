//! Keep functions short; pre-existing long functions have a shrinking exception list.
use std::path::Path;
use std::process::Command;

const LIMIT: usize = 120;

// Ceilings are the function's line count when the rule was added plus 20.
// Remove entries as functions shrink; never add new exceptions. Test-module
// functions count too: a reason may say "test fixture", but the ceiling holds.
const ALLOWLIST: &[(&str, &str, usize, &str)] = &[
    (
        "repomap/src/main.rs",
        "real_end",
        162,
        "existing function awaiting a focused split",
    ),
    (
        "src/agent.rs",
        "run_chat",
        161,
        "existing function awaiting a focused split",
    ),
    (
        "src/agent.rs",
        "run_codex",
        202,
        "existing function awaiting a focused split",
    ),
    (
        "src/agent.rs",
        "run_once",
        217,
        "existing function awaiting a focused split",
    ),
    (
        "src/attempt.rs",
        "run_attempt",
        280,
        "existing function awaiting a focused split",
    ),
    (
        "src/audit.rs",
        "diagnose",
        254,
        "existing function awaiting a focused split",
    ),
    (
        "src/cli/statistics.rs",
        "quality_stats",
        165,
        "existing function awaiting a focused split",
    ),
    (
        "src/cli/statistics.rs",
        "stats",
        198,
        "existing function awaiting a focused split",
    ),
    (
        "src/cli/task_records.rs",
        "show",
        347,
        "existing function awaiting a focused split",
    ),
    (
        "src/cli/task_records.rs",
        "trace",
        191,
        "existing function awaiting a focused split",
    ),
    (
        "src/cli/workflows.rs",
        "list_workflows",
        228,
        "existing function awaiting a focused split",
    ),
    (
        "src/concierge.rs",
        "ask",
        142,
        "existing function awaiting a focused split",
    ),
    (
        "src/deploy.rs",
        "run",
        258,
        "existing function awaiting a focused split",
    ),
    (
        "src/egress.rs",
        "handle",
        153,
        "existing function awaiting a focused split",
    ),
    (
        "src/engine.rs",
        "prepare_worktree",
        149,
        "existing function awaiting a focused split",
    ),
    (
        "src/engine.rs",
        "run_directive_step",
        538,
        "existing function awaiting a focused split",
    ),
    (
        "src/engine.rs",
        "run_task",
        188,
        "existing function awaiting a focused split",
    ),
    (
        "src/graph.rs",
        "overlay_splits_repair_cost_and_matches_demotions_by_evidence",
        183,
        "test fixture",
    ),
    (
        "src/job.rs",
        "bench",
        143,
        "existing function awaiting a focused split",
    ),
    (
        "src/job.rs",
        "run_directive",
        155,
        "existing function awaiting a focused split",
    ),
    (
        "src/job.rs",
        "run_now",
        460,
        "existing function awaiting a focused split",
    ),
    (
        "src/landing.rs",
        "integrate",
        458,
        "existing function awaiting a focused split",
    ),
    (
        "src/landing.rs",
        "integrate_many",
        171,
        "existing function awaiting a focused split",
    ),
    (
        "src/operation.rs",
        "run_operation",
        305,
        "existing function awaiting a focused split",
    ),
    (
        "src/queue.rs",
        "edit_task",
        145,
        "existing function awaiting a focused split",
    ),
    (
        "src/queue.rs",
        "enqueue",
        259,
        "existing function awaiting a focused split",
    ),
    (
        "src/report.rs",
        "render",
        160,
        "existing function awaiting a focused split",
    ),
    (
        "src/report.rs",
        "summary",
        146,
        "existing function awaiting a focused split",
    ),
    (
        "src/report.rs",
        "to_json_matches_the_hand_written_shape_for_every_variant",
        255,
        "test fixture",
    ),
    (
        "src/sandbox.rs",
        "command",
        184,
        "existing function awaiting a focused split",
    ),
    (
        "src/sandbox.rs",
        "command_binds_tmpfs_home_before_ro_dirs_before_the_worktree",
        204,
        "existing function awaiting a focused split",
    ),
    (
        "src/store/factors.rs",
        "factor_stats",
        401,
        "existing function awaiting a focused split",
    ),
    (
        "src/store/questions.rs",
        "question_records",
        150,
        "existing function awaiting a focused split",
    ),
    (
        "src/store/stats_tests.rs",
        "factor_stats_recovers_a_planted_provider_effect_and_widens_a_thin_levels_interval",
        148,
        "test fixture",
    ),
    (
        "src/store/stats_tests.rs",
        "file_changes_groups_by_path_and_task_and_sums_only_the_touching_attempts",
        156,
        "test fixture",
    ),
    (
        "src/store/stats_tests.rs",
        "role_stats_splits_by_provider_and_model_and_averages_within_each",
        148,
        "test fixture",
    ),
    (
        "src/supervisor.rs",
        "prompt",
        169,
        "existing function awaiting a focused split",
    ),
    (
        "src/supervisor.rs",
        "supervise",
        474,
        "existing function awaiting a focused split",
    ),
    (
        "src/verify.rs",
        "common_l0",
        160,
        "existing function awaiting a focused split",
    ),
    (
        "src/verify.rs",
        "verify_directive",
        176,
        "existing function awaiting a focused split",
    ),
    (
        "src/view/portal_tests.rs",
        "portal_doc_never_carries_a_forbidden_key_even_when_the_project_has_everything",
        160,
        "test fixture",
    ),
    (
        "src/view/portal_tests.rs",
        "running_for_you_carries_a_description_effects_a_rehearsal_flag_and_gates_the_needs_human_reason",
        155,
        "test fixture",
    ),
    (
        "src/view/projects.rs",
        "portal_doc",
        295,
        "existing function awaiting a focused split",
    ),
    (
        "src/view/stats_tests.rs",
        "repair_cost_attributes_a_later_landings_cost_by_the_share_of_lines_it_rewrote",
        148,
        "test fixture",
    ),
    (
        "src/view/tasks.rs",
        "trace_doc",
        185,
        "existing function awaiting a focused split",
    ),
    (
        "src/worker.rs",
        "work",
        223,
        "existing function awaiting a focused split",
    ),
    (
        "src/workflows.rs",
        "parse_action",
        175,
        "existing function awaiting a focused split",
    ),
    (
        "tests/e2e/concierge.rs",
        "a_pattern_files_a_proposal_and_a_yes_creates_the_initiative",
        226,
        "test fixture",
    ),
    ("tests/e2e/concierge.rs", "setup", 146, "test fixture"),
    (
        "tests/e2e/deploy.rs",
        "a_deploy_that_passes_records_ok_and_a_failing_one_rolls_back_and_blocks_a_question",
        172,
        "test fixture",
    ),
    (
        "tests/e2e/deploy.rs",
        "deploy_remove_deletes_the_target_and_its_future_on_landing_runs",
        150,
        "test fixture",
    ),
    (
        "tests/e2e/deploy.rs",
        "deploy_targets_are_added_listed_and_forge_deploy_log_starts_empty",
        206,
        "test fixture",
    ),
    (
        "tests/e2e/fixtures.rs",
        "capture_job_deploy_and_portal_fixtures",
        211,
        "test fixture",
    ),
    (
        "tests/e2e/jobs.rs",
        "a_skip_if_that_exits_0_skips_the_job_and_one_that_exits_1_lets_the_steps_run",
        195,
        "test fixture",
    ),
    (
        "tests/e2e/jobs.rs",
        "doctor_daily_dry_run_parses_resolves_and_records_effects",
        149,
        "test fixture",
    ),
    (
        "tests/e2e/jobs.rs",
        "drift_weekly_model_drift_fires_when_an_alias_moved",
        170,
        "test fixture",
    ),
    (
        "tests/e2e/jobs.rs",
        "engineering_weekly_dry_run_measures_and_files_nothing",
        146,
        "test fixture",
    ),
    (
        "tests/e2e/jobs.rs",
        "forge_job_start_delay_leaves_the_job_scheduled_until_due_and_zero_runs_it",
        149,
        "test fixture",
    ),
    (
        "tests/e2e/jobs.rs",
        "forge_job_start_now_runs_operations_inline_and_dry_run_writes_nothing",
        159,
        "test fixture",
    ),
    (
        "tests/e2e/jobs.rs",
        "forge_job_start_resolves_a_run_workflow_from_the_projects_repository_and_records_the_source",
        146,
        "test fixture",
    ),
    (
        "tests/e2e/jobs.rs",
        "killed_job_recovery",
        186,
        "test fixture",
    ),
    (
        "tests/e2e/jobs.rs",
        "retry_1_runs_the_job_twice_then_stops",
        148,
        "test fixture",
    ),
    (
        "tests/e2e/jobs.rs",
        "send_sms_error_30034_surfaces_its_message",
        146,
        "test fixture",
    ),
    (
        "tests/e2e/landing.rs",
        "a_task_queued_after_another_waits_for_its_landing_and_blocks_on_its_failure",
        218,
        "test fixture",
    ),
    (
        "tests/e2e/landing.rs",
        "landing_reverifies_against_the_moved_base_and_folds_the_hidden_tests",
        186,
        "test fixture",
    ),
    (
        "tests/e2e/listing.rs",
        "a_portal_token_answers_only_its_own_projects_blocked_question",
        145,
        "test fixture",
    ),
    (
        "tests/e2e/listing.rs",
        "add_json_names_the_task_and_show_json_round_trips_the_spec_through_add",
        142,
        "test fixture",
    ),
    (
        "tests/e2e/listing.rs",
        "decisions_and_requests_grep_their_own_fields",
        150,
        "test fixture",
    ),
    (
        "tests/e2e/listing.rs",
        "forge_task_set_replaces_text_workflow_after_and_checks_and_records_old_and_new",
        181,
        "test fixture",
    ),
    (
        "tests/e2e/messages.rs",
        "record_then_list_round_trips_and_since_filters",
        237,
        "test fixture",
    ),
    (
        "tests/e2e/ops.rs",
        "a_mutating_operation_is_committed_and_verified_by_the_kernel",
        145,
        "test fixture",
    ),
    (
        "tests/e2e/ops.rs",
        "operations_run_in_order_and_appear_as_rows",
        147,
        "test fixture",
    ),
    (
        "tests/e2e/plugins.rs",
        "github_issues_files_a_task_and_reports_back_when_it_lands",
        177,
        "test fixture",
    ),
    (
        "tests/e2e/plugins.rs",
        "the_signal_plugin_delivers_an_addressed_question_to_its_contact_and_records_her_answer",
        205,
        "test fixture",
    ),
    (
        "tests/e2e/plugins.rs",
        "the_signal_plugin_notifies_a_blocked_task_and_files_an_answer_from_a_reply",
        169,
        "test fixture",
    ),
    (
        "tests/e2e/plugins.rs",
        "the_signal_plugin_routes_a_contacts_message_through_the_concierge",
        219,
        "test fixture",
    ),
    (
        "tests/e2e/plugins.rs",
        "the_signal_plugin_sends_a_contact_their_portal_link_on_intake_accept_and_on_request",
        220,
        "test fixture",
    ),
    (
        "tests/e2e/providers.rs",
        "a_task_on_a_codex_provider_runs_end_to_end_and_records_runner_and_provider",
        151,
        "test fixture",
    ),
    (
        "tests/e2e/provision.rs",
        "provision_creates_a_firewall_and_server_and_records_the_host_suggestion",
        150,
        "test fixture",
    ),
    (
        "tui/src/lib.rs",
        "draw",
        144,
        "existing function awaiting a focused split",
    ),
    (
        "tui/src/lib.rs",
        "draw_task",
        227,
        "existing function awaiting a focused split",
    ),
    (
        "tui/src/lib.rs",
        "handle_key",
        171,
        "existing function awaiting a focused split",
    ),
    (
        "tui/src/stats.rs",
        "tables",
        272,
        "existing function awaiting a focused split",
    ),
    (
        "web/src/main.rs",
        "handle",
        377,
        "existing function awaiting a focused split",
    ),
];

/// Replace comments, string, raw string and char literal contents with spaces,
/// keeping newlines so line numbers survive.
fn blank(src: &str) -> Vec<u8> {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;
    let wipe = |out: &mut Vec<u8>, from: usize, to: usize| {
        for c in &mut out[from..to] {
            if *c != b'\n' {
                *c = b' ';
            }
        }
    };
    while i < b.len() {
        let c = b[i];
        if c == b'/' && b.get(i + 1) == Some(&b'/') {
            let end = b[i..]
                .iter()
                .position(|&x| x == b'\n')
                .map_or(b.len(), |p| i + p);
            wipe(&mut out, i, end);
            i = end;
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            let (mut depth, mut j) = (1, i + 2);
            while j < b.len() && depth > 0 {
                if b[j..].starts_with(b"/*") {
                    depth += 1;
                    j += 2;
                } else if b[j..].starts_with(b"*/") {
                    depth -= 1;
                    j += 2;
                } else {
                    j += 1;
                }
            }
            wipe(&mut out, i, j);
            i = j;
        } else if (c == b'r' || (c == b'b' && b.get(i + 1) == Some(&b'r')))
            && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_'))
            && raw_start(b, i).is_some()
        {
            let (hashes, open) = raw_start(b, i).unwrap();
            let mut close = vec![b'"'];
            close.extend(std::iter::repeat_n(b'#', hashes));
            let end = b[open..]
                .windows(close.len())
                .position(|w| w == close.as_slice())
                .map_or(b.len(), |p| open + p + close.len());
            wipe(&mut out, i, end);
            i = end;
        } else if c == b'"' {
            let mut j = i + 1;
            while j < b.len() && b[j] != b'"' {
                j += if b[j] == b'\\' { 2 } else { 1 };
            }
            let end = (j + 1).min(b.len());
            wipe(&mut out, i, end);
            i = end;
        } else if c == b'\'' {
            // A char literal, unless it is a lifetime.
            let end = if b.get(i + 1) == Some(&b'\\') {
                b[i + 2..]
                    .iter()
                    .position(|&x| x == b'\'')
                    .map(|p| i + 2 + p + 1)
            } else {
                let w = src[i + 1..].chars().next().map_or(0, char::len_utf8);
                (w > 0 && b.get(i + 1 + w) == Some(&b'\'')).then_some(i + 2 + w)
            };
            match end {
                Some(end) => {
                    wipe(&mut out, i, end);
                    i = end;
                }
                None => i += 1,
            }
        } else {
            i += 1;
        }
    }
    out
}

/// For `r"`, `r#"`, `br"`: the hash count and the index just past the opening quote.
fn raw_start(b: &[u8], i: usize) -> Option<(usize, usize)> {
    let mut j = i + if b[i] == b'b' { 2 } else { 1 };
    let hashes = b[j..].iter().take_while(|&&x| x == b'#').count();
    j += hashes;
    (b.get(j) == Some(&b'"')).then_some((hashes, j + 1))
}

/// Every `fn` with a body: (name, first line, line count), lines 1-based.
fn functions(src: &str) -> Vec<(String, usize, usize)> {
    let b = blank(src);
    let line_of = |at: usize| b[..at].iter().filter(|&&c| c == b'\n').count() + 1;
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut found = Vec::new();
    let mut i = 0;
    while i + 3 < b.len() {
        let token = b[i] == b'f'
            && b[i + 1] == b'n'
            && b[i + 2].is_ascii_whitespace()
            && (i == 0 || !ident(b[i - 1]));
        if !token {
            i += 1;
            continue;
        }
        let mut j = i + 2;
        while b[j].is_ascii_whitespace() {
            j += 1;
        }
        let start = j;
        while j < b.len() && ident(b[j]) {
            j += 1;
        }
        let name = String::from_utf8_lossy(&b[start..j]).into_owned();
        // The signature ends at `{` (body) or `;` (no body) outside parens/brackets.
        let mut nest = 0i32;
        while j < b.len() {
            match b[j] {
                b'(' | b'[' => nest += 1,
                b')' | b']' => nest -= 1,
                b'{' | b';' if nest <= 0 => break,
                _ => {}
            }
            j += 1;
        }
        if name.is_empty() || j >= b.len() || b[j] == b';' {
            i = j.max(i + 2);
            continue;
        }
        let mut depth = 0;
        let mut k = j;
        while k < b.len() {
            match b[k] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            k += 1;
        }
        let first = line_of(i);
        found.push((name, first, line_of(k.min(b.len() - 1)) - first + 1));
        i += 2;
    }
    found
}

#[test]
fn tracked_rust_functions_stay_within_their_line_limits() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new("git")
        .args(["ls-files", "-z", "--", "*.rs"])
        .current_dir(root)
        .output()
        .expect("list tracked Rust files");
    assert!(
        output.status.success(),
        "git ls-files failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let paths = std::str::from_utf8(&output.stdout).expect("UTF-8 tracked paths");
    let mut failures = Vec::new();
    let mut over = std::collections::BTreeSet::new();
    for path in paths.split('\0').filter(|p| !p.is_empty()) {
        let full = root.join(path);
        if !full.exists() {
            failures.push(format!("{path}: tracked file is missing"));
            continue;
        }
        let src = std::fs::read_to_string(full).expect("read tracked Rust source");
        for (name, line, len) in functions(&src) {
            if len <= LIMIT {
                continue;
            }
            over.insert((path.to_string(), name.clone()));
            let ceiling = ALLOWLIST
                .iter()
                .find(|(p, f, _, _)| *p == path && *f == name)
                .map_or(LIMIT, |e| e.2);
            if len > ceiling {
                failures.push(format!(
                    "{path}:{line}: fn {name} is {len} lines, exceeds ceiling {ceiling}"
                ));
            }
        }
    }
    for &(path, name, _, reason) in ALLOWLIST {
        assert!(
            !reason.is_empty(),
            "{path}: {name}: exception needs a reason"
        );
        if !over.contains(&(path.to_string(), name.to_string())) {
            failures.push(format!(
                "{path}: fn {name}: stale allowlist entry; delete it (gone or at most {LIMIT} lines)"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{}\nSplit the function into named steps instead of raising a ceiling.",
        failures.join("\n")
    );
}

#[cfg(test)]
mod scanner {
    use super::functions;

    fn lens(src: &str) -> Vec<(String, usize)> {
        functions(src).into_iter().map(|(n, _, l)| (n, l)).collect()
    }

    #[test]
    fn braces_in_strings_and_raw_strings_do_not_count() {
        let src = "fn a() {\n let s = \"}}\";\n let r = r#\"\n}\n\"}\"#;\n}\nfn b() {}\n";
        assert_eq!(lens(src), [("a".into(), 6), ("b".into(), 1)]);
    }

    #[test]
    fn char_literals_and_lifetimes() {
        let src = "fn a<'x>(s: &'x str) -> char {\n let c = '{';\n let d = '\\'';\n '}'\n}\n";
        assert_eq!(lens(src), [("a".into(), 5)]);
    }

    #[test]
    fn comments_are_ignored() {
        let src = "fn a() {\n // }\n /* { /* } */ } */\n}\n";
        assert_eq!(lens(src), [("a".into(), 4)]);
    }

    #[test]
    fn nested_closures_match_arms_and_inner_fns() {
        let src = "fn a(x: u8) {\n let f = |y| { match y { 1 => { 2 } _ => 3 } };\n fn inner() {\n }\n match x { _ => {} }\n}\n";
        assert_eq!(lens(src), [("a".into(), 6), ("inner".into(), 2)]);
    }

    #[test]
    fn bodyless_fns_and_fn_pointer_types_are_skipped() {
        let src = "trait T { fn a(&self); }\nfn b(f: fn(u8) -> u8) -> u8 {\n f(1)\n}\n";
        assert_eq!(lens(src), [("b".into(), 3)]);
    }
}
