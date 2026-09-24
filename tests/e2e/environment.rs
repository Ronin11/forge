use crate::support::*;

/// A setup that fails once with the egress proxy's refusal of `host`, then
/// passes: what a build wanting a download behind the proxy looks like.
fn refusing_setup(e: &Env, host: &str, always: bool) {
    let once = if always {
        ""
    } else {
        "if [ -f .seen ]; then exit 0; fi; touch .seen; "
    };
    std::fs::write(
        e.repo.join("forge.toml"),
        format!(
            "[checks]\nanswer = [\"true\"]\nsetup = [\"bash\", \"-c\", \"{once}echo 'forge egress: {host}:443 is not allowed. This attempt may reach only: api.anthropic.com.'; exit 1\"]\n"
        ),
    )
    .unwrap();
    git(&e.repo, &["commit", "-qam", "setup wants the network"]);
}

#[test]
fn a_covered_host_is_granted_and_the_run_repeats_with_no_question_and_no_retry_spent() {
    let e = Env::new();
    refusing_setup(&e, "registry.npmjs.org", false);
    let o = e.run("ok.sh", &["--retries", "0"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let (state, reason, _) = e.task(1);
    assert_eq!(state, "succeeded", "{reason}");
    assert_eq!(
        e.attempts(1).len(),
        1,
        "the code attempt ran once, no retry"
    );

    let d = e.decisions_json();
    let rows = d.as_array().unwrap();
    assert_eq!(rows.len(), 1, "{d}");
    assert_eq!(rows[0]["answered_by"], "forge", "{d}");
    let text = rows[0].to_string();
    assert!(text.contains("registry.npmjs.org"), "{text}");
    assert!(text.contains("is not allowed"), "the evidence line: {text}");

    let doctor = e.forge("ok.sh", &["doctor"]);
    let out = String::from_utf8_lossy(&doctor.stdout);
    assert!(out.contains("1 automatic grant"), "{out}");
    assert!(out.contains("registry.npmjs.org"), "{out}");
}

#[test]
fn a_host_the_table_does_not_cover_still_fails_as_before() {
    let e = Env::new();
    refusing_setup(&e, "evil.example", true);
    assert!(!e.run("ok.sh", &["--retries", "0"]).status.success());
    let (state, reason, _) = e.task(1);
    assert_eq!(state, "failed");
    assert!(reason.starts_with("operation setup failed"), "{reason}");
    assert!(reason.contains("evil.example"), "{reason}");
    assert_eq!(e.decisions_json().as_array().unwrap().len(), 0);
}

fn supervised_run(e: &Env, supervisor: &str) -> std::process::Output {
    let mut c = e.with_role("ok.sh", "SUPERVISOR", supervisor);
    c.env("FORGE_SUPERVISOR", "1");
    c.args([
        "run",
        e.repo.to_str().unwrap(),
        "write 42 to answer.txt",
        "--retries",
        "0",
    ])
    .output()
    .unwrap()
}

#[test]
fn an_unlisted_registry_host_is_approved_by_the_supervisor_and_the_run_repeats() {
    let e = Env::new();
    refusing_setup(&e, "registry.example.net", false);
    let o = supervised_run(&e, "supervisor-env-approve.sh");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let (state, reason, _) = e.task(1);
    assert_eq!(state, "succeeded", "{reason}");

    let d = e.decisions_json();
    let rows = d.as_array().unwrap();
    assert_eq!(rows.len(), 1, "{d}");
    assert_eq!(rows[0]["answered_by"], "supervisor", "{d}");
    let text = rows[0].to_string();
    assert!(text.contains("registry.example.net"), "{text}");
    assert!(text.contains("is not allowed"), "the evidence line: {text}");
    assert!(e.requests_json().as_array().unwrap().is_empty());

    let doctor = e.forge("ok.sh", &["doctor"]);
    let out = String::from_utf8_lossy(&doctor.stdout);
    assert!(out.contains("approved by the supervisor"), "{out}");
}

#[test]
fn a_wildcard_is_denied_whatever_the_supervisor_says_and_the_question_names_why() {
    let e = Env::new();
    refusing_setup(&e, "*.example.net", true);
    supervised_run(&e, "supervisor-env-approve.sh");
    let (state, _, _) = e.task(1);
    assert_eq!(state, "blocked");
    let reqs = e.requests_json();
    let text = reqs.to_string();
    assert!(text.contains("*.example.net"), "{text}");
    assert!(text.contains("wildcard"), "{text}");
    assert!(text.contains("yes or no"), "{text}");
    assert_eq!(e.decisions_json().as_array().unwrap().len(), 0);
}

#[test]
fn a_denial_reaches_the_operator_with_the_supervisors_reason() {
    let e = Env::new();
    refusing_setup(&e, "registry.example.net", true);
    supervised_run(&e, "supervisor-env-deny.sh");
    assert_eq!(e.task(1).0, "blocked");
    let text = e.requests_json().to_string();
    assert!(text.contains("registry.example.net"), "{text}");
    assert!(
        text.contains("nothing shows the build needs this host"),
        "{text}"
    );
}

#[test]
fn the_repository_can_deny_a_host_the_supervisor_would_approve() {
    let e = Env::new();
    std::fs::write(
        e.repo.join("forge.toml"),
        "[checks]\nanswer = [\"true\"]\nsetup = [\"bash\", \"-c\", \"echo 'forge egress: registry.example.net:443 is not allowed.'; exit 1\"]\n[environment]\ndeny = [\"*.example.net\"]\n",
    )
    .unwrap();
    git(&e.repo, &["commit", "-qam", "deny example.net"]);
    supervised_run(&e, "supervisor-env-approve.sh");
    assert_eq!(e.task(1).0, "blocked");
    let text = e.requests_json().to_string();
    assert!(text.contains("deny"), "{text}");
}
