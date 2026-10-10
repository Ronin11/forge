//! Reading `git merge-tree --write-tree` and `git --version`: the pure half
//! of `would_conflict` and of `forge doctor`'s git version row.

/// The oldest git whose `merge-tree` has `--write-tree`, the form
/// `would_conflict` reads.
pub const MERGE_TREE_MIN: (u32, u32) = (2, 38);

/// `merge-tree --write-tree`'s answer: exit 0 is clean; exit 1 with the
/// merged tree's oid on the first line and then one `mode oid stage\tpath`
/// line per conflicted stage (or a bare path, under `--name-only`) up to a
/// blank line is a conflict. Exit 1 with no tree (a bad rev) is no answer.
pub fn parse(code: Option<i32>, stdout: &str) -> Option<Vec<String>> {
    if code != Some(1) {
        return None;
    }
    let mut lines = stdout.lines();
    let tree = lines.next()?.trim();
    if tree.len() < 40 || !tree.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut files: Vec<String> = Vec::new();
    for line in lines.take_while(|l| !l.is_empty()) {
        let path = line.split_once('\t').map_or(line, |(_, p)| p);
        if !files.iter().any(|f| f == path) {
            files.push(path.to_string());
        }
    }
    (!files.is_empty()).then_some(files)
}

/// `(major, minor)` from `git --version`'s "git version 2.43.0" (or a
/// vendor suffix such as "2.39.3 (Apple Git-146)").
pub fn parse_version(out: &str) -> Option<(u32, u32)> {
    let v = out.trim().strip_prefix("git version ")?;
    let mut parts = v.split(|c: char| !c.is_ascii_digit());
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}
