//! Machine-local credentials. Backend choice is persistent, never guessed by readers.
use anyhow::{Result, bail};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, AeadCore, KeyInit, OsRng},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::fs::{OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
struct Config {
    backend: String,
}
#[derive(Serialize, Deserialize)]
struct Secret {
    value: String,
    set_at: i64,
}
type Values = BTreeMap<String, Secret>;

pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_- .".contains(&b) && b != b' ')
    {
        bail!(
            "secret names must contain only letters, digits, underscores, dots or hyphens (1–128 characters)"
        );
    }
    Ok(())
}
pub fn reference(reference: &str) -> Result<&str> {
    let name = reference
        .strip_prefix("secret:")
        .ok_or_else(|| anyhow::anyhow!("credential must be a secret:NAME reference"))?;
    validate_name(name)?;
    Ok(name)
}

pub struct Store {
    dir: PathBuf,
    backend: String,
    _lock: File,
}
impl Store {
    pub fn open(home: &Path) -> Result<Self> {
        Self::open_inner(home).map_err(|_| anyhow::anyhow!("cannot open secret store"))
    }
    fn open_inner(home: &Path) -> Result<Self> {
        let dir = home.join("secrets");
        fs::create_dir_all(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(dir.join("lock"))?;
        // All readers and writers serialize backend selection and atomic updates.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
            bail!("lock failed");
        }
        let config = dir.join("config.toml");
        let backend = if config.exists() {
            toml::from_str::<Config>(&fs::read_to_string(&config)?)?.backend
        } else {
            // Linux must work before login, even if setup runs in an unlocked desktop.
            let backend = if cfg!(target_os = "macos")
                && keychain(&dir)
                    .and_then(|e| {
                        e.set_password("{}")?;
                        e.get_password()
                    })
                    .is_ok()
            {
                "keychain"
            } else {
                "file"
            };
            crate::login::replace_atomic(
                &config,
                toml::to_string(&Config {
                    backend: backend.into(),
                })?
                .as_bytes(),
            )?;
            backend.into()
        };
        if backend != "file" && backend != "keychain" {
            bail!("unknown secret backend");
        }
        Ok(Self {
            dir,
            backend,
            _lock: lock,
        })
    }
    fn cipher(&self) -> Result<XChaCha20Poly1305> {
        let path = self.dir.join("key");
        if !path.exists() {
            if self.dir.join("values.enc").exists() {
                bail!("missing key");
            }
            crate::login::replace_atomic(&path, &XChaCha20Poly1305::generate_key(&mut OsRng))?;
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        XChaCha20Poly1305::new_from_slice(&fs::read(path)?)
            .map_err(|_| anyhow::anyhow!("invalid key"))
    }
    fn read(&self) -> Result<Values> {
        let bytes = if self.backend == "keychain" {
            match keychain(&self.dir)?.get_password() {
                Ok(s) => s.into_bytes(),
                Err(keyring::Error::NoEntry) => return Ok(BTreeMap::new()),
                Err(_) => bail!("keychain unavailable"),
            }
        } else {
            let path = self.dir.join("values.enc");
            if !path.exists() {
                return Ok(BTreeMap::new());
            }
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
            let bytes = fs::read(path)?;
            if bytes.len() < 24 {
                bail!("invalid ciphertext");
            }
            self.cipher()?
                .decrypt(XNonce::from_slice(&bytes[..24]), &bytes[24..])
                .map_err(|_| anyhow::anyhow!("decryption failed"))?
        };
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid secret store"))
    }
    fn write(&self, values: &Values) -> Result<()> {
        let bytes = serde_json::to_vec(values)?;
        if self.backend == "keychain" {
            keychain(&self.dir)?.set_password(std::str::from_utf8(&bytes)?)?;
        } else {
            let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
            let mut encrypted = nonce.to_vec();
            encrypted.extend(
                self.cipher()?
                    .encrypt(&nonce, bytes.as_slice())
                    .map_err(|_| anyhow::anyhow!("encryption failed"))?,
            );
            crate::login::replace_atomic(&self.dir.join("values.enc"), &encrypted)?;
        }
        Ok(())
    }
    pub fn get(&self, name: &str) -> Result<String> {
        validate_name(name)?;
        self.read()
            .map_err(|_| anyhow::anyhow!("cannot read secret store"))?
            .remove(name)
            .map(|s| s.value)
            .ok_or_else(|| anyhow::anyhow!("secret {name} is not set"))
    }
    pub fn set(&self, name: &str, value: String) -> Result<()> {
        validate_name(name)?;
        if value.is_empty() {
            bail!("secret value is empty");
        }
        (|| -> Result<()> {
            let mut values = self.read()?;
            values.insert(
                name.into(),
                Secret {
                    value,
                    set_at: crate::unix_now(),
                },
            );
            self.write(&values)
        })()
        .map_err(|_| anyhow::anyhow!("cannot save secret store"))
    }
    pub fn list(&self) -> Result<Vec<(String, i64)>> {
        Ok(self
            .read()
            .map_err(|_| anyhow::anyhow!("cannot read secret store"))?
            .into_iter()
            .map(|(n, s)| (n, s.set_at))
            .collect())
    }
    pub fn remove(&self, name: &str) -> Result<()> {
        validate_name(name)?;
        let mut values = self
            .read()
            .map_err(|_| anyhow::anyhow!("cannot read secret store"))?;
        if values.remove(name).is_none() {
            bail!("secret {name} is not set");
        }
        self.write(&values)
            .map_err(|_| anyhow::anyhow!("cannot save secret store"))
    }
}
fn keychain(dir: &Path) -> keyring::Result<keyring::Entry> {
    use sha2::{Digest, Sha256};
    let path = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    keyring::Entry::new(
        "forge-secrets",
        &format!("{:x}", Sha256::digest(path.as_os_str().as_encoded_bytes())),
    )
}

pub fn resolve_at(home: &Path, secret: Option<&str>, env: Option<&str>) -> Result<Option<String>> {
    if let Some(s) = secret {
        return Store::open(home)?.get(reference(s)?).map(Some);
    }
    env.map(|name| std::env::var(name).map_err(|_| anyhow::anyhow!("${name} is not set")))
        .transpose()
}
pub fn resolve(secret: Option<&str>, env: Option<&str>) -> Result<Option<String>> {
    resolve_at(&crate::ctx::Paths::compute_home()?, secret, env)
}
