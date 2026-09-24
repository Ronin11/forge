use super::*;

/// The token `forge-web` gates every request on: read from
/// `FORGE_HOME/web.token`, or generated the same way `forge-web` itself
/// generates it (32 bytes of OS randomness as hex, file mode 0600) if it
/// is not there yet — so `forge web link` works whether `forge-web` has
/// ever run or not.
pub(crate) fn web_token(dir: &Path) -> Result<String> {
    let path = dir.join("web.token");
    if let Ok(t) = std::fs::read_to_string(&path) {
        let t = t.trim().to_string();
        if t.len() >= 32 {
            return Ok(t);
        }
    }
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut bytes))
        .context("reading /dev/urandom")?;
    let t: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::create_dir_all(dir).ok();
    std::fs::write(&path, &t).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(t)
}

/// `forge web link [--bind ADDR]`: the tokened link for a running or
/// future `forge-web`, without starting a second one to see it.
pub(super) fn web_link(bind: String) -> Result<()> {
    let home = crate::ctx::Paths::resolve()?.home;
    let secret = web_token(&home)?;
    out!("http://{bind}/?token={secret}");
    Ok(())
}

/// `forge web open [--bind ADDR]`: the same link as `forge web link`,
/// handed to `xdg-open`.
pub(super) fn web_open(bind: String) -> Result<()> {
    let home = crate::ctx::Paths::resolve()?.home;
    let secret = web_token(&home)?;
    let link = format!("http://{bind}/?token={secret}");
    std::process::Command::new("xdg-open")
        .arg(&link)
        .status()
        .context("running xdg-open")?;
    out!("{link}");
    Ok(())
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// `forge web serve [--bind ADDR]`: exec `forge-web`, found in the
/// directory of the running `forge` binary, else on PATH. Never embeds
/// any web code; a missing binary is a one-line error naming both places.
pub(super) fn web_serve(bind: Option<String>) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let beside = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("forge-web")))
        .filter(|p| is_executable(p));
    let bin = match beside {
        Some(p) => p,
        None => {
            let path = std::env::var_os("PATH").unwrap_or_default();
            match std::env::split_paths(&path)
                .map(|d| d.join("forge-web"))
                .find(|p| is_executable(p))
            {
                Some(p) => p,
                None => {
                    let dir = std::env::current_exe()
                        .ok()
                        .and_then(|e| e.parent().map(|d| d.display().to_string()))
                        .unwrap_or_else(|| "?".into());
                    bail!("forge-web not found: looked beside forge in {dir} and on PATH");
                }
            }
        }
    };
    let mut cmd = std::process::Command::new(&bin);
    if let Some(b) = bind {
        cmd.arg("--bind").arg(b);
    }
    let err = cmd.exec();
    bail!("running {}: {err}", bin.display())
}
