//! `forge-test failing` reads test output on stdin and prints the failing
//! test names, one per line. The parser is the library's.

use std::io::Read;

fn main() {
    let mut out = String::new();
    if std::io::stdin().read_to_string(&mut out).is_err() {
        eprintln!("forge-test: stdin is not text");
        std::process::exit(2);
    }
    if std::env::args().nth(1).as_deref() != Some("failing") {
        eprintln!("usage: forge-test failing < test-output");
        std::process::exit(2);
    }
    for name in forge_test::failing_tests(&out) {
        println!("{name}");
    }
}
