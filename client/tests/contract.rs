//! Two loud-failure guarantees this crate makes on top of `docs/CLIENT.md`
//! (see the crate's own top-level doc comment): a run of `forge` is held
//! to a deadline, and a document missing a field `docs/CLIENT.md` marks
//! required fails the parse instead of silently defaulting it.

use forge_client::Forge;
use std::time::Duration;

fn fake_forge(dir: &std::path::Path, script: &str) -> String {
    let bin = dir.join("forge");
    std::fs::write(&bin, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    bin.to_string_lossy().into_owned()
}

#[test]
fn a_run_past_its_deadline_is_killed_and_the_error_names_the_verb() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("ran-to-completion");
    let bin = fake_forge(
        dir.path(),
        &format!("#!/bin/sh\nsleep 5\ntouch {}\n", marker.to_string_lossy()),
    );
    let forge = Forge {
        bin,
        timeout: Duration::from_millis(200),
    };

    let err = forge.run(&["snapshot"]).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("snapshot"),
        "error should name the verb: {msg}"
    );
    assert!(
        msg.contains("timed out"),
        "error should say it timed out: {msg}"
    );

    // The child was actually killed, not merely abandoned: it never
    // reached the `touch` past its `sleep 5`.
    std::thread::sleep(Duration::from_millis(300));
    assert!(!marker.exists(), "the child kept running past the deadline");
}

#[test]
fn a_row_missing_a_required_field_is_an_error_naming_it_and_the_verb() {
    let dir = tempfile::tempdir().unwrap();
    // `events_offset` is required (docs/CLIENT.md's Snapshot document
    // shows it always present, never null); this fake binary omits it.
    let bin = fake_forge(
        dir.path(),
        "#!/bin/sh\necho '{\"tasks\":[],\"requests\":[],\"worker\":{\"running\":false}}'\n",
    );
    let forge = Forge {
        bin,
        ..Forge::new()
    };

    let err = forge.snapshot().unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("events_offset"),
        "error should name the missing field: {msg}"
    );
    assert!(
        msg.contains("snapshot"),
        "error should name the verb: {msg}"
    );
}
