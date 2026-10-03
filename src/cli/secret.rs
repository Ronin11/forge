use anyhow::{Result, bail};
use clap::Subcommand;
use std::{
    io::{IsTerminal, Read, Write},
    process::{Command, Stdio},
};

#[derive(Subcommand)]
pub enum SecretCmd {
    /// Save a credential from a hidden prompt or piped stdin.
    Set {
        name: String,
        #[arg(long)]
        from_clipboard: bool,
    },
    /// Show names and set times, never values.
    List,
    /// Delete a credential.
    Rm { name: String },
}

fn clipboard() -> Result<String> {
    let candidates: &[(&str, &[&str], &str, &[&str])] = if cfg!(target_os = "macos") {
        &[("pbpaste", &[], "pbcopy", &[])]
    } else {
        &[
            ("wl-paste", &["--no-newline"], "wl-copy", &["--clear"]),
            (
                "xclip",
                &["-selection", "clipboard", "-o"],
                "xclip",
                &["-selection", "clipboard", "-i"],
            ),
        ]
    };
    for (reader, args, clearer, clear_args) in candidates {
        let output = match Command::new(reader)
            .args(*args)
            .stderr(Stdio::null())
            .output()
        {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => bail!("cannot read clipboard"),
            Ok(o) => o,
        };
        // Clear even if reading or UTF-8 decoding failed. No clipboard data enters argv.
        let cleared = Command::new(clearer)
            .args(*clear_args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if !cleared.is_ok_and(|s| s.success()) {
            bail!("cannot clear clipboard");
        }
        if !output.status.success() {
            continue;
        }
        return String::from_utf8(output.stdout)
            .map_err(|_| anyhow::anyhow!("clipboard is not UTF-8"));
    }
    bail!("no working clipboard tool; install wl-clipboard or xclip on Linux")
}

pub fn run(cmd: SecretCmd) -> Result<()> {
    let home = crate::ctx::Paths::compute_home()?;
    match cmd {
        SecretCmd::Set {
            name,
            from_clipboard,
        } => {
            crate::secret_store::validate_name(&name)?;
            let mut value = if from_clipboard {
                clipboard()?
            } else if std::io::stdin().is_terminal() {
                rpassword::prompt_password("Secret value: ")
                    .map_err(|_| anyhow::anyhow!("cannot read hidden secret prompt"))?
            } else {
                let mut value = String::new();
                std::io::stdin()
                    .read_to_string(&mut value)
                    .map_err(|_| anyhow::anyhow!("cannot read secret from stdin"))?;
                value
            };
            // Strip a single terminal/pipeline line ending, preserving other whitespace.
            if value.ends_with('\n') {
                value.pop();
                if value.ends_with('\r') {
                    value.pop();
                }
            }
            crate::secret_store::Store::open(&home)?.set(&name, value)?;
        }
        SecretCmd::List => {
            let mut out = std::io::stdout().lock();
            for (name, time) in crate::secret_store::Store::open(&home)?.list()? {
                writeln!(out, "{name}\t{time} (Unix seconds)")?;
            }
        }
        SecretCmd::Rm { name } => crate::secret_store::Store::open(&home)?.remove(&name)?,
    }
    Ok(())
}
