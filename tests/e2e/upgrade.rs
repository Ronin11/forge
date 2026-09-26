//! `forge upgrade`: see upgrade.rs and docs/OPS.md, "Upgrading". A fake
//! release tarball built from this suite's own `forge` binary (so its
//! `doctor --json` schema check runs for real) plus dummy placeholders for
//! the other four release binaries, `systemctl` and `curl` faked on
//! `PATH`, and an "old" release under `FORGE_HOME/bin/releases/old` that
//! `FORGE_HOME/bin/current` points at.

use crate::support::*;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const FAKE_SYSTEMCTL: &str = r#"#!/bin/bash
echo "systemctl $*" >> "$UPGRADE_CALLS_LOG"
if [ "$1" = "--user" ] && [ "$2" = "is-active" ]; then
  echo active
fi
exit 0
"#;

const FAKE_CURL_OK: &str = r#"#!/bin/bash
echo "curl $*" >> "$UPGRADE_CALLS_LOG"
printf '200'
"#;

const FAKE_CURL_BAD: &str = r#"#!/bin/bash
echo "curl $*" >> "$UPGRADE_CALLS_LOG"
printf '500'
"#;

fn write_fake(path: &Path, content: &str) {
    std::fs::write(path, content).unwrap();
    let mut perm = std::fs::metadata(path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    std::fs::set_permissions(path, perm).unwrap();
}

fn sh(cmd: &mut Command) {
    let o = cmd.output().unwrap();
    assert!(
        o.status.success(),
        "{:?} failed: {}",
        cmd,
        String::from_utf8_lossy(&o.stderr)
    );
}

/// A release tarball beside its `SHA256SUMS`, built with a real `forge`
/// binary (so the new binary's own `doctor --json` runs for real) and
/// dummy placeholders for the other four release binaries.
fn build_tarball(dir: &Path, version: &str) -> PathBuf {
    let name = format!("forge-{version}-x86_64-unknown-linux-gnu");
    let pkg = dir.join(&name);
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_forge"), pkg.join("forge")).unwrap();
    for b in [
        "forge-web",
        "forge-portal",
        "forge-repomap",
        "forge-test",
        "forge-tui",
    ] {
        write_fake(&pkg.join(b), "#!/bin/sh\n# new\nexit 0\n");
    }
    sh(Command::new("tar")
        .arg("-C")
        .arg(dir)
        .arg("-czf")
        .arg(format!("{name}.tar.gz"))
        .arg(&name)
        .current_dir(dir));
    let tarball = dir.join(format!("{name}.tar.gz"));
    let sums = Command::new("sha256sum")
        .arg(format!("{name}.tar.gz"))
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(sums.status.success());
    std::fs::write(dir.join("SHA256SUMS"), &sums.stdout).unwrap();
    tarball
}

/// `FORGE_HOME/bin` holding one release, `old`, of dummy binaries, live
/// through `current`.
fn old_bin_dir(home: &Path) -> PathBuf {
    let bins = home.join("bin");
    let old = bins.join("releases/old");
    std::fs::create_dir_all(&old).unwrap();
    std::os::unix::fs::symlink("releases/old", bins.join("current")).unwrap();
    for b in [
        "forge",
        "forge-web",
        "forge-portal",
        "forge-repomap",
        "forge-test",
        "forge-tui",
    ] {
        write_fake(&old.join(b), "#!/bin/sh\n# old\nexit 0\n");
    }
    bins
}

struct Upgrade {
    e: Env,
    scratch: PathBuf,
    bins: PathBuf,
    calls_log: PathBuf,
    path: String,
}

impl Upgrade {
    /// `units`: the unit file names (without `.service`) to pre-create
    /// under the test's own `XDG_CONFIG_HOME/systemd/user`, so
    /// `forge upgrade`'s "when their units exist" check sees exactly them.
    fn new(units: &[&str], curl_fake: &str) -> Upgrade {
        let e = Env::new();
        // Creates FORGE_HOME and a fresh forge.db for `forge upgrade` to
        // back up and read the schema version of.
        e.forge("ok.sh", &["doctor", "--json"]);
        std::fs::write(e.home.join("web.token"), "tok123\n").unwrap();

        let scratch = e._dir.path().join("release");
        std::fs::create_dir_all(&scratch).unwrap();
        let bins = old_bin_dir(&e.home);

        let unit_dir = e.xdg_config.join("systemd/user");
        std::fs::create_dir_all(&unit_dir).unwrap();
        for u in units {
            std::fs::write(unit_dir.join(format!("{u}.service")), "# test unit\n").unwrap();
        }

        let fakebin = e._dir.path().join("fakebin");
        std::fs::create_dir_all(&fakebin).unwrap();
        write_fake(&fakebin.join("systemctl"), FAKE_SYSTEMCTL);
        write_fake(&fakebin.join("curl"), curl_fake);
        let path = format!(
            "{}:{}",
            fakebin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let calls_log = e._dir.path().join("upgrade-calls.log");

        Upgrade {
            e,
            scratch,
            bins,
            calls_log,
            path,
        }
    }

    fn tarball(&self, version: &str) -> PathBuf {
        build_tarball(&self.scratch, version)
    }

    fn run(&self, args: &[&str]) -> Output {
        self.e
            .cmd("ok.sh")
            .env("PATH", &self.path)
            .env("UPGRADE_CALLS_LOG", &self.calls_log)
            .args(["upgrade"])
            .args(args)
            .output()
            .unwrap()
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.calls_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn link(&self, name: &str) -> String {
        std::fs::read_link(self.bins.join(name))
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    }

    fn binary(&self, name: &str) -> String {
        std::fs::read_to_string(self.bins.join("current").join(name)).unwrap_or_default()
    }
}

#[test]
fn forge_upgrade_installs_migrates_and_restarts_web_portal_then_worker_last() {
    let u = Upgrade::new(&["forge-web", "forge-portal", "forge-worker"], FAKE_CURL_OK);
    let tarball = u.tarball("9.9.9");

    let o = u.run(&[tarball.to_str().unwrap()]);
    let out = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(o.status.success(), "{out}");

    // The new (real) forge binary is live through current; the old
    // release is untouched and previous points at it.
    let real = std::fs::read(env!("CARGO_BIN_EXE_forge")).unwrap();
    assert_eq!(std::fs::read(u.bins.join("current/forge")).unwrap(), real);
    assert_eq!(u.link("current"), "releases/9.9.9");
    assert_eq!(u.link("previous"), "releases/old");
    assert!(
        std::fs::read_to_string(u.bins.join("previous/forge"))
            .unwrap()
            .contains("old")
    );
    for b in [
        "forge-web",
        "forge-portal",
        "forge-repomap",
        "forge-test",
        "forge-tui",
    ] {
        assert!(u.binary(b).contains("new"), "{b}: {}", u.binary(b));
        assert!(
            std::fs::read_to_string(u.bins.join(format!("releases/old/{b}")))
                .unwrap()
                .contains("old"),
            "{b}"
        );
    }

    // Schema reported before and after, web+portal restarted before the
    // check, and the worker asked to restart last, without blocking.
    assert!(out.contains("schema before: version"), "{out}");
    assert!(out.contains("schema after:  version"), "{out}");

    let calls = u.calls();
    let systemctl: Vec<&String> = calls
        .iter()
        .filter(|c| c.starts_with("systemctl") && !c.contains("is-active"))
        .collect();
    assert_eq!(
        systemctl,
        [
            "systemctl --user restart forge-web forge-portal",
            "systemctl --user restart --no-block forge-worker"
        ],
        "{calls:?}"
    );
    let curl = calls.iter().position(|c| c.starts_with("curl ")).unwrap();
    assert!(
        calls[curl].contains("Authorization: Bearer tok123")
            && calls[curl].contains("http://127.0.0.1:7788/tasks"),
        "{}",
        calls[curl]
    );
    assert_eq!(
        calls.last().unwrap(),
        "systemctl --user restart --no-block forge-worker",
        "{calls:?}"
    );
    assert!(curl < calls.len() - 1);

    assert!(out.contains("upgraded"), "{out}");
}

#[test]
fn forge_upgrade_only_restarts_units_that_exist() {
    // No forge-portal, no forge-worker unit: forge-web restarts and is
    // checked, nothing else is asked to.
    let u = Upgrade::new(&["forge-web"], FAKE_CURL_OK);
    let tarball = u.tarball("1.0.0");

    let o = u.run(&[tarball.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let calls = u.calls();
    let systemctl: Vec<&String> = calls
        .iter()
        .filter(|c| c.starts_with("systemctl") && !c.contains("is-active"))
        .collect();
    assert_eq!(
        systemctl,
        ["systemctl --user restart forge-web"],
        "{calls:?}: forge-portal and forge-worker have no unit here"
    );
    assert!(calls.iter().any(|c| c.starts_with("curl ")), "{calls:?}");
}

#[test]
fn forge_upgrade_check_only_verifies_and_changes_nothing() {
    let u = Upgrade::new(&["forge-web", "forge-portal", "forge-worker"], FAKE_CURL_OK);
    let tarball = u.tarball("9.9.9");

    let o = u.run(&[tarball.to_str().unwrap(), "--check-only"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{out}");
    assert!(out.contains("verified"), "{out}");
    assert!(out.contains("Nothing installed"), "{out}");

    assert_eq!(u.binary("forge"), "#!/bin/sh\n# old\nexit 0\n");
    assert!(!u.bins.join("previous").exists());
    assert_eq!(u.link("current"), "releases/old");
    assert!(!u.bins.join("releases/9.9.9").exists());
    assert!(out.contains("Would flip"), "{out}");
    assert!(u.calls().is_empty(), "{:?}", u.calls());
}

#[test]
fn forge_upgrade_refuses_a_tarball_older_than_the_running_version_without_force() {
    let u = Upgrade::new(&["forge-web", "forge-portal", "forge-worker"], FAKE_CURL_OK);
    let tarball = u.tarball("0.0.1");

    let o = u.run(&[tarball.to_str().unwrap()]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("older"), "{err}");
    assert!(err.contains("migrations do not run backwards"), "{err}");
    assert!(err.contains("--force"), "{err}");

    // Nothing was touched: the version gate runs before anything installs.
    assert_eq!(u.binary("forge"), "#!/bin/sh\n# old\nexit 0\n");
    assert!(!u.bins.join("previous").exists());
    assert!(u.calls().is_empty(), "{:?}", u.calls());
}

#[test]
fn forge_upgrade_force_installs_a_tarball_older_than_the_running_version() {
    let u = Upgrade::new(&["forge-web", "forge-portal", "forge-worker"], FAKE_CURL_OK);
    let tarball = u.tarball("0.0.1");

    let o = u.run(&[tarball.to_str().unwrap(), "--force"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let real = std::fs::read(env!("CARGO_BIN_EXE_forge")).unwrap();
    assert_eq!(std::fs::read(u.bins.join("current/forge")).unwrap(), real);
}

#[test]
fn forge_upgrade_refuses_an_older_tarball_than_the_live_release_without_force() {
    let u = Upgrade::new(&[], FAKE_CURL_OK);
    let newer = u.tarball("9.9.9");
    let o = u.run(&[newer.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let older = u.tarball("9.9.8");
    let o = u.run(&[older.to_str().unwrap()]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("older") && err.contains("--force"), "{err}");
    assert_eq!(u.link("current"), "releases/9.9.9");
    assert_eq!(u.link("previous"), "releases/old");

    let o = u.run(&[older.to_str().unwrap(), "--force"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(u.link("current"), "releases/9.9.8");
    assert_eq!(u.link("previous"), "releases/9.9.9");
}

#[test]
fn forge_upgrade_restores_the_previous_binaries_when_the_web_check_fails() {
    let u = Upgrade::new(
        &["forge-web", "forge-portal", "forge-worker"],
        FAKE_CURL_BAD,
    );
    let tarball = u.tarball("9.9.9");

    let o = u.run(&[tarball.to_str().unwrap()]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("restored the previous release"), "{err}");

    // What runs is what ran before: current points at the old release again.
    assert_eq!(u.link("current"), "releases/old");
    assert!(!u.bins.join("releases/9.9.9").exists());
    for b in [
        "forge",
        "forge-web",
        "forge-portal",
        "forge-repomap",
        "forge-test",
        "forge-tui",
    ] {
        assert!(u.binary(b).contains("old"), "{b}: {}", u.binary(b));
    }
    let calls = u.calls();
    assert!(calls.iter().any(|c| c.starts_with("curl ")), "{calls:?}");
    assert!(
        !calls.iter().any(|c| c.contains("forge-worker")),
        "the worker must not be restarted onto a build that failed its check: {calls:?}"
    );
    let restarts = calls
        .iter()
        .filter(|c| *c == "systemctl --user restart forge-web forge-portal")
        .count();
    assert_eq!(
        restarts, 2,
        "once before the check, once to roll back: {calls:?}"
    );
}

#[test]
fn forge_upgrade_refuses_a_tarball_whose_checksum_does_not_match() {
    let u = Upgrade::new(&[], FAKE_CURL_OK);
    let tarball = u.tarball("9.9.9");
    let sums = u.scratch.join("SHA256SUMS");
    let text = std::fs::read_to_string(&sums).unwrap();
    let hash = text.split_whitespace().next().unwrap().to_string();
    let bad_hash = "0".repeat(hash.len());
    std::fs::write(&sums, text.replacen(&hash, &bad_hash, 1)).unwrap();

    let o = u.run(&[tarball.to_str().unwrap()]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("sha256 mismatch"), "{err}");
    assert_eq!(u.binary("forge"), "#!/bin/sh\n# old\nexit 0\n");
}
