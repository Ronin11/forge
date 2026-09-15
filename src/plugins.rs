//! Plugins: a directory whose base name matches its manifest name, holding
//! `plugin.toml`. Discovery follows the same shape as `src/workflows.rs`
//! and for the same reason: one broken `plugin.toml` must not stop the
//! others loading. See docs/PLUGINS.md.

use crate::workflows::Problem;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Capability {
    Events,
    Intake,
}

impl Capability {
    pub fn as_str(self) -> &'static str {
        match self {
            Capability::Events => "events",
            Capability::Intake => "intake",
        }
    }
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The supervision policy. `on-failure` (the default) restarts only on a
/// non-zero exit; `always` restarts unconditionally; `never` leaves it
/// stopped. Supervision itself is not implemented yet (see docs/PLUGINS.md).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Restart {
    Always,
    #[default]
    OnFailure,
    Never,
}

impl Restart {
    pub fn as_str(self) -> &'static str {
        match self {
            Restart::Always => "always",
            Restart::OnFailure => "on-failure",
            Restart::Never => "never",
        }
    }
}

impl std::fmt::Display for Restart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestRaw {
    name: String,
    #[serde(default)]
    description: String,
    run: Vec<String>,
    build: Option<Vec<String>>,
    capabilities: Vec<Capability>,
    #[serde(default)]
    restart: Restart,
}

/// One plugin's `plugin.toml`, parsed and validated.
#[derive(Clone, Debug)]
pub struct Manifest {
    pub name: String,
    pub description: String,
    pub run: Vec<String>,
    pub build: Option<Vec<String>>,
    pub capabilities: BTreeSet<Capability>,
    pub restart: Restart,
}

fn parse_manifest(path: &Path, text: &str, dir_name: &str) -> Result<Manifest> {
    let raw: ManifestRaw =
        toml::from_str(text).with_context(|| format!("parsing {}", path.display()))?;
    if raw.name != dir_name {
        bail!(
            "{}: name {:?} does not match the directory name {:?}",
            path.display(),
            raw.name,
            dir_name
        );
    }
    if raw.run.is_empty() {
        bail!("{}: `run` is empty", path.display());
    }
    if raw.build.as_ref().is_some_and(|b| b.is_empty()) {
        bail!("{}: `build` is empty", path.display());
    }
    let capabilities: BTreeSet<Capability> = raw.capabilities.into_iter().collect();
    if capabilities.is_empty() {
        bail!("{}: `capabilities` is empty", path.display());
    }
    Ok(Manifest {
        name: raw.name,
        description: raw.description,
        run: raw.run,
        build: raw.build,
        capabilities,
        restart: raw.restart,
    })
}

/// One plugin as discovered: its manifest, its directory, and the root it
/// was found under.
#[derive(Clone, Debug)]
pub struct Plugin {
    pub name: String,
    pub manifest: Manifest,
    pub dir: PathBuf,
    pub root: PathBuf,
}

/// Every plugin found across every root, loaded once: one read of each
/// root directory, per-file error capture. A `plugin.toml` that fails to
/// parse contributes a problem instead of aborting the load, so one bad
/// plugin does not hide the rest.
pub struct Catalog {
    pub plugins: BTreeMap<String, Plugin>,
    pub problems: Vec<Problem>,
}

/// Discover plugins over the ordered roots: `<home>/plugins` first, then
/// every entry of `plugin_dirs` in the order given. An earlier root wins a
/// duplicate name; the shadowed copy is a non-blocking problem. A
/// `plugin_dirs` entry that does not exist is a non-blocking problem;
/// `<home>/plugins` not existing is not (nothing has been installed yet).
pub fn load_catalog(home: &Path, plugin_dirs: &[PathBuf]) -> Catalog {
    let mut plugins: BTreeMap<String, Plugin> = BTreeMap::new();
    let mut problems = Vec::new();

    let roots: Vec<(PathBuf, bool)> = std::iter::once((home.join("plugins"), false))
        .chain(plugin_dirs.iter().cloned().map(|p| (p, true)))
        .collect();

    for (root, configured) in roots {
        let entries = match std::fs::read_dir(&root) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if configured {
                    problems.push(Problem {
                        file: root.display().to_string(),
                        blocking: false,
                        what: "directory does not exist".into(),
                    });
                }
                continue;
            }
            Err(e) => {
                problems.push(Problem {
                    file: root.display().to_string(),
                    blocking: false,
                    what: format!("{e}"),
                });
                continue;
            }
        };
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();

        for dir in dirs {
            let name = dir.file_name().unwrap().to_string_lossy().into_owned();
            let manifest_path = dir.join("plugin.toml");
            let text = match std::fs::read_to_string(&manifest_path) {
                Ok(t) => t,
                Err(e) => {
                    problems.push(Problem {
                        file: format!("{name}/plugin.toml"),
                        blocking: true,
                        what: format!("{e}"),
                    });
                    continue;
                }
            };
            match parse_manifest(&manifest_path, &text, &name) {
                Ok(manifest) => {
                    if plugins.contains_key(&name) {
                        problems.push(Problem {
                            file: dir.display().to_string(),
                            blocking: false,
                            what: format!(
                                "plugin {name:?} is shadowed by an earlier root and not loaded"
                            ),
                        });
                        continue;
                    }
                    plugins.insert(
                        name,
                        Plugin {
                            name: manifest.name.clone(),
                            manifest,
                            dir: dir.clone(),
                            root: root.clone(),
                        },
                    );
                }
                Err(e) => {
                    problems.push(Problem {
                        file: format!("{name}/plugin.toml"),
                        blocking: true,
                        what: format!("{e:#}"),
                    });
                }
            }
        }
    }

    Catalog { plugins, problems }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes `<root>/<name>/plugin.toml`; `root` is a root directory
    /// itself (what `load_catalog` scans directly, e.g. a `plugin_dirs`
    /// entry), not `<FORGE2_HOME>`.
    fn write_plugin(root: &Path, name: &str, text: &str) {
        let plugin_dir = root.join(name);
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join("plugin.toml"), text).unwrap();
    }

    /// Writes `<home>/plugins/<name>/plugin.toml`, the built-in root
    /// `load_catalog` always scans first.
    fn write_manifest(home: &Path, name: &str, text: &str) {
        write_plugin(&home.join("plugins"), name, text);
    }

    #[test]
    fn manifest_parses_with_defaults() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "notify",
            "name = \"notify\"\ndescription = \"posts a notification\"\nrun = [\"./notify.sh\"]\ncapabilities = [\"events\"]\n",
        );
        let cat = load_catalog(dir.path(), &[]);
        assert!(cat.problems.is_empty(), "{:?}", cat.problems);
        let p = cat.plugins.get("notify").unwrap();
        assert_eq!(p.manifest.description, "posts a notification");
        assert_eq!(p.manifest.run, vec!["./notify.sh"]);
        assert_eq!(p.manifest.build, None);
        assert_eq!(
            p.manifest.capabilities,
            [Capability::Events].into_iter().collect()
        );
        assert_eq!(p.manifest.restart, Restart::OnFailure, "the default");
    }

    #[test]
    fn manifest_parses_every_field() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "inbox",
            "name = \"inbox\"\ndescription = \"files tasks\"\nrun = [\"./inbox\"]\nbuild = [\"cargo\", \"build\", \"--release\"]\ncapabilities = [\"events\", \"intake\"]\nrestart = \"always\"\n",
        );
        let cat = load_catalog(dir.path(), &[]);
        assert!(cat.problems.is_empty(), "{:?}", cat.problems);
        let p = cat.plugins.get("inbox").unwrap();
        assert_eq!(
            p.manifest.build,
            Some(vec!["cargo".into(), "build".into(), "--release".into()])
        );
        assert_eq!(
            p.manifest.capabilities,
            [Capability::Events, Capability::Intake]
                .into_iter()
                .collect()
        );
        assert_eq!(p.manifest.restart, Restart::Always);
    }

    #[test]
    fn unknown_fields_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "bad",
            "name = \"bad\"\nrun = [\"x\"]\ncapabilities = [\"events\"]\ntypo = true\n",
        );
        let cat = load_catalog(dir.path(), &[]);
        assert!(!cat.plugins.contains_key("bad"));
        assert!(
            cat.problems
                .iter()
                .any(|p| p.blocking && p.what.contains("unknown field")),
            "{:?}",
            cat.problems
        );
    }

    #[test]
    fn empty_run_and_empty_capabilities_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "norun",
            "name = \"norun\"\nrun = []\ncapabilities = [\"events\"]\n",
        );
        write_manifest(
            dir.path(),
            "nocap",
            "name = \"nocap\"\nrun = [\"x\"]\ncapabilities = []\n",
        );
        let cat = load_catalog(dir.path(), &[]);
        assert!(!cat.plugins.contains_key("norun"));
        assert!(!cat.plugins.contains_key("nocap"));
        assert!(
            cat.problems
                .iter()
                .any(|p| p.what.contains("`run` is empty"))
        );
        assert!(
            cat.problems
                .iter()
                .any(|p| p.what.contains("`capabilities` is empty"))
        );
    }

    #[test]
    fn an_unknown_capability_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "weird",
            "name = \"weird\"\nrun = [\"x\"]\ncapabilities = [\"tools\"]\n",
        );
        let cat = load_catalog(dir.path(), &[]);
        assert!(!cat.plugins.contains_key("weird"));
        assert!(cat.problems.iter().any(|p| p.blocking));
    }

    #[test]
    fn the_directory_name_must_match_the_manifest_name() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "on-disk",
            "name = \"other\"\nrun = [\"x\"]\ncapabilities = [\"events\"]\n",
        );
        let cat = load_catalog(dir.path(), &[]);
        assert!(cat.plugins.is_empty());
        assert!(
            cat.problems
                .iter()
                .any(|p| p.what.contains("does not match the directory name")),
            "{:?}",
            cat.problems
        );
    }

    #[test]
    fn an_earlier_root_wins_a_duplicate_name_and_the_shadowed_copy_is_a_warning() {
        let home = tempfile::tempdir().unwrap();
        let extra = tempfile::tempdir().unwrap();
        write_manifest(
            home.path(),
            "notify",
            "name = \"notify\"\ndescription = \"home copy\"\nrun = [\"./a\"]\ncapabilities = [\"events\"]\n",
        );
        write_plugin(
            extra.path(),
            "notify",
            "name = \"notify\"\ndescription = \"extra copy\"\nrun = [\"./b\"]\ncapabilities = [\"events\"]\n",
        );
        let cat = load_catalog(home.path(), &[extra.path().to_path_buf()]);
        let p = cat.plugins.get("notify").unwrap();
        assert_eq!(p.manifest.description, "home copy", "the earlier root wins");
        assert!(
            cat.problems
                .iter()
                .any(|p| !p.blocking && p.what.contains("shadowed")),
            "{:?}",
            cat.problems
        );
    }

    #[test]
    fn a_missing_configured_root_is_a_non_blocking_problem() {
        let home = tempfile::tempdir().unwrap();
        let missing = home.path().join("does-not-exist");
        let cat = load_catalog(home.path(), std::slice::from_ref(&missing));
        assert!(cat.plugins.is_empty());
        assert_eq!(cat.problems.len(), 1);
        assert!(!cat.problems[0].blocking);
        assert!(cat.problems[0].what.contains("does not exist"));
        assert_eq!(cat.problems[0].file, missing.display().to_string());
    }

    #[test]
    fn a_missing_home_plugins_dir_is_not_a_problem() {
        let home = tempfile::tempdir().unwrap();
        let cat = load_catalog(home.path(), &[]);
        assert!(cat.plugins.is_empty());
        assert!(cat.problems.is_empty());
    }

    #[test]
    fn one_broken_plugin_does_not_stop_the_others_loading() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "good",
            "name = \"good\"\nrun = [\"x\"]\ncapabilities = [\"events\"]\n",
        );
        write_manifest(dir.path(), "bad", "not valid toml [[[");
        let cat = load_catalog(dir.path(), &[]);
        assert!(cat.plugins.contains_key("good"));
        assert!(!cat.plugins.contains_key("bad"));
        assert!(cat.problems.iter().any(|p| p.blocking));
    }
}
