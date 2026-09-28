//! A method's required args (docs/REVIEW-4.md, E3-19): `forge project
//! deploy add`/`set` refuse a target that leaves one missing or blank, and
//! deploy-command and deploy-user-service refuse an empty host or an empty
//! or `/` dest before rsync ever runs, so `rsync -a --delete` never
//! targets the remote's `/`.

use super::*;

fn add(e: &Env, name: &str, method: &str, args: &[&str]) -> std::process::Output {
    let repo_s = e.repo.to_str().unwrap();
    let mut argv = vec![
        "project", "deploy", "add", "demo", name, "--repo", repo_s, "--method", method,
    ];
    for a in args {
        argv.extend(["--arg", a]);
    }
    argv.extend(["--check", "true"]);
    e.forge("ok.sh", &argv)
}

fn demo(e: &Env) {
    let repo_s = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "demo", "--purpose", "p", "--repo", repo_s],
        )
        .status
        .success()
    );
}

fn targets(e: &Env) -> Vec<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["project", "deploy", "list", "demo", "--json"])
            .stdout,
    )
    .unwrap();
    v.as_array().unwrap().clone()
}

#[test]
fn add_and_set_refuse_a_target_missing_or_blanking_a_required_arg() {
    let e = Env::new();
    demo(&e);

    let cases: &[(&str, &[&str], &str)] = &[
        ("deploy-command", &["host=box"], "dest"),
        ("deploy-command", &["dest=/srv/app"], "host"),
        ("deploy-command", &["host=box", "dest="], "dest"),
        ("deploy-command", &["host=  ", "dest=/srv/app"], "host"),
        (
            "deploy-user-service",
            &["host=box", "unit=a.service"],
            "dest",
        ),
        (
            "deploy-user-service",
            &["dest=/srv/app", "unit=a.service"],
            "host",
        ),
        (
            "deploy-user-service",
            &["host=box", "dest=/srv/app"],
            "unit",
        ),
    ];
    for (method, args, missing) in cases {
        let o = add(&e, "prod", method, args);
        assert!(!o.status.success(), "{method} {args:?} was accepted");
        let err = String::from_utf8_lossy(&o.stderr).to_string();
        assert!(err.contains(&format!("--arg {missing}=")), "{err}");
    }
    assert!(targets(&e).is_empty());

    let o = add(&e, "prod", "deploy-command", &["host=box", "dest=/srv/app"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    // `set` blanking dest, or switching to a method whose required arg the
    // target lacks, is refused and leaves the stored target as it was.
    let bad = e.forge(
        "ok.sh",
        &["project", "deploy", "set", "demo", "prod", "--arg", "dest="],
    );
    assert!(!bad.status.success());
    let bad = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "set",
            "demo",
            "prod",
            "--method",
            "deploy-user-service",
        ],
    );
    assert!(!bad.status.success());
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("--arg unit="),
        "{}",
        String::from_utf8_lossy(&bad.stderr)
    );
    let rows = targets(&e);
    assert_eq!(rows[0]["method"], "deploy-command", "{rows:?}");
    assert_eq!(rows[0]["args"]["dest"], "/srv/app", "{rows:?}");
}

#[test]
fn deploy_command_and_deploy_user_service_refuse_a_root_dest_before_rsync() {
    let e = Env::new();
    demo(&e);
    for (name, method, extra) in [
        ("cmd", "deploy-command", "command=true"),
        ("svc", "deploy-user-service", "unit=demo.service"),
    ] {
        for (i, dest) in ["dest=/", "dest=//"].into_iter().enumerate() {
            let target = format!("{name}{i}");
            let o = add(&e, &target, method, &["host=remotebox", dest, extra]);
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        }
    }

    let fakebin = e._dir.path().join("fakebin");
    std::fs::create_dir_all(&fakebin).unwrap();
    write_fake(
        &fakebin.join("rsync"),
        "#!/bin/bash\necho \"rsync $*\" >> \"$HOME/deploy-calls.log\"\n",
    );
    write_fake(&fakebin.join("ssh"), FAKE_SSH);
    write_fake(&fakebin.join("systemctl"), FAKE_SYSTEMCTL);
    let path = format!(
        "{}:{}",
        fakebin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let fakehome = e._dir.path().join("fakehome");
    std::fs::create_dir_all(&fakehome).unwrap();
    let sha = git(&e.repo, &["rev-parse", "HEAD"]);

    for target in ["cmd0", "cmd1", "svc0", "svc1"] {
        let o = e
            .cmd("ok.sh")
            .env("PATH", &path)
            .env("HOME", &fakehome)
            .args(["deploy", "demo", target, "--sha", &sha])
            .output()
            .unwrap();
        assert!(!o.status.success(), "{target} deployed onto /");
    }
    let calls = std::fs::read_to_string(fakehome.join("deploy-calls.log")).unwrap_or_default();
    assert!(!calls.contains("rsync"), "{calls}");
}
