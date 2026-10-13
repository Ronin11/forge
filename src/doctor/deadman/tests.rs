use super::*;
use crate::ctx::{Forge, Paths};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const NOW: i64 = 1_791_590_400;

fn forge() -> (tempfile::TempDir, Forge) {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths {
        worktrees: dir.path().join("worktrees"),
        logs: dir.path().join("logs"),
        home: dir.path().to_path_buf(),
    };
    std::fs::create_dir_all(&paths.worktrees).unwrap();
    std::fs::create_dir_all(&paths.logs).unwrap();
    let store = Store::open(&dir.path().join("forge.db")).unwrap();
    (dir, Forge::open_with(paths, store).unwrap())
}

/// A listener that answers `status` to each of `n` requests and returns
/// the request-line path of each.
async fn listener(status: u16, n: usize) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook", l.local_addr().unwrap());
    let h = tokio::spawn(async move {
        let mut paths = Vec::new();
        for _ in 0..n {
            let (mut s, _) = l.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let got = s.read(&mut buf).await.unwrap();
            let head = String::from_utf8_lossy(&buf[..got]).to_string();
            let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
            paths.push(path);
            let reply =
                format!("HTTP/1.1 {status} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            s.write_all(reply.as_bytes()).await.unwrap();
        }
        paths
    });
    (url, h)
}

#[test]
fn the_fail_url_is_the_url_with_fail_appended() {
    assert_eq!(
        fail_url("https://hc.example/abc"),
        "https://hc.example/abc/fail"
    );
    assert_eq!(
        fail_url("https://hc.example/abc/"),
        "https://hc.example/abc/fail"
    );
}

#[tokio::test]
async fn a_pass_pings_the_plain_url_and_the_row_says_when() {
    let (_d, mut f) = forge();
    let (url, seen) = listener(200, 1).await;
    f.alerts.deadman_url = Some(url);
    ping(&f).await;
    assert_eq!(seen.await.unwrap(), vec!["/hook".to_string()]);
    let m = f.store.doctor_mark(MARK).unwrap().unwrap();
    assert_eq!(m.status, "ok");
    assert!(m.at >= crate::unix_now() - 60);
}

#[tokio::test]
async fn a_refused_ping_is_remembered_as_failed_and_does_not_panic() {
    let (_d, mut f) = forge();
    let (url, seen) = listener(500, 1).await;
    f.alerts.deadman_url = Some(url);
    ping(&f).await;
    seen.await.unwrap();
    assert_eq!(f.store.doctor_mark(MARK).unwrap().unwrap().status, "failed");
}

#[tokio::test]
async fn nothing_is_pinged_when_the_url_is_unset() {
    let (_d, f) = forge();
    ping(&f).await;
    assert!(f.store.doctor_mark(MARK).unwrap().is_none());
}

#[tokio::test]
async fn send_reaches_a_local_listener_with_the_fail_path() {
    let (url, seen) = listener(200, 1).await;
    send(&fail_url(&url)).await.unwrap();
    assert_eq!(seen.await.unwrap(), vec!["/hook/fail".to_string()]);
}

#[test]
fn the_row_reports_unset_never_stale_and_recent() {
    let (d, f) = forge();
    let home = d.path();
    assert_eq!(row(home, &f.store, NOW).status, Status::Ok);
    std::fs::write(
        home.join("config.toml"),
        "[alerts]\ndeadman_url = \"http://127.0.0.1:1/x\"\n",
    )
    .unwrap();
    assert_eq!(row(home, &f.store, NOW).status, Status::Warn);
    let ok = |at| Mark {
        status: "ok".into(),
        detail: String::new(),
        at,
    };
    f.store.set_doctor_mark(MARK, &ok(NOW - 60)).unwrap();
    let r = row(home, &f.store, NOW);
    assert_eq!(r.status, Status::Ok);
    assert!(r.detail.contains("last pinged"), "{}", r.detail);
    f.store
        .set_doctor_mark(MARK, &ok(NOW - 2 * STALE_SECS))
        .unwrap();
    assert_eq!(row(home, &f.store, NOW).status, Status::Warn);
}
