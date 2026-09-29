//! Two configs. `forge.toml` in the repository declares its checks; Forge
//! runs them and never trusts the agent's word for it, and reads them from
//! the trusted base commit so the branch under test cannot change what it
//! is verified against. `<FORGE_HOME>/config.toml` is the operator's.

mod home;
mod measure;
mod providers;
mod repo;
mod trust;

#[allow(unused_imports)] // HomeConfig is named directly only by secrets.rs's tests
pub use home::{
    Budget, EarlyEnding, HomeConfig, Intake, SandboxPaths, Supervisor, ensure_home_config,
    load_home,
};
pub use measure::{ExploreRole, Measure};
pub use providers::ROLES;
pub use repo::{
    Config, Execution, in_scope, is_protected, load_at, load_working, load_working_checks,
    load_working_egress, load_working_execution,
};
pub use trust::{TrustEgress, TrustPolicies, TrustPolicy};

/// `FORGE_<NAME>`, falling back to `FORGE2_<NAME>` for one release: the
/// repository, the unit and the remote have been "forge" since 2026-09-19,
/// but every operator script, unit file and shell profile that still
/// exports the old name must keep working until they are updated by hand.
/// The one resolver every env-reading site in the kernel and its clients
/// goes through, so the fallback lives in exactly one place; `forge doctor`
/// (`old_env_vars_set`) warns about each old name still set.
pub fn env(name: &str) -> Result<String, std::env::VarError> {
    std::env::var(format!("FORGE_{name}")).or_else(|_| std::env::var(format!("FORGE2_{name}")))
}

/// Every `FORGE2_*` variable currently set, sorted by name: what `forge
/// doctor` names and tells the operator to rename to `FORGE_*` (see `env`).
pub fn old_env_vars_set() -> Vec<String> {
    let mut v: Vec<String> = std::env::vars()
        .filter(|(k, _)| k.starts_with("FORGE2_"))
        .map(|(k, _)| k)
        .collect();
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `env` reads the new name first, and only falls back to the old one
    /// when the new one is unset; `old_env_vars_set` names every `FORGE2_*`
    /// still set. A name unique to this test avoids racing every other test
    /// in the binary over process-wide env state.
    #[test]
    fn env_prefers_the_new_name_and_falls_back_to_the_old_one() {
        // SAFETY: "ENV_RESOLVER_TEST" is set and removed by only this test,
        // so it races with nothing else that reads or writes env vars.
        unsafe {
            std::env::remove_var("FORGE_ENV_RESOLVER_TEST");
            std::env::remove_var("FORGE2_ENV_RESOLVER_TEST");
        }
        assert!(env("ENV_RESOLVER_TEST").is_err());

        unsafe { std::env::set_var("FORGE2_ENV_RESOLVER_TEST", "old") };
        assert_eq!(env("ENV_RESOLVER_TEST").unwrap(), "old");
        assert!(old_env_vars_set().contains(&"FORGE2_ENV_RESOLVER_TEST".to_string()));

        unsafe { std::env::set_var("FORGE_ENV_RESOLVER_TEST", "new") };
        assert_eq!(
            env("ENV_RESOLVER_TEST").unwrap(),
            "new",
            "the new name wins when both are set"
        );

        unsafe {
            std::env::remove_var("FORGE_ENV_RESOLVER_TEST");
            std::env::remove_var("FORGE2_ENV_RESOLVER_TEST");
        }
    }
}
