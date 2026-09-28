use super::*;

fn rule(s: &str) -> Rule {
    Rule::parse(s).unwrap()
}

#[test]
fn the_sweep_removes_dead_pids_and_keeps_live_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let live = tmp.path().join(format!("egress-{}", std::process::id()));
    // pid_max is at most 2^22: this pid cannot exist.
    let dead = tmp.path().join("egress-2147483646");
    let other = tmp.path().join("forge-egress-refused.jsonl");
    for d in [&live, &dead] {
        std::fs::create_dir(d).unwrap();
    }
    std::fs::write(&other, "").unwrap();
    assert_eq!(sweep_dead(tmp.path()), 1);
    assert!(live.exists() && !dead.exists() && other.exists());
}

#[test]
fn a_preexisting_world_writable_proxy_dir_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("egress-1");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(create_private(&dir).is_err());
    // Refused, and left as it was.
    let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o777);
    // Nor is a directory that exists but is not private accepted as ours.
    assert!(verify_private(&dir).is_err());
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(verify_private(&dir).is_ok());
    assert!(
        create_private(&dir).is_err(),
        "an existing name is never adopted"
    );
}

#[test]
fn a_fresh_proxy_dir_is_private_and_a_symlink_is_not_ours() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run").join("egress-1");
    create_private(&dir).unwrap();
    assert_eq!(
        std::fs::metadata(&dir).unwrap().permissions().mode() & 0o077,
        0
    );
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&dir, &link).unwrap();
    assert!(verify_private(&link).is_err());
}

#[test]
fn a_host_matches_itself_on_the_web_ports_only() {
    let r = rule("registry.npmjs.org");
    assert_eq!(r.matches("registry.npmjs.org", 443), Some(Matched::Exact));
    assert_eq!(r.matches("Registry.NPMJS.org.", 80), Some(Matched::Exact));
    assert_eq!(r.matches("registry.npmjs.org", 22), None);
    assert_eq!(r.matches("evil-registry.npmjs.org", 443), None);
    assert_eq!(r.matches("registry.npmjs.org.evil.com", 443), None);
}

#[test]
fn a_port_pins_the_port() {
    let r = rule("dev.home:11434");
    assert_eq!(r.matches("dev.home", 11434), Some(Matched::Exact));
    assert_eq!(r.matches("dev.home", 443), None);
}

#[test]
fn a_suffix_matches_below_the_domain_and_not_the_domain() {
    let r = rule("*.crates.io");
    assert_eq!(r.matches("static.crates.io", 443), Some(Matched::Suffix));
    assert_eq!(r.matches("a.b.crates.io", 443), Some(Matched::Suffix));
    assert_eq!(r.matches("crates.io", 443), None);
    assert_eq!(r.matches("evilcrates.io", 443), None);
    assert_eq!(r.matches("static.crates.io.evil.com", 443), None);
}

#[test]
fn what_is_not_a_host_is_refused() {
    for bad in [
        "",
        "*",
        "*.com",
        "*.",
        "https://x.io",
        "x.io/path",
        "user@x.io",
        "x.io:0",
        "x.io:http",
        "x .io",
        "-x.io",
        "a..b",
        "::1",
        "*x.io",
        "x.*.io",
    ] {
        assert!(Rule::parse(bad).is_err(), "{bad:?} should be refused");
    }
}

#[test]
fn display_round_trips() {
    for s in [
        "registry.npmjs.org",
        "*.github.com",
        "dev.home:11434",
        "*.x.io:8443",
        "10.0.0.5",
    ] {
        assert_eq!(rule(s).to_string(), s);
    }
    assert_eq!(rule("  Example.COM ").to_string(), "example.com");
}

/// A proxy on a socket in a temp dir, allowing `rules`.
fn start(rules: &[&str]) -> (tempfile::TempDir, PathBuf, tokio::task::JoinHandle<()>) {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("p.sock");
    let policy = Policy::new(rules.iter().map(|r| rule(r)));
    let task = tokio::spawn(serve(bind(&sock).unwrap(), Arc::new(policy)));
    (dir, sock, task)
}

/// Send `req`, read until the proxy closes, return everything it said.
async fn ask(sock: &Path, req: &str) -> String {
    let mut s = UnixStream::connect(sock).await.unwrap();
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut out))
        .await
        .expect("the proxy answers")
        .ok();
    String::from_utf8_lossy(&out).into_owned()
}

/// A local server that answers one connection with what `reply` makes
/// of the bytes it received first.
async fn upstream(reply: fn(&str) -> String) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (mut c, _) = l.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let n = c.read(&mut buf).await.unwrap_or(0);
                let got = String::from_utf8_lossy(&buf[..n]).into_owned();
                c.write_all(reply(&got).as_bytes()).await.ok();
            });
        }
    });
    port
}

#[tokio::test]
async fn a_proxy_with_a_limit_of_two_refuses_the_third_concurrent_connection_with_a_503() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("p.sock");
    let policy = Arc::new(Policy::new([rule("example.com")]));
    let task = tokio::spawn(serve_limited(bind(&sock).unwrap(), policy, 2));
    // Two connections held open without a request.
    let _a = UnixStream::connect(&sock).await.unwrap();
    let _b = UnixStream::connect(&sock).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let third = ask(&sock, "").await;
    assert!(third.starts_with("HTTP/1.1 503"), "{third}");
    drop(_a);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let again = ask(&sock, "GET http://forge-egress.invalid/ HTTP/1.1\r\n\r\n").await;
    assert!(again.starts_with("HTTP/1.1 200"), "{again}");
    task.abort();
}

#[tokio::test]
async fn a_connect_target_with_a_newline_is_a_400() {
    let (_d, sock, task) = start(&["example.com"]);
    let got = ask(&sock, "CONNECT a\nb:443 HTTP/1.1\r\n\r\n").await;
    assert!(got.starts_with("HTTP/1.1 400"), "{got}");
    assert_eq!(split_authority("a\nb:443", 443), None);
    assert_eq!(split_authority("a\rb", 443), None);
    task.abort();
}

/// A listener whose every accept fails, counting the calls.
struct Failing(Arc<std::sync::atomic::AtomicUsize>);

impl Accept for Failing {
    async fn accept(&self) -> std::io::Result<UnixStream> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Err(std::io::Error::from_raw_os_error(libc::EMFILE))
    }
}

#[tokio::test]
async fn the_accept_loop_backs_off_when_accept_errors() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let task = tokio::spawn(serve_limited(
        Failing(calls.clone()),
        Arc::new(Policy::new([])),
        2,
    ));
    tokio::time::sleep(Duration::from_millis(500)).await;
    task.abort();
    let n = calls.load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        (2..=12).contains(&n),
        "{n} accepts in 500ms at a 50ms backoff"
    );
}

#[test]
fn refusals_are_logged_at_most_a_burst_per_window() {
    let log = RefusalLog::new();
    let admitted = (0..100).filter(|_| log.admit().is_some()).count();
    assert_eq!(admitted, REFUSAL_LOG_BURST as usize);
}

#[tokio::test]
async fn a_host_off_the_list_gets_a_403_that_names_it_for_connect_and_for_http() {
    let (_d, sock, task) = start(&["registry.npmjs.org"]);
    let r = ask(
        &sock,
        "CONNECT evil.example:443 HTTP/1.1\r\nHost: evil.example:443\r\n\r\n",
    )
    .await;
    assert!(r.starts_with("HTTP/1.1 403"), "{r}");
    assert!(r.contains("evil.example:443"), "{r}");
    assert!(
        r.contains("registry.npmjs.org"),
        "the refusal lists what is allowed: {r}"
    );
    let r = ask(
        &sock,
        "GET http://evil.example/x HTTP/1.1\r\nHost: evil.example\r\n\r\n",
    )
    .await;
    assert!(r.starts_with("HTTP/1.1 403"), "{r}");
    // An allowed host on a port the rule does not cover is refused too.
    let r = ask(&sock, "CONNECT registry.npmjs.org:22 HTTP/1.1\r\n\r\n").await;
    assert!(r.starts_with("HTTP/1.1 403"), "{r}");
    task.abort();
}

#[tokio::test]
async fn the_policy_host_answers_with_the_rules_and_needs_no_network() {
    let (_d, sock, task) = start(&["registry.npmjs.org", "*.crates.io"]);
    let r = ask(
        &sock,
        &format!("GET http://{POLICY_HOST}/ HTTP/1.1\r\nHost: {POLICY_HOST}\r\n\r\n"),
    )
    .await;
    assert!(r.starts_with("HTTP/1.1 200"), "{r}");
    assert!(r.contains("forge-egress: ok"), "{r}");
    assert!(r.contains("allow registry.npmjs.org"), "{r}");
    assert!(r.contains("allow *.crates.io"), "{r}");
    task.abort();
}

#[tokio::test]
async fn connect_to_an_allowed_host_tunnels_bytes_both_ways() {
    let port = upstream(|got| format!("echo:{got}")).await;
    let (_d, sock, task) = start(&[&format!("127.0.0.1:{port}")]);
    let mut s = UnixStream::connect(&sock).await.unwrap();
    s.write_all(format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut head = [0u8; 39];
    s.read_exact(&mut head).await.unwrap();
    assert!(String::from_utf8_lossy(&head).starts_with("HTTP/1.1 200 Connection Established"));
    s.write_all(b"hello").await.unwrap();
    let mut out = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut out))
        .await
        .unwrap()
        .ok();
    assert_eq!(String::from_utf8_lossy(&out), "echo:hello");
    task.abort();
}

#[tokio::test]
async fn an_allowed_http_request_is_forwarded_origin_form_and_closed() {
    let port = upstream(|got| {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{got}",
            got.len()
        )
    })
    .await;
    let (_d, sock, task) = start(&[&format!("127.0.0.1:{port}")]);
    let r = ask(
        &sock,
        &format!("GET http://127.0.0.1:{port}/a/b?c=1 HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nProxy-Connection: keep-alive\r\nAccept: */*\r\n\r\n"),
    )
    .await;
    assert!(r.starts_with("HTTP/1.1 200 OK"), "{r}");
    assert!(
        r.contains("GET /a/b?c=1 HTTP/1.1\r\n"),
        "the target is origin-form: {r}"
    );
    assert!(r.contains("Accept: */*"), "{r}");
    assert!(r.contains("Connection: close"), "{r}");
    assert!(!r.contains("Proxy-Connection"), "{r}");
    task.abort();
}

#[tokio::test]
async fn the_relay_pipes_loopback_to_the_proxy_socket() {
    let (dir, sock, task) = start(&["registry.npmjs.org"]);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let ready = dir.path().join("ready");
    let r = ready.clone();
    let relay_task = tokio::spawn(async move { relay_on(listener, &sock, Some(&r), None).await });
    for _ in 0..100 {
        if ready.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(ready.exists(), "the relay signals it is listening");
    let mut s = TcpStream::connect(&addr).await.unwrap();
    s.write_all(format!("GET http://{POLICY_HOST}/ HTTP/1.1\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut out = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut out))
        .await
        .unwrap()
        .ok();
    assert!(String::from_utf8_lossy(&out).contains("allow registry.npmjs.org"));
    relay_task.abort();
    task.abort();
}

#[tokio::test]
async fn the_relay_records_each_refusal_beside_the_attempt() {
    let (dir, sock, task) = start(&["registry.npmjs.org"]);
    let clone = dir.path().join("clone");
    std::fs::create_dir_all(clone.join(".git")).unwrap();
    let record = refused_path(&clone).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let ready = dir.path().join("ready");
    let (r, rec) = (ready.clone(), record.clone());
    let relay_task =
        tokio::spawn(async move { relay_on(listener, &sock, Some(&r), Some(&rec)).await });
    for _ in 0..100 {
        if ready.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let requests = [
        "CONNECT http-intake.logs.example.com:443 HTTP/1.1\r\n\r\n".to_string(),
        "CONNECT http-intake.logs.example.com:443 HTTP/1.1\r\n\r\n".to_string(),
        "GET http://evil.example/x HTTP/1.1\r\nHost: evil.example\r\n\r\n".to_string(),
        // The policy host answers 200: not a refusal.
        format!("GET http://{POLICY_HOST}/ HTTP/1.1\r\n\r\n"),
    ];
    for req in requests {
        let mut s = TcpStream::connect(&addr).await.unwrap();
        s.write_all(req.as_bytes()).await.unwrap();
        let mut out = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut out))
            .await
            .unwrap()
            .ok();
        assert!(!out.is_empty(), "the answer still reaches the client");
    }
    let got: Vec<String> = read_refused(&clone).iter().map(|r| r.to_string()).collect();
    assert_eq!(
        got,
        ["http-intake.logs.example.com:443 x2", "evil.example:80 x1"]
    );
    // Carried across a new `.git`, and forgotten at the next attempt.
    let refused = read_refused(&clone);
    clear_refused(&clone);
    assert!(read_refused(&clone).is_empty());
    restore_refused(&clone, &refused);
    assert_eq!(read_refused(&clone), refused);
    relay_task.abort();
    task.abort();
}

#[test]
fn a_403_counts_as_a_refusal_only_when_the_proxy_names_the_host_asked_for() {
    let ask = b"CONNECT a.example:443 HTTP/1.1\r\n\r\n";
    assert_eq!(requested(ask), Some(("a.example".into(), 443)));
    assert_eq!(
        requested(b"GET http://b.example:8080/x HTTP/1.1\r\n\r\n"),
        Some(("b.example".into(), 8080))
    );
    assert_eq!(requested(b""), None);
    let refused = format!("HTTP/1.1 403 Forbidden\r\n{REFUSED_HEADER}: a.example:443\r\n\r\n");
    assert_eq!(
        refused_in(refused.as_bytes()),
        Some(("a.example".into(), 443))
    );
    // An upstream server's own 403 carries no header.
    assert_eq!(refused_in(b"HTTP/1.1 403 Forbidden\r\n\r\n"), None);
    let ok = format!("HTTP/1.1 200 OK\r\n{REFUSED_HEADER}: a.example:443\r\n\r\n");
    assert_eq!(refused_in(ok.as_bytes()), None);
}

#[test]
fn a_directory_that_is_not_a_clone_records_nothing() {
    let d = tempfile::tempdir().unwrap();
    assert!(refused_path(d.path()).is_none());
    assert!(read_refused(d.path()).is_empty());
}

#[test]
fn only_public_addresses_pass_for_a_suffix_match() {
    for private in [
        "127.0.0.1",
        "10.1.2.3",
        "172.16.0.9",
        "192.168.1.1",
        "169.254.169.254",
        "0.0.0.0",
        "100.64.0.1",
        "::1",
        "fc00::1",
        "fe80::1",
        "::ffff:127.0.0.1",
        "::ffff:10.0.0.1",
    ] {
        assert!(!is_public(private.parse().unwrap()), "{private}");
    }
    for public in ["1.1.1.1", "93.184.216.34", "2606:4700::1111"] {
        assert!(is_public(public.parse().unwrap()), "{public}");
    }
}

#[test]
fn a_url_becomes_the_rule_for_its_host_and_port() {
    let r = |u: &str| rule_for_url(u).map(|r| r.to_string());
    assert_eq!(
        r("https://api.openai.com/v1"),
        Some("api.openai.com".into())
    );
    assert_eq!(r("http://dev.home:11434/v1"), Some("dev.home:11434".into()));
    assert_eq!(r("http://dev.home/v1"), Some("dev.home:80".into()));
    assert_eq!(r("https://u:p@x.io:8443/a?b"), Some("x.io:8443".into()));
    assert_eq!(r("file:///etc"), None);
    assert_eq!(r("not a url"), None);
}

#[test]
fn the_model_endpoint_is_always_in_the_rules() {
    let mut providers = BTreeMap::new();
    providers.insert("anthropic".to_string(), crate::agent::Provider::default());
    let rules: Vec<String> = model_rules(&providers)
        .iter()
        .map(|r| r.to_string())
        .collect();
    assert!(rules.iter().any(|r| r == "*.anthropic.com"), "{rules:?}");
    let p = crate::agent::Provider {
        name: "devhome".into(),
        runner: crate::agent::Runner::CodexCli,
        env: vec![("OLLAMA_HOST".into(), "http://dev.home:11434".into())],
        ..crate::agent::Provider::default()
    };
    providers.insert("devhome".to_string(), p);
    let rules: Vec<String> = model_rules(&providers)
        .iter()
        .map(|r| r.to_string())
        .collect();
    assert!(rules.iter().any(|r| r == "dev.home:11434"), "{rules:?}");
}

#[test]
fn chat_and_jev_providers_open_nothing_of_their_own_even_with_a_base_url() {
    let jev = crate::agent::Provider {
        runner: crate::agent::Runner::Jev,
        base_url: Some(crate::agent::JEV_DEFAULT_URL.into()),
        ..crate::agent::Provider::default()
    };
    assert!(provider_rules(&jev).is_empty());
    let chat = crate::agent::Provider {
        runner: crate::agent::Runner::Chat,
        base_url: Some("http://chat.lan:8080".into()),
        ..crate::agent::Provider::default()
    };
    assert!(provider_rules(&chat).is_empty());
    let mut providers = BTreeMap::new();
    providers.insert("jev".to_string(), jev);
    providers.insert("chat".to_string(), chat);
    assert!(model_rules(&providers).is_empty());
}

#[test]
fn a_providers_own_rules_leave_out_every_other_configured_providers_endpoints() {
    let mut providers = BTreeMap::new();
    providers.insert("anthropic".to_string(), crate::agent::Provider::default());
    providers.insert(
        "codex".to_string(),
        crate::agent::Provider {
            runner: crate::agent::Runner::CodexCli,
            ..crate::agent::Provider::default()
        },
    );
    let claude = providers.get("anthropic").unwrap();
    let rules: Vec<String> = provider_rules(claude)
        .iter()
        .map(|r| r.to_string())
        .collect();
    assert!(rules.iter().any(|r| r == "*.anthropic.com"), "{rules:?}");
    assert!(!rules.iter().any(|r| r == "*.openai.com"), "{rules:?}");
}

#[test]
fn a_policy_is_the_same_whatever_order_its_rules_came_in() {
    let a = Policy::new([rule("b.io"), rule("a.io"), rule("a.io")]);
    let b = Policy::new([rule("a.io"), rule("b.io")]);
    assert_eq!(a, b);
    assert_eq!(a.rules().len(), 2);
}

#[tokio::test]
async fn a_granted_name_that_resolves_to_loopback_is_refused_but_an_operator_written_one_connects()
{
    let port = upstream(|got| format!("echo:{got}")).await;
    let connect = |policy: Policy| async move {
        let d = tempfile::tempdir().unwrap();
        let sock = d.path().join("p.sock");
        let task = tokio::spawn(serve(bind(&sock).unwrap(), Arc::new(policy)));
        let mut s = UnixStream::connect(&sock).await.unwrap();
        s.write_all(format!("CONNECT localhost:{port} HTTP/1.1\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut head = [0u8; 12];
        tokio::time::timeout(Duration::from_secs(5), s.read_exact(&mut head))
            .await
            .unwrap()
            .unwrap();
        let mut rest = Vec::new();
        if head.starts_with(b"HTTP/1.1 502") {
            s.read_to_end(&mut rest).await.unwrap();
        }
        task.abort();
        format!(
            "{}{}",
            String::from_utf8_lossy(&head),
            String::from_utf8_lossy(&rest)
        )
    };
    let target = format!("localhost:{port}");
    let refused = connect(Policy::new([Rule::granted(&target).unwrap()])).await;
    assert!(refused.starts_with("HTTP/1.1 502"), "{refused}");
    assert!(
        refused.contains("127.0.0.1"),
        "names the address: {refused}"
    );
    assert!(refused.contains("not a public address"), "{refused}");
    let allowed = connect(Policy::new([rule(&target)])).await;
    assert!(allowed.starts_with("HTTP/1.1 200"), "{allowed}");
}

#[test]
fn a_granted_rule_matches_as_granted_and_an_operator_rule_outranks_it() {
    let granted = Rule::granted("registry.example.net").unwrap();
    assert_eq!(
        granted.matches("registry.example.net", 443),
        Some(Matched::Granted)
    );
    let both = Policy::new([granted, rule("registry.example.net")]);
    assert_eq!(
        both.allows("registry.example.net", 443),
        Some(Matched::Exact)
    );
}
