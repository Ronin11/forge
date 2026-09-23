//! `forge-test failing` reads test output on stdin and prints the failing
//! test names, one per line. `forge-test [--] [command...]` runs the declared
//! test check (or the command) and prints a condensed result; the parser and
//! the rendering are the library's.

use std::io::Read;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.as_slice() == ["failing"] {
        let mut out = String::new();
        if std::io::stdin().read_to_string(&mut out).is_err() {
            eprintln!("forge-test: stdin is not text");
            std::process::exit(2);
        }
        for name in forge_test::failing_tests(&out) {
            println!("{name}");
        }
        return;
    }
    let dir = std::env::current_dir().unwrap_or_else(|_| ".".into());
    match forge_test::run::run(&dir, &args) {
        Ok((text, code)) => {
            print!("{text}");
            std::process::exit(code);
        }
        Err(e) => {
            eprintln!("forge-test: {e}");
            std::process::exit(2);
        }
    }
}
