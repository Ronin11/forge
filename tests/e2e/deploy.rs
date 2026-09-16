use crate::support::*;

#[test]
fn deploy_targets_are_added_listed_and_forge_deploy_log_starts_empty() {
    let e = Env::new();
    let repo = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "demo", "--purpose", "p", "--repo", repo],
        )
        .status
        .success()
    );

    // No targets, no deploys, yet.
    let targets: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["project", "deploy", "list", "demo", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(targets.as_array().unwrap().len(), 0);

    let deploys: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["deploy", "log", "demo", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(deploys.as_array().unwrap().len(), 0);

    // Add two targets.
    let o = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "add",
            "demo",
            "prod",
            "--repo",
            repo,
            "--method",
            "deploy-user-service",
            "--arg",
            "unit=demo.service",
            "--check",
            "systemctl --user is-active demo.service",
            "--on-landing",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let o = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "add",
            "demo",
            "staging",
            "--repo",
            repo,
            "--scope",
            "web,api",
            "--method",
            "deploy-static",
            "--check",
            "curl -f https://staging.example.com/health",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    // A duplicate (project, name) is refused.
    let bad = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "add",
            "demo",
            "prod",
            "--repo",
            repo,
            "--method",
            "deploy-command",
            "--check",
            "true",
        ],
    );
    assert!(!bad.status.success());

    // A target for an unknown project is refused.
    let bad = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "add",
            "nope",
            "prod",
            "--repo",
            repo,
            "--method",
            "deploy-command",
            "--check",
            "true",
        ],
    );
    assert!(!bad.status.success());

    // `forge project deploy list --json` carries both, alphabetically.
    let rows: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["project", "deploy", "list", "demo", "--json"])
            .stdout,
    )
    .unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0]["name"], "prod");
    assert_eq!(rows[0]["method"], "deploy-user-service");
    assert_eq!(rows[0]["args"]["unit"], "demo.service");
    assert_eq!(
        rows[0]["check_cmd"],
        "systemctl --user is-active demo.service"
    );
    assert_eq!(rows[0]["on_landing"], true);
    assert_eq!(rows[0]["scope"], serde_json::Value::Null);
    assert_eq!(rows[1]["name"], "staging");
    assert_eq!(rows[1]["method"], "deploy-static");
    assert_eq!(rows[1]["on_landing"], false);
    assert_eq!(rows[1]["scope"], "[\"web\",\"api\"]");

    // The text form lists both too.
    let out = String::from_utf8_lossy(
        &e.forge("ok.sh", &["project", "deploy", "list", "demo"])
            .stdout,
    )
    .to_string();
    assert!(out.contains("prod"), "{out}");
    assert!(out.contains("staging"), "{out}");

    // No deploy has run yet: the log is still empty.
    let deploys: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["deploy", "log", "demo", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(deploys.as_array().unwrap().len(), 0);
    let deploys: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["deploy", "log", "demo", "prod", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(deploys.as_array().unwrap().len(), 0);

    // Running a deploy target prints that its method is not yet available.
    let o = e.forge("ok.sh", &["deploy", "demo", "prod"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(out.contains("not yet available"), "{out}");
    assert!(out.contains("deploy-user-service"), "{out}");

    // A nonexistent target is refused.
    let bad = e.forge("ok.sh", &["deploy", "demo", "nope"]);
    assert!(!bad.status.success());
}
