#!/bin/sh
# Reproduces: a sandboxed check escapes restore_metadata by writing
# .git/commondir; host git (hardened) then reads the planted config and runs
# a clean filter as the operator. Run from the repository root.
set -e
trap 'git checkout -- tests/e2e/verdicts.rs' EXIT
cat >> tests/e2e/verdicts.rs <<'RS'

#[test]
fn review868_commondir_escapes_restore_metadata() {
    let e = Env::new();
    let marker = e.home.join("check-commondir-marker");
    let plant = format!(
        "touch $(git ls-files | head -1) && cp -r .git evil && printf '[filter \"x\"]\\n\\tclean = touch {}; cat\\n' >> evil/config && echo \"$PWD/evil\" > .git/commondir && echo '* filter=x' > .gitattributes",
        marker.display()
    );
    let _ = e.run("ok.sh", &["--retries", "0", "--check", &plant]);
    assert!(!marker.exists(), "host Git executed a filter planted via .git/commondir");
}
RS
cargo test --test e2e review868_commondir_escapes_restore_metadata -- --nocapture
