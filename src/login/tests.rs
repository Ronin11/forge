use super::*;
use std::time::Duration;

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(f)
}

/// One shape's fixtures: a login from two raw tokens and an expiry, and
/// tokens shaped as its CLI writes them, tagged so an assertion can
/// still find a distinguishing substring.
struct Fix {
    shape: &'static Shape,
    /// A login holding `access` and `refresh` as they are, expiring at
    /// `at` (ms) where the shape records an expiry.
    raw: fn(access: &str, refresh: &str, at: i64) -> String,
    oat: fn(&str) -> String,
    ort: fn(&str) -> String,
}

impl Fix {
    fn login(&self, access: &str, refresh: &str, at: i64) -> String {
        (self.raw)(access, refresh, at)
    }
    /// A login the CLI could have written, tagged `a` and `r`.
    fn good(&self, a: &str, r: &str, at: i64) -> String {
        self.login(&(self.oat)(a), &(self.ort)(r), at)
    }
}

fn b64(bytes: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = c.iter().fold(0u32, |n, &b| (n << 8) | u32::from(b)) << (8 * (3 - c.len()));
        for i in 0..=c.len() {
            out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

/// A codex access token: a JWT expiring at `at` (ms), tagged.
fn jwt(tag: &str, at: i64) -> String {
    let claims = serde_json::json!({"exp": at / 1000, "tag": tag}).to_string();
    format!(
        "{}.{}.{}",
        b64(br#"{"alg":"RS256"}"#),
        b64(claims.as_bytes()),
        b64(tag.as_bytes())
    )
}

fn claude_raw(access: &str, refresh: &str, at: i64) -> String {
    format!(
        r#"{{"claudeAiOauth":{{"accessToken":"{access}","refreshToken":"{refresh}","expiresAt":{at}}}}}"#
    )
}

/// A codex login. An access token that is a plain tag (letters, digits,
/// `-`, `_`) is made the tag of a JWT expiring at `at`, so the expiry is
/// where the CLI keeps it; anything else goes in as it is.
fn codex_raw(access: &str, refresh: &str, at: i64) -> String {
    let tag = !access.is_empty()
        && access
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    let access = if tag {
        jwt(access, at)
    } else {
        access.to_string()
    };
    serde_json::json!({
        "auth_mode": "chatgpt",
        "OPENAI_API_KEY": null,
        "tokens": {
            "id_token": jwt("id", at),
            "access_token": access,
            "refresh_token": refresh,
            "account_id": "acct-1",
        },
        "last_refresh": "2026-09-25T20:29:36.151046962Z",
    })
    .to_string()
}

fn copilot_raw(access: &str, _refresh: &str, _at: i64) -> String {
    format!(
        "// User settings belong in settings.json.\n// This file is managed automatically.\n{{\"firstLaunchAt\":\"2026-01-01\",\"copilotTokens\":{{\"https://github.com:octo\":\"{access}\"}}}}"
    )
}

static FIXES: [Fix; 3] = [
    Fix {
        shape: &CLAUDE,
        raw: claude_raw,
        oat: |t| format!("sk-ant-oat01-{t:x<32}"),
        ort: |t| format!("sk-ant-ort01-{t:x<32}"),
    },
    Fix {
        shape: &CODEX,
        raw: codex_raw,
        oat: |t| t.to_string(),
        ort: |t| format!("rt.1.{t:x<40}"),
    },
    Fix {
        shape: &COPILOT,
        raw: copilot_raw,
        oat: |t| format!("gho_{t:x<36}"),
        ort: |t| t.to_string(),
    },
];

/// The shapes whose logins rotate, and so may be written back.
fn rotating() -> impl Iterator<Item = &'static Fix> {
    FIXES.iter().filter(|f| f.shape.rotates())
}

const NOW: i64 = 1_800_000_000_000;

#[test]
fn a_login_is_usable_only_with_every_token() {
    for f in &FIXES {
        let p = |t: &str| f.shape.parse(t).usable;
        let cli = f.shape.cli;
        assert!(p(&f.login("a", "r", NOW)), "{cli}");
        assert!(!p(&f.login("", "", 0)), "{cli}");
        assert!(!p(&f.login("", "r", NOW)), "{cli}");
        assert_eq!(p(&f.login("a", "", NOW)), !f.shape.rotates(), "{cli}");
        assert!(!p(""), "{cli}");
        assert!(!p("{}"), "{cli}");
        assert!(!p("not json"), "{cli}");
    }
}

#[test]
fn expiry_is_read_where_each_cli_keeps_it() {
    let c = |f: &Fix, at| f.shape.parse(&f.login("a", "r", at)).expires_at_ms;
    let [claude, codex, copilot] = &FIXES;
    assert_eq!(c(claude, NOW), NOW);
    assert_eq!(c(claude, NOW / 1000), NOW, "seconds are read as such");
    assert_eq!(c(codex, NOW), NOW, "the access JWT's exp");
    assert_eq!(c(copilot, NOW), NEVER, "a GitHub token does not expire");
    for f in &FIXES {
        assert_eq!(f.shape.parse(&f.login("", "", 0)).expires_at_ms, 0);
    }
}

#[test]
fn a_codex_api_key_is_a_login_that_never_expires() {
    let key = r#"{"OPENAI_API_KEY":"sk-proj-abc","tokens":null}"#;
    assert_eq!(
        CODEX.parse(key),
        Creds {
            usable: true,
            expires_at_ms: NEVER
        }
    );
    assert!(!CODEX.parse(key).near_expiry(NOW, REFRESH_WINDOW_MS));
}

#[test]
fn near_expiry_is_within_thirty_minutes() {
    for f in rotating() {
        let c = |ms| f.shape.parse(&f.login("a", "r", ms));
        let w = REFRESH_WINDOW_MS;
        assert!(c(NOW + 29 * 60_000).near_expiry(NOW, w));
        assert!(!c(NOW + 31 * 60_000).near_expiry(NOW, w));
        assert!(c(NOW - 1000).near_expiry(NOW, w));
    }
    let short = refresh_window_ms(Duration::from_secs(60), Duration::ZERO);
    assert_eq!(short, REFRESH_WINDOW_MS);
}

#[test]
fn a_long_timeout_widens_the_refresh_window() {
    // A two-hour attempt must not start on a token with 90 minutes left:
    // it would expire under the attempt and every concurrent one.
    let f = &FIXES[0];
    let c = |ms| f.shape.parse(&f.login("a", "r", ms));
    let w = refresh_window_ms(Duration::from_secs(2 * 3600), Duration::from_secs(600));
    assert_eq!(w, (120 + 10 + 5) * 60_000);
    assert!(c(NOW + 90 * 60_000).near_expiry(NOW, w));
    assert!(!c(NOW + 136 * 60_000).near_expiry(NOW, w));
    assert!(!c(NOW + 90 * 60_000).near_expiry(NOW, REFRESH_WINDOW_MS));
}

#[test]
fn a_later_whole_private_login_is_written_back() {
    for f in rotating() {
        let c = |ms| f.shape.parse(&f.login("a", "r", ms));
        assert!(should_write_back(c(NOW), c(NOW + 1000), NOW));
        assert!(
            !should_write_back(c(NOW), c(NOW), NOW),
            "equal is not later"
        );
        assert!(!should_write_back(c(NOW), c(NOW - 1000), NOW));
        let empty = f.shape.parse(&f.login("", "", NOW + 9000));
        assert!(
            !should_write_back(c(NOW), empty, NOW),
            "an empty copy never wins"
        );
    }
}

#[test]
fn an_unusable_host_file_takes_only_an_unexpired_private_login() {
    for f in rotating() {
        let c = |ms| f.shape.parse(&f.login("a", "r", ms));
        let cleared = f.shape.parse(&f.login("", "", 0));
        assert!(should_write_back(cleared, c(NOW + 60_000), NOW));
        assert!(!should_write_back(cleared, c(NOW - 60_000), NOW));
        assert!(!should_write_back(Creds::NONE, Creds::NONE, NOW));
    }
}

#[test]
fn replace_atomic_swaps_the_file_and_leaves_no_sibling() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join(CLAUDE.file);
    std::fs::write(&dest, "old").unwrap();
    replace_atomic(&dest, b"new").unwrap();
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "new");
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        [CLAUDE.file],
        "the temporary sibling is renamed away"
    );
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
    assert_eq!(mode & 0o077, 0, "the login is private to its owner");
}

#[test]
fn replace_atomic_into_a_missing_directory_fails_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    assert!(replace_atomic(&dir.path().join("gone/x"), b"new").is_err());
}

fn state_of(root: &tempfile::TempDir) -> PathBuf {
    root.path().join("forge-home")
}

/// A host login `text`, seeded into a private copy the way a launch does.
struct Seeded {
    root: tempfile::TempDir,
    dir: PathBuf,
    state: PathBuf,
    worktree: PathBuf,
    private: PathBuf,
}

fn seeded(shape: &Shape, text: &str) -> Seeded {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join(shape.cli);
    let worktree = root.path().join("work/task");
    let private = root
        .path()
        .join("work/task-provider")
        .join(shape.cli)
        .join(shape.file);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::create_dir_all(private.parent().unwrap()).unwrap();
    std::fs::write(dir.join(shape.file), text).unwrap();
    let state = state_of(&root);
    block_on(shape.seed(&dir, &state, &worktree, &private));
    Seeded {
        root,
        dir,
        state,
        worktree,
        private,
    }
}

fn far() -> i64 {
    crate::unix_now() * 1000 + 8 * 3600 * 1000
}

#[test]
fn write_back_replaces_only_with_a_later_login() {
    for f in rotating() {
        let s = f.shape;
        let seed_text = f.login("old-a", "old-r", far());
        let t = seeded(s, &seed_text);
        assert_eq!(std::fs::read_to_string(&t.private).unwrap(), seed_text);
        let host = t.dir.join(s.file);
        std::fs::write(&t.private, f.good("new-a", "new-r", far() + 1000)).unwrap();
        assert!(
            block_on(s.write_back(&t.dir, &t.state, &t.private)).unwrap(),
            "{}",
            s.cli
        );
        assert!(std::fs::read_to_string(&host).unwrap().contains("new-r"));
        assert!(last_write_back(&t.dir).is_some());
        // The same login again is not later: nothing to do.
        assert!(!block_on(s.write_back(&t.dir, &t.state, &t.private)).unwrap());
        // An older one never goes back over a newer.
        std::fs::write(&t.private, f.good("older-a", "older-r", far() - 1000)).unwrap();
        assert!(!block_on(s.write_back(&t.dir, &t.state, &t.private)).unwrap());
        assert!(std::fs::read_to_string(&host).unwrap().contains("new-r"));
    }
}

#[test]
fn an_accepted_rotation_keeps_the_login_it_replaced_once_and_privately() {
    use std::os::unix::fs::PermissionsExt;
    for f in rotating() {
        let s = f.shape;
        let seed_text = f.login("a0", "r0", far());
        let t = seeded(s, &seed_text);
        std::fs::write(&t.private, f.good("a1", "r1", far() + 1000)).unwrap();
        assert!(block_on(s.write_back(&t.dir, &t.state, &t.private)).unwrap());
        let prev = t.dir.join(s.prev());
        assert_eq!(std::fs::read_to_string(&prev).unwrap(), seed_text);
        assert_eq!(
            std::fs::metadata(&prev).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::write(&t.private, f.good("a2", "r2", far() + 2000)).unwrap();
        assert!(block_on(s.write_back(&t.dir, &t.state, &t.private)).unwrap());
        assert!(
            std::fs::read_to_string(&prev).unwrap().contains("r1"),
            "one copy: the latest replaced"
        );
    }
}

/// Whether the private copy `forged` writes is taken as a write-back
/// over a host login seeded with `seed_text`, and the host file after;
/// a rejection leaves it as seeded.
fn offered(shape: &Shape, seed_text: &str, forged: impl FnOnce(&Path)) -> (bool, String) {
    let t = seeded(shape, seed_text);
    std::fs::remove_file(&t.private).unwrap();
    forged(&t.private);
    let took = block_on(shape.write_back(&t.dir, &t.state, &t.private)).unwrap();
    assert_eq!(
        t.dir.join(shape.prev()).exists(),
        took,
        "a backup is made exactly when a login is accepted"
    );
    (
        took,
        std::fs::read_to_string(t.dir.join(shape.file)).unwrap(),
    )
}

#[test]
fn a_private_login_that_did_not_rotate_the_refresh_token_is_rejected() {
    for f in rotating() {
        let seed_text = f.login("a0", &(f.ort)("r0"), far());
        let (took, host) = offered(f.shape, &seed_text, |p| {
            std::fs::write(p, f.login(&(f.oat)("other"), &(f.ort)("r0"), far() + 1000)).unwrap();
        });
        assert!(!took, "{}", f.shape.cli);
        assert_eq!(host, seed_text);
    }
}

#[test]
fn a_private_login_expiring_past_the_clis_token_lifetime_is_rejected() {
    for f in rotating() {
        let seed_text = f.login("a0", "r0", far());
        let life = f.shape.max_lifetime_ms;
        let (took, host) = offered(f.shape, &seed_text, |p| {
            let at = crate::unix_now() * 1000 + life + 60_000;
            std::fs::write(p, f.good("a1", "r1", at)).unwrap();
        });
        assert!(!took, "{}", f.shape.cli);
        assert_eq!(host, seed_text);
        let (took, _) = offered(f.shape, &seed_text, |p| {
            std::fs::write(p, f.good("a1", "r1", far() + 1000)).unwrap();
        });
        assert!(
            took,
            "{}: a login within the lifetime is taken",
            f.shape.cli
        );
    }
}

#[test]
fn an_oversize_private_file_is_rejected() {
    for f in rotating() {
        let seed_text = f.login("a0", "r0", far());
        let (took, host) = offered(f.shape, &seed_text, |p| {
            let pad = " ".repeat(MAX_PRIVATE_BYTES as usize);
            std::fs::write(p, format!("{}{pad}", f.good("a1", "r1", far() + 1000))).unwrap();
        });
        assert!(!took);
        assert_eq!(host, seed_text);
    }
}

#[test]
fn a_symlinked_private_file_is_rejected_even_when_it_holds_a_good_rotation() {
    for f in rotating() {
        let seed_text = f.login("a0", "r0", far());
        let (took, host) = offered(f.shape, &seed_text, |p| {
            let good = p.with_file_name("elsewhere.json");
            std::fs::write(&good, f.good("a1", "r1", far() + 1000)).unwrap();
            std::os::unix::fs::symlink(&good, p).unwrap();
        });
        assert!(!took);
        assert_eq!(host, seed_text);
    }
}

#[test]
fn a_private_login_with_changed_unrotating_fields_is_rejected() {
    for f in rotating() {
        let seed_text = f.login("a0", "r0", far());
        // A field the seed had, changed; and one it did not, added.
        let changed = |text: String, key: &str| {
            let mut v = json(&text).unwrap();
            let o = v.as_object_mut().unwrap();
            let inner = o.values_mut().find(|x| x.is_object()).unwrap();
            inner
                .as_object_mut()
                .unwrap()
                .insert(key.into(), Value::from("widened"));
            v.to_string()
        };
        for key in ["scopes", "account_id", "subscriptionType"] {
            let (took, host) = offered(f.shape, &seed_text, |p| {
                let text = changed(f.good("a1", "r1", far() + 1000), key);
                std::fs::write(p, text).unwrap();
            });
            assert!(!took, "{} {key}", f.shape.cli);
            assert_eq!(host, seed_text);
        }
        let (took, host) = offered(f.shape, &seed_text, |p| {
            std::fs::write(p, f.good("a1", "r1", far() + 1000)).unwrap();
        });
        assert!(took, "the same fields with a rotated token are taken");
        assert!(host.contains("r1"));
    }
}

#[test]
fn a_claude_login_with_changed_scopes_or_subscription_is_rejected() {
    let access = format!("sk-ant-oat01-{:x<32}", "a1");
    let ort = |t: &str| format!("sk-ant-ort01-{t:x<32}");
    let with = |scopes: &str, sub: &str, refresh: &str, at: i64| {
        format!(
            r#"{{"claudeAiOauth":{{"accessToken":"{access}","refreshToken":"{refresh}","expiresAt":{at},"scopes":[{scopes}],"subscriptionType":"{sub}"}}}}"#
        )
    };
    let seed_text = with(r#""user:inference""#, "pro", "r0", far());
    let (took, _) = offered(&CLAUDE, &seed_text, |p| {
        let widened = with(
            r#""user:inference","user:admin""#,
            "pro",
            &ort("r1"),
            far() + 1000,
        );
        std::fs::write(p, widened).unwrap();
    });
    assert!(!took, "changed scopes");
    let (took, _) = offered(&CLAUDE, &seed_text, |p| {
        let text = with(r#""user:inference""#, "max", &ort("r1"), far() + 1000);
        std::fs::write(p, text).unwrap();
    });
    assert!(!took, "changed subscriptionType");
}

/// Token pairs each CLI would never write: (access, refresh), raw.
fn misshapen(f: &Fix) -> Vec<(String, String)> {
    let x = |n| "x".repeat(n);
    let (oat, ort) = (f.oat, f.ort);
    let mut cases = vec![
        ("a 1".into(), ort("r1")),
        (oat("a1"), "r\\n1".into()),
        (oat("a1"), "".into()),
        ("".into(), ort("r1")),
        ("=".into(), "=".into()),
        (oat("a1"), x(40)),
        (oat("a1"), oat("swap")),
    ];
    match f.shape.cli {
        "claude" => cases.extend([
            (x(32), ort("r1")),
            (ort("swap"), ort("r1")),
            (format!("sk-ant-oat01-{}", x(31)), ort("r1")),
            (oat("a1"), format!("sk-ant-ort01-{}", x(31))),
            (format!("sk-ant-oat01-{}=", x(31)), ort("r1")),
            (format!("sk-ant-oat01-{}/{}", x(15), x(16)), ort("r1")),
        ]),
        "codex" => cases.extend([
            // Not a JWT: two parts, four, an empty one, a non-JSON body.
            ("aaaa.bbbb".into(), ort("r1")),
            ("aaaa.bbbb.cccc.dddd".into(), ort("r1")),
            (format!("{}..sig", b64(b"{}")), ort("r1")),
            (
                format!("{}.{}.sig", b64(b"{}"), b64(b"not json")),
                ort("r1"),
            ),
            // A JWT with no expiry, or with padding.
            (format!("{}.{}.sig", b64(b"{}"), b64(b"{}")), ort("r1")),
            (
                format!("{}.{}=.sig", b64(b"{}"), b64(br#"{"exp":1}"#)),
                ort("r1"),
            ),
            // A refresh token without its prefix, version or body.
            (oat("a1"), format!("rt.{}", x(40))),
            (oat("a1"), format!("rt..{}", x(40))),
            (oat("a1"), format!("rt.1.{}", x(31))),
            (oat("a1"), format!("rt.1.{}/{}", x(20), x(20))),
            (oat("a1"), format!("sk-ant-ort01-{}", x(32))),
        ]),
        _ => {}
    }
    cases
}

#[test]
fn a_private_login_with_tokens_the_cli_would_not_write_is_rejected() {
    for f in rotating() {
        let seed_text = f.login("a0", "r0", far());
        for (access, refresh) in misshapen(f) {
            let (took, host) = offered(f.shape, &seed_text, |p| {
                std::fs::write(p, f.login(&access, &refresh, far() + 1000)).unwrap();
            });
            assert!(!took, "{} {access:?} {refresh:?}", f.shape.cli);
            assert_eq!(host, seed_text);
        }
    }
}

#[test]
fn a_codex_login_with_a_forged_refresh_stamp_is_rejected() {
    let f = &FIXES[1];
    let seed_text = f.login("a0", "r0", far());
    for stamp in [Value::from("x".repeat(60_000)), Value::from(1), Value::Null] {
        let (took, _) = offered(&CODEX, &seed_text, |p| {
            let mut v = json(&f.good("a1", "r1", far() + 1000)).unwrap();
            v["last_refresh"] = stamp.clone();
            std::fs::write(p, v.to_string()).unwrap();
        });
        assert!(!took, "{stamp:?}");
    }
}

#[test]
fn a_private_login_with_no_recorded_seed_is_rejected() {
    for f in rotating() {
        let t = seeded(f.shape, &f.login("a0", "r0", far()));
        std::fs::remove_dir_all(&t.state).unwrap();
        std::fs::write(&t.private, f.good("a1", "r1", far() + 1000)).unwrap();
        assert!(!block_on(f.shape.write_back(&t.dir, &t.state, &t.private)).unwrap());
    }
}

#[test]
fn the_seed_record_holds_a_hash_of_the_refresh_token_never_the_token() {
    for f in rotating() {
        let refresh = (f.ort)("r0-secret");
        let t = seeded(f.shape, &f.login("a0-secret", &refresh, far()));
        let mut n = 0;
        for e in std::fs::read_dir(t.state.join(SEEDS)).unwrap().flatten() {
            let text = std::fs::read_to_string(e.path()).unwrap();
            assert!(
                !text.contains(&refresh) && !text.contains("a0-secret"),
                "{text}"
            );
            n += 1;
        }
        assert_eq!(n, 1, "{}", f.shape.cli);
    }
}

#[test]
fn write_back_does_not_resurrect_a_logged_out_host() {
    for f in rotating() {
        let t = seeded(f.shape, &f.login("a0", "r0", far()));
        std::fs::remove_file(t.dir.join(f.shape.file)).unwrap();
        std::fs::write(&t.private, f.good("a1", "r1", far())).unwrap();
        assert!(!block_on(f.shape.write_back(&t.dir, &t.state, &t.private)).unwrap());
        assert!(!t.dir.join(f.shape.file).exists());
    }
}

#[test]
fn seed_copies_a_usable_host_login_and_removes_a_stale_copy_of_an_empty_one() {
    for f in rotating() {
        let s = f.shape;
        let text = f.login("a", "r", far());
        let t = seeded(s, &text);
        assert_eq!(std::fs::read_to_string(&t.private).unwrap(), text);
        assert!(t.state.join(SEEDS).is_dir());
        // The host file is emptied and the private copy is expired: it
        // must neither be restored over the empty file nor left to seed
        // anything.
        std::fs::write(t.dir.join(s.file), f.login("", "", 0)).unwrap();
        std::fs::write(&t.private, f.login("a", "r", 1_000)).unwrap();
        block_on(s.seed(&t.dir, &state_of(&t.root), &t.worktree, &t.private));
        assert!(!t.private.exists(), "{}", s.cli);
        assert_eq!(s.host_state(&t.dir), Host::Empty);
        assert!(
            !Seed::path(&t.state, &t.private).exists(),
            "no record either"
        );
    }
}

#[test]
fn a_login_that_cannot_rotate_is_seeded_without_a_record_and_never_written_back() {
    // copilot's file, with a token or without (the keychain holds it),
    // and codex's API key.
    let with_token = copilot_raw(&format!("gho_{:x<36}", "t"), "", 0);
    let settings_only = "// managed\n{\"firstLaunchAt\":\"2026-01-01\"}";
    let key = r#"{"OPENAI_API_KEY":"sk-proj-abc","tokens":null}"#;
    for (s, text) in [
        (&COPILOT, with_token.as_str()),
        (&COPILOT, settings_only),
        (&CODEX, key),
    ] {
        let t = seeded(s, text);
        assert_eq!(std::fs::read_to_string(&t.private).unwrap(), text);
        assert!(!Seed::path(&t.state, &t.private).exists());
        let later = copilot_raw(&format!("gho_{:x<36}", "planted"), "", 0);
        std::fs::write(&t.private, &later).unwrap();
        assert!(!block_on(s.write_back(&t.dir, &t.state, &t.private)).unwrap());
        block_on(s.seed(&t.dir, &t.state, &t.worktree, &t.private));
        assert_eq!(std::fs::read_to_string(t.dir.join(s.file)).unwrap(), text);
        assert_eq!(std::fs::read_to_string(&t.private).unwrap(), text);
        assert!(!t.dir.join(s.prev()).exists());
    }
}

#[test]
fn seed_never_writes_through_a_private_symlink_to_the_host_login() {
    for f in &FIXES {
        let text = f.login("a", "r", far());
        let t = seeded(f.shape, &text);
        let host = t.dir.join(f.shape.file);
        std::fs::remove_file(&t.private).unwrap();
        std::os::unix::fs::symlink(&host, &t.private).unwrap();
        block_on(f.shape.seed(&t.dir, &t.state, &t.worktree, &t.private));
        assert_eq!(std::fs::read_to_string(&host).unwrap(), text);
        assert!(is_regular_file(&t.private));
        assert_eq!(std::fs::read_to_string(&t.private).unwrap(), text);
    }
}

#[test]
fn seed_never_writes_through_a_private_symlink_to_an_unrelated_file() {
    for f in &FIXES {
        let text = f.login("a", "r", far());
        let t = seeded(f.shape, &text);
        let victim = t.root.path().join("victim");
        std::fs::write(&victim, "precious").unwrap();
        std::fs::remove_file(&t.private).unwrap();
        std::os::unix::fs::symlink(&victim, &t.private).unwrap();
        block_on(f.shape.seed(&t.dir, &t.state, &t.worktree, &t.private));
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious");
        assert!(is_regular_file(&t.private));
        assert_eq!(std::fs::read_to_string(&t.private).unwrap(), text);
    }
}

#[test]
fn seed_forgets_the_records_of_copies_that_are_gone() {
    for f in rotating() {
        let s = f.shape;
        let t = seeded(s, &f.login("a", "r", far()));
        let old = Seed::path(&t.state, &t.private);
        assert!(old.exists());
        std::fs::remove_dir_all(t.private.parent().unwrap().parent().unwrap()).unwrap();
        let next = t
            .root
            .path()
            .join("work/next-provider")
            .join(s.cli)
            .join(s.file);
        std::fs::create_dir_all(next.parent().unwrap()).unwrap();
        block_on(s.seed(&t.dir, &t.state, &t.worktree, &next));
        assert!(!old.exists());
        assert!(Seed::path(&t.state, &next).exists());
    }
}

#[test]
fn seed_first_writes_back_a_later_private_login_from_a_sibling_task() {
    for f in rotating() {
        let s = f.shape;
        let t = seeded(s, &f.login("a", "dead", far()));
        // A sibling task seeded from the same host file, then rotated.
        let other = t
            .root
            .path()
            .join("work/other-provider")
            .join(s.cli)
            .join(s.file);
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        block_on(s.seed(&t.dir, &t.state, &t.worktree, &other));
        std::fs::write(&other, f.good("b", "live", far() + 5000)).unwrap();
        std::fs::remove_file(&t.private).unwrap();
        block_on(s.seed(&t.dir, &t.state, &t.worktree, &t.private));
        let host = std::fs::read_to_string(t.dir.join(s.file)).unwrap();
        assert!(host.contains("live"), "{}", s.cli);
        assert!(
            std::fs::read_to_string(&t.private)
                .unwrap()
                .contains("live")
        );
    }
}

#[test]
fn each_login_keeps_its_own_lock_mark_and_backup_beside_its_file() {
    let t = seeded(&CODEX, &codex_raw("a0", "r0", far()));
    let good = codex_raw("a1", &format!("rt.1.{:x<40}", "r1"), far() + 1000);
    std::fs::write(&t.private, good).unwrap();
    assert!(block_on(CODEX.write_back(&t.dir, &t.state, &t.private)).unwrap());
    for name in [LOCK, MARK, "auth.json.forge-prev"] {
        assert!(t.dir.join(name).exists(), "{name}");
    }
    // A claude copy beside it is not codex's to judge.
    let claude = t
        .root
        .path()
        .join("work/task-provider/claude")
        .join(CLAUDE.file);
    std::fs::create_dir_all(claude.parent().unwrap()).unwrap();
    std::fs::write(&claude, "{}").unwrap();
    assert_eq!(
        CODEX.private_copies(&t.worktree),
        std::slice::from_ref(&t.private)
    );
}

/// docs/REVIEW-4.md #1.10: a launch waiting on the login lock must not
/// hold a worker thread, or with every worker waiting nothing is left to
/// drive the holder (a refresh probe) to release it.
#[test]
fn waiting_on_the_login_lock_leaves_the_only_worker_thread_free() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().to_path_buf();
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    // The holder: another task, on the same one worker, that keeps the
    // lock until it is told to let go.
    let holder = rt.spawn({
        let dir = dir.clone();
        async move {
            let _lock = lock(&dir).await;
            held_tx.send(()).unwrap();
            let _ = release_rx.await;
        }
    });
    held_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let (waited_tx, waited_rx) = std::sync::mpsc::channel();
    rt.spawn({
        let dir = dir.clone();
        async move {
            let _lock = lock(&dir).await;
            waited_tx.send(()).unwrap();
        }
    });
    let (ran_tx, ran_rx) = std::sync::mpsc::channel();
    rt.spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        ran_tx.send(()).unwrap();
    });
    let progressed = ran_rx.recv_timeout(Duration::from_secs(5));
    let waited_early = waited_rx.try_recv().is_ok();
    let _ = release_tx.send(());
    let waited = waited_rx.recv_timeout(Duration::from_secs(5));
    assert!(
        progressed.is_ok(),
        "a task waiting on the lock held the only worker"
    );
    assert!(!waited_early, "the lock was taken while held");
    assert!(waited.is_ok(), "the waiter never took the released lock");
    rt.block_on(holder).unwrap();
}

#[test]
fn a_seed_waits_for_a_refresh_probe_on_the_host() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let t = seeded(&CLAUDE, &claude_raw("a0", "r0", far()));
    rt.block_on(async {
        let probing = probe_lock(&t.dir).await;
        let seed = CLAUDE.seed(&t.dir, &t.state, &t.worktree, &t.private);
        tokio::pin!(seed);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut seed)
                .await
                .is_err(),
            "seeded while a probe was refreshing the host login"
        );
        // The main lock is free throughout: a write-back is not held up.
        drop(lock(&t.dir).await);
        drop(probing);
        tokio::time::timeout(Duration::from_secs(5), seed)
            .await
            .unwrap();
    });
}
