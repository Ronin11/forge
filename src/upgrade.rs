//! `forge upgrade [<path-or-url>] [--check-only] [--force]`: install a
//! release tarball over the running binaries (see docs/OPS.md,
//! "Upgrading"). Verifies the tarball against its own `SHA256SUMS`,
//! refuses one whose version is older than the running binary's unless
//! `--force` (migrations are forward-only), backs the store up with
//! sqlite's `.backup`, keeps the current binaries under `<bin
//! dir>/previous/`, installs the new ones, opens the store once — through
//! the newly installed `forge doctor --json`, so it is the new binary's
//! own migrations that run — and reports the schema version before and
//! after, restarts `forge-web` and `forge-portal` when their units exist,
//! checks the web client the way `deploy-self` does, and only then asks
//! `forge-worker` to restart, last, after its drain. Any failure once the
//! binaries have been touched restores the previous ones, leaves the
//! worker alone, and says so.
//!
//! Shares its shape with `deploy-self`
//! (`src/builtins/operations/deploy-self.toml`, docs/DEPLOY.md "Deploying
//! Forge itself") — snapshot to `previous/`, restart web and portal, wait,
//! check, restart the worker last, without blocking — but not its text.
//! `deploy-self` has to be one self-contained script that runs from an
//! archived checkout of whatever repository a deploy target names, under
//! the operation kernel's `FORGE_ARG_*` contract; nothing guarantees that
//! tree is Forge's own source (the e2e suite deploys a throwaway fixture
//! repo through it), so it cannot `source` a file out of it. `forge
//! upgrade` is a first-class CLI command with its own tarball,
//! `SHA256SUMS` and backup steps that have no home in that generic bash
//! contract. The two are kept honest against each other by mirroring the
//! same step order and restart/check/rollback semantics here in Rust,
//! rather than by literally sharing one file.

use crate::ctx::Paths;
use crate::store::Store;
use anyhow::{Context, Result, bail};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// The workspace's own release binaries, in the order `scripts/release.sh`
/// packs them (see `tests/release.rs`).
const BINS: &[&str] = &[
    "forge",
    "forge-web",
    "forge-portal",
    "forge-repomap",
    "forge-test",
    "forge-tui",
];

/// The units `forge upgrade` restarts when their unit files exist;
/// `forge-worker` is asked to restart separately, last.
const UNIT_NAMES: &[&str] = &["forge-web", "forge-portal"];

const CHECK_URL: &str = "http://127.0.0.1:7788/tasks";
const TRIES: u32 = 40;

fn scratch_dir(paths: &Paths) -> PathBuf {
    paths.worktrees.join("upgrade")
}

/// Where the running binaries live: `FORGE_UPGRADE_BIN_DIR` when set (so
/// the e2e suite can point this at a throwaway directory rather than the
/// real `target/debug` its own test binary runs from — the same shape as
/// `FORGE_CLAUDE_BIN`), else the currently running binary's own directory.
fn bin_dir() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("FORGE_UPGRADE_BIN_DIR") {
        return Ok(PathBuf::from(p));
    }
    std::env::current_exe()?
        .parent()
        .map(Path::to_path_buf)
        .context("the running binary has no parent directory")
}

/// A path or URL fetched with curl into a temp dir (see the module doc);
/// returns the tarball's own path and its `SHA256SUMS`'s.
fn obtain(source: &str, scratch: &Path) -> Result<(PathBuf, PathBuf)> {
    if source.starts_with("http://") || source.starts_with("https://") {
        let _ = std::fs::remove_dir_all(scratch);
        std::fs::create_dir_all(scratch)?;
        let name = source
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .with_context(|| format!("{source} names no file"))?;
        let base = &source[..source.len() - name.len()];
        let tarball = scratch.join(name);
        curl(source, &tarball)?;
        let sums = scratch.join("SHA256SUMS");
        curl(&format!("{base}SHA256SUMS"), &sums)?;
        Ok((tarball, sums))
    } else {
        let tarball = PathBuf::from(source);
        anyhow::ensure!(tarball.is_file(), "{} is not a file", tarball.display());
        let sums = tarball.with_file_name("SHA256SUMS");
        anyhow::ensure!(sums.is_file(), "no SHA256SUMS beside {}", tarball.display());
        Ok((tarball, sums))
    }
}

fn curl(url: &str, dest: &Path) -> Result<()> {
    let status = Command::new("curl")
        .args(["-fsSL", "--max-time", "120", "-o"])
        .arg(dest)
        .arg(url)
        .status()
        .with_context(|| format!("running curl for {url}"))?;
    anyhow::ensure!(status.success(), "curl {url} failed");
    Ok(())
}

/// `tarball`'s SHA-256 against the entry `sums` names for its own file
/// name (the format `sha256sum` writes and reads).
fn verify_sha256(tarball: &Path, sums: &Path) -> Result<()> {
    let name = tarball
        .file_name()
        .and_then(|n| n.to_str())
        .context("the tarball's name is not valid UTF-8")?;
    let text =
        std::fs::read_to_string(sums).with_context(|| format!("reading {}", sums.display()))?;
    let want = text
        .lines()
        .find_map(|l| {
            let mut parts = l.split_whitespace();
            let hash = parts.next()?;
            let file = parts.next()?.trim_start_matches('*');
            (file == name).then(|| hash.to_ascii_lowercase())
        })
        .with_context(|| format!("{} names no {name:?}", sums.display()))?;
    let bytes = std::fs::read(tarball).with_context(|| format!("reading {}", tarball.display()))?;
    let got = crate::job::sha256_hex(&bytes);
    anyhow::ensure!(
        got == want,
        "sha256 mismatch for {name}: {} says {want}, the file hashes to {got}",
        sums.display()
    );
    Ok(())
}

/// The version a release tarball names in its own file name
/// (`forge-<version>-<target>.tar.gz`, see `scripts/release.sh`) — read
/// from the name Forge itself chose it, the same source of truth
/// `scripts/release.sh` uses, rather than by running an unverified binary.
fn tarball_version(tarball: &Path) -> Result<String> {
    let name = tarball
        .file_name()
        .and_then(|n| n.to_str())
        .context("the tarball's name is not valid UTF-8")?;
    let stem = name
        .strip_suffix(".tar.gz")
        .with_context(|| format!("{name} does not end in .tar.gz"))?;
    let rest = stem
        .strip_prefix("forge-")
        .with_context(|| format!("{name} does not start with forge-<version>-"))?;
    let version = rest
        .split('-')
        .next()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("{name} names no version"))?;
    Ok(version.to_string())
}

/// `(major, minor, patch)`, missing segments read as 0.
fn semver(v: &str) -> Result<(u64, u64, u64)> {
    let mut it = v.split('.');
    let major: u64 = it
        .next()
        .unwrap_or("0")
        .parse()
        .with_context(|| format!("{v} is not a version"))?;
    let minor: u64 = it.next().map(str::parse).transpose()?.unwrap_or(0);
    let patch: u64 = it.next().map(str::parse).transpose()?.unwrap_or(0);
    Ok((major, minor, patch))
}

/// Extract into a fresh directory under `scratch` and return the one
/// top-level directory the archive holds (`forge-<version>-<target>/`,
/// see `scripts/release.sh`).
fn extract(tarball: &Path, scratch: &Path) -> Result<PathBuf> {
    let dest = scratch.join("extracted");
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::create_dir_all(&dest)?;
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(tarball)
        .arg("-C")
        .arg(&dest)
        .status()
        .context("running tar")?;
    anyhow::ensure!(status.success(), "tar -xzf {} failed", tarball.display());
    let mut entries = std::fs::read_dir(&dest)?.filter_map(|e| e.ok());
    let first = entries
        .next()
        .with_context(|| format!("{} extracted to nothing", tarball.display()))?;
    anyhow::ensure!(
        entries.next().is_none(),
        "{} extracted more than one top-level entry",
        tarball.display()
    );
    Ok(first.path())
}

/// `sqlite3`'s online `.backup` of the store into
/// `FORGE_HOME/backups/<version>-<unix time>/forge.db` — a consistent
/// snapshot while the worker keeps writing, the same mechanism
/// `backup-store.toml` uses (see docs/OPS.md, "backup-daily").
fn backup_store(home: &Path, version: &str) -> Result<PathBuf> {
    let db = home.join("forge.db");
    anyhow::ensure!(db.is_file(), "no store at {}", db.display());
    let dir = home
        .join("backups")
        .join(format!("{version}-{}", crate::unix_now()));
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join("forge.db");
    let status = Command::new("sqlite3")
        .arg(&db)
        .arg(".timeout 30000")
        .arg(format!(".backup '{}'", dest.display()))
        .status()
        .context("running sqlite3 .backup")?;
    anyhow::ensure!(
        status.success(),
        "sqlite3 .backup of {} failed",
        db.display()
    );
    Ok(dest)
}

/// Copy the bin dir's current binaries (those that exist) into
/// `previous.new/`, then swap it in as `previous/` — atomic from the
/// directory entry's point of view, the same copy-then-rename shape
/// `deploy-self.toml` uses. Returns whether there was anything to keep.
fn snapshot_previous(bin_dir: &Path) -> Result<bool> {
    let prev_new = bin_dir.join("previous.new");
    let prev = bin_dir.join("previous");
    let _ = std::fs::remove_dir_all(&prev_new);
    std::fs::create_dir_all(&prev_new)?;
    let mut have_prev = false;
    for b in BINS {
        let src = bin_dir.join(b);
        if src.is_file() {
            std::fs::copy(&src, prev_new.join(b))
                .with_context(|| format!("keeping the previous {b}"))?;
            have_prev = true;
        }
    }
    let _ = std::fs::remove_dir_all(&prev);
    std::fs::rename(&prev_new, &prev)?;
    Ok(have_prev)
}

/// Copy `extracted`'s binaries into `bin_dir`, executable, via a
/// copy-then-rename so a binary already running is never written to.
fn install_binaries(bin_dir: &Path, extracted: &Path) -> Result<()> {
    for b in BINS {
        let src = extracted.join(b);
        anyhow::ensure!(src.is_file(), "the tarball has no {b}");
        let tmp = bin_dir.join(format!("{b}.new"));
        std::fs::copy(&src, &tmp).with_context(|| format!("installing {b}"))?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        std::fs::rename(&tmp, bin_dir.join(b))?;
    }
    Ok(())
}

/// Restore `previous/` over `bin_dir`'s binaries and restart whatever web
/// or portal units exist, best-effort — never the worker, so a rollback
/// leaves it running the binary it already trusted (see
/// docs/DEPLOY.md, "Rollback").
fn restore_previous(bin_dir: &Path, have_prev: bool) {
    if !have_prev {
        println!("upgrade: no previous binaries to restore");
        return;
    }
    println!(
        "upgrade: restoring the previous binaries from {}",
        bin_dir.join("previous").display()
    );
    let prev = bin_dir.join("previous");
    for b in BINS {
        let src = prev.join(b);
        if !src.is_file() {
            continue;
        }
        let tmp = bin_dir.join(format!("{b}.restore"));
        if std::fs::copy(&src, &tmp).is_ok() {
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
            let _ = std::fs::rename(&tmp, bin_dir.join(b));
        }
    }
    let units = existing_units();
    let _ = restart_units_and_wait(&units, TRIES);
}

fn unit_exists(name: &str) -> bool {
    crate::init::systemd_user_dir()
        .map(|d| d.join(format!("{name}.service")).exists())
        .unwrap_or(false)
}

fn existing_units() -> Vec<&'static str> {
    UNIT_NAMES
        .iter()
        .copied()
        .filter(|u| unit_exists(u))
        .collect()
}

fn restart_units_and_wait(units: &[&str], tries: u32) -> Result<()> {
    if units.is_empty() {
        return Ok(());
    }
    let status = Command::new("systemctl")
        .args(["--user", "restart"])
        .args(units)
        .status()
        .context("running systemctl restart")?;
    anyhow::ensure!(
        status.success(),
        "systemctl --user restart {} failed",
        units.join(" ")
    );
    for u in units {
        wait_active(u, tries)?;
    }
    Ok(())
}

fn wait_active(unit: &str, tries: u32) -> Result<()> {
    for _ in 0..tries {
        if let Ok(out) = Command::new("systemctl")
            .args(["--user", "is-active", unit])
            .output()
            && String::from_utf8_lossy(&out.stdout).trim() == "active"
        {
            println!("{unit} is active");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    bail!("{unit} did not become active")
}

/// The default check `deploy-self` runs: curl the web client's `/tasks`
/// with the token from `FORGE_HOME/web.token`, expecting 200, retried
/// while it fails up to `tries`.
fn check_web(home: &Path, tries: u32) -> Result<()> {
    let token = std::fs::read_to_string(home.join("web.token")).unwrap_or_default();
    let token = token.trim();
    for _ in 0..tries {
        if let Ok(out) = Command::new("curl")
            .args(["-s", "-o", "/dev/null", "-w", "%{http_code}"])
            .args(["-H", &format!("Authorization: Bearer {token}")])
            .arg(CHECK_URL)
            .output()
        {
            let code = String::from_utf8_lossy(&out.stdout).to_string();
            println!("GET {CHECK_URL}: {code}");
            if code == "200" {
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    bail!("the web client's check did not pass after {tries} tries")
}

/// `systemctl --user restart --no-block forge-worker`: the worker drains
/// its running attempts, this command's own process included were it the
/// worker, and comes back on the new binary. Never called from
/// `restore_previous`.
fn restart_worker() -> Result<()> {
    let status = Command::new("systemctl")
        .args(["--user", "restart", "--no-block", "forge-worker"])
        .status()
        .context("running systemctl restart --no-block forge-worker")?;
    anyhow::ensure!(
        status.success(),
        "systemctl --user restart --no-block forge-worker failed"
    );
    println!("asked forge-worker to restart once its running attempts finish");
    Ok(())
}

/// Open the store once through the newly installed `forge doctor --json`
/// — so it is the new binary's own migrations that run, forward-only —
/// and return the schema version its `schema` check reports.
fn migrate_via_new_binary(bin_dir: &Path, home: &Path) -> Result<i64> {
    let out = Command::new(bin_dir.join("forge"))
        .args(["doctor", "--json"])
        .env("FORGE_HOME", home)
        .output()
        .context("running the newly installed forge doctor --json")?;
    let checks: serde_json::Value = serde_json::from_slice(&out.stdout).with_context(|| {
        format!(
            "the newly installed forge did not print doctor JSON: {}",
            String::from_utf8_lossy(&out.stderr)
        )
    })?;
    let schema = checks
        .as_array()
        .and_then(|a| a.iter().find(|c| c["name"] == "schema"))
        .context("the newly installed forge's doctor reported no schema check")?;
    anyhow::ensure!(
        schema["status"] == "ok",
        "the newly installed forge's schema check failed: {}",
        schema["detail"]
    );
    let detail = schema["detail"].as_str().unwrap_or_default();
    detail
        .strip_prefix("version ")
        .and_then(|v| v.parse().ok())
        .with_context(|| format!("could not read the schema version from {detail:?}"))
}

/// Everything after the binaries are on disk: migrate, restart web and
/// portal (when their units exist) and check, then ask the worker to
/// restart last. Returns the post-migration schema version.
fn bring_up(bin_dir: &Path, home: &Path) -> Result<i64> {
    let after = migrate_via_new_binary(bin_dir, home)?;
    println!("schema after:  version {after}");

    let units = existing_units();
    if units.is_empty() {
        println!("no forge-web or forge-portal unit found; nothing to restart there");
    }
    restart_units_and_wait(&units, TRIES)?;

    if units.contains(&"forge-web") {
        check_web(home, TRIES)?;
    }

    if unit_exists("forge-worker") {
        restart_worker()?;
    } else {
        println!("no forge-worker unit found; restart it yourself once ready");
    }

    Ok(after)
}

pub fn run(source: Option<String>, check_only: bool, force: bool) -> Result<()> {
    let source = source.context("forge upgrade needs a path or URL to a release tarball")?;
    let paths = Paths::resolve()?;
    let scratch = scratch_dir(&paths);

    let (tarball, sums) = obtain(&source, &scratch)?;
    verify_sha256(&tarball, &sums)?;
    println!("verified {} against {}", tarball.display(), sums.display());

    let new_version = tarball_version(&tarball)?;
    let old_version = env!("CARGO_PKG_VERSION");
    if !force && semver(&new_version)? < semver(old_version)? {
        bail!(
            "{new_version} is older than the running {old_version}; migrations do not run \
             backwards, so this tarball is refused. Pass --force to install it anyway."
        );
    }

    if check_only {
        println!(
            "{new_version} verified; the running binary is {old_version}. Nothing installed \
             (--check-only)."
        );
        return Ok(());
    }

    let extracted = extract(&tarball, &scratch)?;
    for b in BINS {
        anyhow::ensure!(extracted.join(b).is_file(), "the tarball has no {b}");
    }

    let before = Store::open(&paths.home.join("forge.db"))?.schema_version()?;
    println!("schema before: version {before}");

    let backup = backup_store(&paths.home, &new_version)?;
    println!("backed up the store to {}", backup.display());

    let bin_dir = bin_dir()?;

    let have_prev = snapshot_previous(&bin_dir)?;
    if let Err(e) =
        install_binaries(&bin_dir, &extracted).and_then(|()| bring_up(&bin_dir, &paths.home))
    {
        restore_previous(&bin_dir, have_prev);
        return Err(e.context("forge upgrade failed; restored the previous binaries"));
    }

    println!("upgraded {old_version} -> {new_version}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tarball_version_reads_the_release_naming() {
        assert_eq!(
            tarball_version(Path::new("/x/forge-0.3.1-x86_64-unknown-linux-gnu.tar.gz")).unwrap(),
            "0.3.1"
        );
        assert!(tarball_version(Path::new("/x/nope.tar.gz")).is_err());
        assert!(tarball_version(Path::new("/x/forge-0.3.1.zip")).is_err());
    }

    #[test]
    fn semver_orders_older_tarballs_below_the_running_version() {
        assert!(semver("0.1.9").unwrap() < semver("0.2.0").unwrap());
        assert!(semver("0.2.0").unwrap() == semver("0.2").unwrap());
        assert!(semver("1.0.0").unwrap() > semver("0.99.99").unwrap());
    }

    #[test]
    fn verify_sha256_matches_the_file_name_not_just_any_line() {
        let dir = tempfile::tempdir().unwrap();
        let tarball = dir.path().join("forge-0.2.0-x.tar.gz");
        std::fs::write(&tarball, b"hello").unwrap();
        let got = crate::job::sha256_hex(b"hello");
        let sums = dir.path().join("SHA256SUMS");
        std::fs::write(
            &sums,
            format!("deadbeef  other-file.tar.gz\n{got}  forge-0.2.0-x.tar.gz\n"),
        )
        .unwrap();
        assert!(verify_sha256(&tarball, &sums).is_ok());

        std::fs::write(&sums, format!("{got}  other-file.tar.gz\n")).unwrap();
        assert!(verify_sha256(&tarball, &sums).is_err());
    }
}
