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
