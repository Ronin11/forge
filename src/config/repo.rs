//! The repository's own config: `forge.toml` (or `.forge/forge.toml`), the
//! trusted base every attempt is verified against (see the crate doc
//! comment). Read from the working tree before a task's base commit is
//! pinned (`load_working*`), and from that pinned revision once an attempt
//! starts (`load_at`).

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Deserialize, Default)]
struct Raw {
    #[serde(default)]
    execution: Execution,
    #[serde(default)]
    checks: ChecksRaw,
    #[serde(default)]
    defaults: Defaults,
    #[serde(default)]
    verify: VerifyRaw,
    #[serde(default)]
    sandbox: RepoSandboxRaw,
    #[serde(default)]
    environment: RepoEnvironmentRaw,
}

/// `[environment]` in the repository's forge.toml: what the repository
/// forbids the supervisor to grant it, however the operator's table reads.
/// A host (`registry.example.com`, `*.example.com`) or a cache path
/// (`~/.cache/foo`). Read from the trusted base, like the rest of the file.
#[derive(Deserialize, Default)]
struct RepoEnvironmentRaw {
    #[serde(default)]
    deny: Vec<String>,
}

/// `[sandbox]` in the repository's forge.toml: what an attempt on this
/// repository may reach on the network, beyond the model endpoint (always
/// allowed). Not the operator's `[sandbox]` in config.toml, which is about
/// paths. Read from the trusted base like everything else in forge.toml, so
/// an attempt cannot widen its own allowlist.
#[derive(Deserialize, Default)]
struct RepoSandboxRaw {
    #[serde(default)]
    env: BTreeMap<String, String>,
    /// `host`, `host:port`, `*.suffix` or `*.suffix:port`; see `egress::Rule`.
    #[serde(default)]
    egress: Vec<String>,
}

/// `[checks]`: the ordinary name -> argv entries, flattened, plus the one
/// reserved sub-table `[checks.fixable]` naming, for a check the repository
/// already declares, the command that fixes what it flags (e.g. `cargo fmt
/// --all` for a `fmt` check that only checks). `#[serde(flatten)]` is what
/// makes this split possible: serde tries the named field (`fixable`)
/// first and folds every other key into the flattened map, so an ordinary
/// check is never mistaken for the reserved table and vice versa.
#[derive(Deserialize, Default)]
struct ChecksRaw {
    #[serde(default)]
    fixable: BTreeMap<String, Vec<String>>,
    #[serde(flatten)]
    checks: BTreeMap<String, Vec<String>>,
}

#[derive(Deserialize, Default)]
struct VerifyRaw {
    /// Paths an attempt may not change unless the task allows it: the tests
    /// that guard the product, fixtures, CI config. A file, or a directory
    /// with a trailing slash.
    #[serde(default)]
    protected: Vec<String>,
    /// Directories owned by verification: hidden from the coder, overlaid
    /// from the trusted refs at verify time. Must not exist in the base tree.
    #[serde(default)]
    namespace: Vec<String>,
}

#[derive(Deserialize, Default)]
struct Defaults {
    base_branch: Option<String>,
    remote: Option<String>,
    push: Option<bool>,
    check_timeout_secs: Option<u64>,
}

#[derive(Clone, Deserialize, Default)]
pub struct Execution {
    /// `None` when the repository names no backend: the machine decides
    /// (bwrap where it is installed, else host; `executor::default_backend`).
    #[serde(rename = "backend", default)]
    pub declared: Option<crate::executor::Backend>,
    pub host: Option<String>,
    pub user: Option<String>,
}
impl Execution {
    /// The backend this repository runs on, on this machine.
    pub fn backend(&self) -> crate::executor::Backend {
        self.declared
            .unwrap_or_else(crate::executor::default_backend)
    }
    pub fn ssh_destination(&self) -> Result<String> {
        let valid = |s: &str| {
            !s.is_empty()
                && !s.starts_with('-')
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || ".-_".contains(c))
        };
        let host = self
            .host
            .as_deref()
            .filter(|s| valid(s))
            .context("execution.host must be a hostname for the ssh backend")?;
        match self.user.as_deref() {
            Some(user) if valid(user) => Ok(format!("{user}@{host}")),
            Some(_) => bail!("execution.user must be an SSH username"),
            None => Ok(host.to_owned()),
        }
    }
    fn validate(&self) -> Result<()> {
        if self.declared == Some(crate::executor::Backend::Ssh) {
            self.ssh_destination()?;
        }
        Ok(())
    }
}

pub struct Config {
    pub build_env: BTreeMap<String, String>,
    pub execution: Execution,
    pub checks: BTreeMap<String, Vec<String>>,
    /// `[checks.fixable]`: for a check named here, the command that fixes
    /// what it flags, run by the engine before any agent repair when that
    /// check is the only thing an attempt failed (see docs/ACTIONS.md).
    /// Every key is one of `checks`' own, checked in `parse`.
    pub fixable: BTreeMap<String, Vec<String>>,
    pub base_branch: String,
    /// Remote to push succeeded branches to; `None` when the repo has no
    /// such remote or `push = false`.
    pub push_remote: Option<String>,
    pub check_timeout_secs: u64,
    pub protected: Vec<String>,
    pub namespace: Vec<String>,
    /// `[sandbox] egress`: the hosts an attempt on this repository may
    /// reach besides the model endpoint, typically the package registries
    /// its checks install from. Empty by default: nothing else.
    pub egress: Vec<crate::egress::Rule>,
    /// `[environment] deny`: hosts and cache paths the supervisor may not
    /// grant this repository (see `environment::within_ceiling`).
    pub environment_deny: Vec<String>,
    /// Where the repository's config actually lives: `forge.toml` or
    /// `.forge/forge.toml`. Whatever this is, it is the path every rule
    /// that used to say `forge.toml` by name now means.
    pub config_path: String,
}

/// The repository's config location: `.forge/forge.toml` if it exists,
/// else `forge.toml` at the root. Both existing is an error naming both.
const ALT_CONFIG_PATH: &str = ".forge/forge.toml";
const ROOT_CONFIG_PATH: &str = "forge.toml";

/// Whether `path` is inside a write scope: a file, a directory with a
/// trailing slash, or a `*.ext` suffix pattern.
pub fn in_scope(scope: &[String], path: &str) -> bool {
    scope.iter().any(|p| {
        if let Some(suffix) = p.strip_prefix('*') {
            path.ends_with(suffix)
        } else if let Some(dir) = p.strip_suffix('/') {
            path.starts_with(dir) && path[dir.len()..].starts_with('/')
        } else {
            p == path
        }
    })
}

/// Whether `path` falls under one of the protected entries.
pub fn is_protected(protected: &[String], path: &str) -> bool {
    protected.iter().any(|p| {
        if let Some(dir) = p.strip_suffix('/') {
            path.starts_with(dir) && path[dir.len()..].starts_with('/')
        } else {
            p == path
        }
    })
}

async fn parse(repo: &Path, text: &str, what: &str, config_path: &str) -> Result<Config> {
    let raw: Raw = toml::from_str(text).with_context(|| format!("parsing {what}"))?;
    raw.execution.validate()?;
    crate::config::capacity::validate_env(&raw.sandbox.env)?;
    for (name, argv) in &raw.checks.checks {
        if argv.is_empty() {
            bail!("check `{name}` has an empty command");
        }
    }
    for (name, argv) in &raw.checks.fixable {
        if argv.is_empty() {
            bail!("checks.fixable.{name} has an empty command");
        }
        if !raw.checks.checks.contains_key(name) {
            bail!("checks.fixable.{name}: no such check `{name}` declared in [checks]");
        }
    }
    let egress = raw
        .sandbox
        .egress
        .iter()
        .map(|e| crate::egress::Rule::parse(e).map_err(|e| e.context("sandbox.egress")))
        .collect::<Result<Vec<_>>>()
        .with_context(|| format!("parsing {what}"))?;
    let base_branch = match raw.defaults.base_branch {
        Some(b) => b,
        None => crate::git::current_branch(repo).await?,
    };
    let remote = raw.defaults.remote.unwrap_or_else(|| "origin".to_string());
    let push_remote = if raw.defaults.push.unwrap_or(true)
        && crate::git::remote_url(repo, &remote).await.is_some()
    {
        Some(remote)
    } else {
        None
    };
    Ok(Config {
        build_env: raw.sandbox.env,
        execution: raw.execution,
        checks: raw.checks.checks,
        fixable: raw.checks.fixable,
        base_branch,
        push_remote,
        check_timeout_secs: raw.defaults.check_timeout_secs.unwrap_or(600),
        // `forge.toml` by name is the long-standing convention for
        // protecting the repository's own config; when the config actually
        // lives elsewhere, that convention must protect the real path.
        protected: raw
            .verify
            .protected
            .into_iter()
            .map(|p| {
                if p == ROOT_CONFIG_PATH {
                    config_path.to_string()
                } else {
                    p
                }
            })
            .collect(),
        namespace: raw
            .verify
            .namespace
            .into_iter()
            .map(|d| if d.ends_with('/') { d } else { format!("{d}/") })
            .collect(),
        egress,
        environment_deny: raw.environment.deny,
        config_path: config_path.to_string(),
    })
}

/// The repository's config as it is in the working tree: `.forge/forge.toml`
/// if it exists, else `forge.toml` at the root. Both existing is an error.
/// Used when a task is created, before any base commit is pinned.
pub async fn load_working(repo: &Path) -> Result<Config> {
    let (path, config_path, text) = read_working(repo)?;
    parse(repo, &text, &path.display().to_string(), config_path).await
}

/// Just the `[checks]` table of the config in the working tree at `dir`,
/// which need not be a git repository: `load_working` asks git for the
/// base branch and the push remote, and a job's scratch tree, an archive,
/// has no `.git` to answer with.
pub fn load_working_checks(dir: &Path) -> Result<BTreeMap<String, Vec<String>>> {
    let (path, _, text) = read_working(dir)?;
    let raw: Raw = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    for (name, argv) in &raw.checks.checks {
        if argv.is_empty() {
            bail!("check `{name}` has an empty command");
        }
    }
    Ok(raw.checks.checks)
}

/// The `[sandbox] egress` declared in the working tree's config at `dir`:
/// what `forge doctor` shows for each project. Sync and git-free, like
/// `load_working_checks`; attempts read the same table from the base commit.
pub fn load_working_egress(dir: &Path) -> Result<Vec<crate::egress::Rule>> {
    let (path, _, text) = read_working(dir)?;
    let raw: Raw = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    raw.sandbox
        .egress
        .iter()
        .map(|e| crate::egress::Rule::parse(e))
        .collect::<Result<Vec<_>>>()
        .with_context(|| format!("{}: sandbox.egress", path.display()))
}

pub fn load_working_execution(dir: &Path) -> Result<Execution> {
    let (_, _, text) = read_working(dir)?;
    let raw: Raw = toml::from_str(&text)?;
    raw.execution.validate()?;
    crate::config::capacity::validate_env(&raw.sandbox.env)?;
    Ok(raw.execution)
}

fn read_working(repo: &Path) -> Result<(PathBuf, &'static str, String)> {
    let alt = repo.join(ALT_CONFIG_PATH);
    let root = repo.join(ROOT_CONFIG_PATH);
    match (alt.exists(), root.exists()) {
        (true, true) => bail!(
            "both {} and {} exist; a repository must declare its checks in only one",
            alt.display(),
            root.display()
        ),
        (true, false) => {
            let text = std::fs::read_to_string(&alt)
                .with_context(|| format!("reading {}", alt.display()))?;
            Ok((alt, ALT_CONFIG_PATH, text))
        }
        (false, _) => {
            let text = std::fs::read_to_string(&root).with_context(|| {
                format!(
                    "{}: a repository must declare its checks in forge.toml",
                    root.display()
                )
            })?;
            Ok((root, ROOT_CONFIG_PATH, text))
        }
    }
}

/// The repository's config at `rev`: the trusted base for an attempt.
/// `.forge/forge.toml` if it exists there, else `forge.toml` at the root.
/// `repo` answers questions about remotes; `show_dir` is where `rev` is read
/// from (the task's clone, which has the same objects).
pub async fn load_at(repo: &Path, show_dir: &Path, rev: &str) -> Result<Config> {
    let alt = crate::git::show_file(show_dir, rev, ALT_CONFIG_PATH).await?;
    let root = crate::git::show_file(show_dir, rev, ROOT_CONFIG_PATH).await?;
    let (config_path, text) = match (alt, root) {
        (Some(_), Some(_)) => bail!(
            "both {ALT_CONFIG_PATH} and {ROOT_CONFIG_PATH} exist at {rev}; a repository must declare its checks in only one"
        ),
        (Some(t), None) => (ALT_CONFIG_PATH, t),
        (None, Some(t)) => (ROOT_CONFIG_PATH, t),
        (None, None) => bail!("forge.toml does not exist at {rev}"),
    };
    parse(repo, &text, &format!("{config_path} at {rev}"), config_path).await
}

/// Build tuning in an archived tree, without asking it for Git metadata.
pub fn load_working_build_env(dir: &Path) -> Result<BTreeMap<String, String>> {
    let (path, _, text) = read_working(dir)?;
    let raw: Raw = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    super::capacity::validate_env(&raw.sandbox.env)?;
    Ok(raw.sandbox.env)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_matches_dirs_files_and_suffixes() {
        let sc = vec![
            "docs/".to_string(),
            "*.md".to_string(),
            "CHANGELOG".to_string(),
        ];
        assert!(in_scope(&sc, "docs/a/b.txt"));
        assert!(in_scope(&sc, "README.md"));
        assert!(in_scope(&sc, "src/deep/notes.md"));
        assert!(in_scope(&sc, "CHANGELOG"));
        assert!(!in_scope(&sc, "src/main.rs"));
        assert!(!in_scope(&sc, "docsx/a"));
    }

    #[test]
    fn protected_matches_files_and_directories() {
        let p = vec![
            "tests/progression.test.ts".to_string(),
            "fixtures/".to_string(),
        ];
        assert!(is_protected(&p, "tests/progression.test.ts"));
        assert!(!is_protected(&p, "tests/progression.test.ts.bak"));
        assert!(is_protected(&p, "fixtures/a.json"));
        assert!(!is_protected(&p, "fixtures2/a.json"));
        assert!(!is_protected(&p, "src/x.ts"));
    }

    #[tokio::test]
    async fn both_config_locations_present_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        std::fs::create_dir(repo.join(".forge")).unwrap();
        std::fs::write(repo.join(".forge/forge.toml"), "[checks]\n").unwrap();
        std::fs::write(repo.join("forge.toml"), "[checks]\n").unwrap();
        let err = match load_working(repo).await {
            Ok(_) => panic!("expected an error"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains(".forge/forge.toml"), "{err}");
        assert!(err.contains("forge.toml"), "{err}");
    }

    #[tokio::test]
    async fn fixable_is_split_from_the_ordinary_checks_and_exposed_on_config() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        std::fs::write(
            repo.join("forge.toml"),
            "[defaults]\nbase_branch = \"main\"\n\
             [checks]\n\
             fmt = [\"cargo\", \"fmt\", \"--all\", \"--check\"]\n\
             clippy = [\"cargo\", \"clippy\"]\n\
             \n\
             [checks.fixable]\n\
             fmt = [\"cargo\", \"fmt\", \"--all\"]\n\
             clippy = [\"cargo\", \"clippy\", \"--fix\"]\n",
        )
        .unwrap();
        let c = load_working(repo).await.unwrap();
        assert_eq!(
            c.checks.keys().collect::<Vec<_>>(),
            vec!["clippy", "fmt"],
            "the fixable sub-table must not be mistaken for a check named `fixable`"
        );
        assert_eq!(c.fixable["fmt"], vec!["cargo", "fmt", "--all"]);
        assert_eq!(c.fixable["clippy"], vec!["cargo", "clippy", "--fix"]);
    }

    #[tokio::test]
    async fn fixable_naming_an_undeclared_check_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        std::fs::write(
            repo.join("forge.toml"),
            "[checks]\nfmt = [\"cargo\", \"fmt\", \"--check\"]\n\
             [checks.fixable]\nclippy = [\"cargo\", \"clippy\", \"--fix\"]\n",
        )
        .unwrap();
        let err = match load_working(repo).await {
            Ok(_) => panic!("expected an error"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("clippy"), "{err}");
    }

    #[tokio::test]
    async fn fixable_with_an_empty_command_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        std::fs::write(
            repo.join("forge.toml"),
            "[checks]\nfmt = [\"cargo\", \"fmt\", \"--check\"]\n\
             [checks.fixable]\nfmt = []\n",
        )
        .unwrap();
        let err = match load_working(repo).await {
            Ok(_) => panic!("expected an error"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("fmt"), "{err}");
    }

    #[tokio::test]
    async fn sandbox_egress_parses_into_rules_and_defaults_to_none() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        std::fs::write(
            repo.join("forge.toml"),
            "[defaults]\nbase_branch = \"main\"\n[checks]\nt = [\"true\"]\n",
        )
        .unwrap();
        assert!(load_working(repo).await.unwrap().egress.is_empty());
        std::fs::write(
            repo.join("forge.toml"),
            "[defaults]\nbase_branch = \"main\"\n[checks]\nt = [\"true\"]\n\
             [sandbox]\negress = [\"registry.npmjs.org\", \"*.crates.io\", \"dev.home:11434\"]\n",
        )
        .unwrap();
        let c = load_working(repo).await.unwrap();
        let rules: Vec<String> = c.egress.iter().map(|r| r.to_string()).collect();
        assert_eq!(
            rules,
            ["registry.npmjs.org", "*.crates.io", "dev.home:11434"]
        );
    }

    #[tokio::test]
    async fn sandbox_egress_refuses_urls_and_wildcards_that_allow_the_world() {
        for bad in [
            "https://registry.npmjs.org",
            "registry.npmjs.org/x",
            "*",
            "*.com",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let repo = dir.path();
            std::fs::write(
                repo.join("forge.toml"),
                format!("[defaults]\nbase_branch = \"main\"\n[checks]\nt = [\"true\"]\n[sandbox]\negress = [{bad:?}]\n"),
            )
            .unwrap();
            let err = match load_working(repo).await {
                Ok(_) => panic!("{bad:?} should be refused"),
                Err(e) => format!("{e:#}"),
            };
            assert!(err.contains("egress"), "{err}");
        }
    }
}
