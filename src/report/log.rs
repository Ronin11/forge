//! Generation-aware event log I/O. Readers and writers share the rotation lock.
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Seek, Write};
use std::path::Path;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    pub generation: u64,
    pub offset: u64,
}

impl std::fmt::Display for Cursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.generation, self.offset)
    }
}

impl std::str::FromStr for Cursor {
    type Err = std::num::ParseIntError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (generation, offset) = s.split_once(':').unwrap_or(("0", s));
        Ok(Self {
            generation: generation.parse()?,
            offset: offset.parse()?,
        })
    }
}

fn lock(path: &Path) -> io::Result<File> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(super::log_lock_path(path))?;
    lock.lock()?;
    let pending = path.with_extension("pending");
    if pending.exists() {
        if path.exists() {
            fs::remove_file(pending)?;
        } else {
            fs::rename(pending, path)?;
        }
    }
    Ok(lock)
}

fn header(path: &Path) -> io::Result<Cursor> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Cursor::default()),
        Err(e) => return Err(e),
    };
    let mut line = String::new();
    BufReader::new(file).read_line(&mut line)?;
    let generation = serde_json::from_str::<serde_json::Value>(&line)
        .ok()
        .and_then(|v| v["generation"].as_u64());
    Ok(generation
        .map(|generation| Cursor {
            generation,
            offset: line.len() as u64,
        })
        .unwrap_or_default())
}

pub fn append(path: &Path, line: &str, limit: u64) -> io::Result<()> {
    let _lock = lock(path)?;
    if fs::metadata(path).is_ok_and(|m| m.len() > limit) {
        let generation = header(path)?
            .generation
            .checked_add(1)
            .ok_or_else(|| io::Error::other("event generation exhausted"))?;
        let pending = path.with_extension("pending");
        let mut file = File::create(&pending)?;
        writeln!(file, "{{\"generation\":{generation}}}")?;
        file.sync_all()?;
        let old = path.with_extension("jsonl.1");
        if old.exists() {
            fs::rename(&old, path.with_extension("jsonl.2"))?;
        }
        fs::rename(path, old)?;
        fs::rename(pending, path)?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(line.as_bytes())
}

pub fn snapshot(path: &Path) -> io::Result<Cursor> {
    let _lock = lock(path)?;
    Ok(Cursor {
        offset: fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        ..header(path)?
    })
}

pub struct Batch {
    /// Start cursor, end cursor, original JSON line.
    pub lines: Vec<(Cursor, Cursor, String)>,
    pub next: Cursor,
    pub resync: bool,
}

pub fn read(path: &Path, from: Cursor, budget: u64) -> io::Result<Batch> {
    let _lock = lock(path)?;
    let current = header(path)?;
    let mut batch = Batch {
        lines: Vec::new(),
        next: from,
        resync: false,
    };
    let old = path.with_extension("jsonl.1");
    if from.generation != current.generation {
        if from.generation.checked_add(1) == Some(current.generation)
            && old.exists()
            && header(&old)?.generation == from.generation
            && fs::metadata(&old)?.len() >= from.offset
        {
            batch.next.offset = batch.next.offset.max(header(&old)?.offset);
            let complete = read_file(&old, &mut batch, budget, true)?;
            if !complete {
                return Ok(batch);
            }
        } else {
            batch.resync = true;
        }
        batch.next = current;
    } else if from.offset > fs::metadata(path).map(|m| m.len()).unwrap_or(0) {
        batch.resync = true;
        batch.next = current;
    }
    if batch.next.offset < current.offset {
        batch.next = current;
    }
    read_file(path, &mut batch, budget, false)?;
    Ok(batch)
}

fn read_file(path: &Path, batch: &mut Batch, budget: u64, archived: bool) -> io::Result<bool> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(e) => return Err(e),
    };
    file.seek(io::SeekFrom::Start(batch.next.offset))?;
    let mut reader = BufReader::new(file);
    let mut used: u64 = batch.lines.iter().map(|(_, _, s)| s.len() as u64 + 1).sum();
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(true);
        }
        if used >= budget {
            return Ok(false);
        }
        if !line.ends_with('\n') {
            batch.resync |= archived;
            return Ok(archived);
        }
        let start = batch.next;
        batch.next.offset += n as u64;
        used += n as u64;
        batch
            .lines
            .push((start, batch.next, line.trim_end().to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_drains_old_tail_and_returns_restartable_cursors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        append(&path, "{\"type\":\"a\"}\n", 1000).unwrap();
        let cursor = snapshot(&path).unwrap();
        append(&path, "{\"type\":\"b\"}\n", 1000).unwrap();
        append(&path, "{\"type\":\"c\"}\n", 0).unwrap();
        let batch = read(&path, cursor, 1000).unwrap();
        assert!(!batch.resync);
        assert_eq!(batch.lines.len(), 2);
        assert_eq!(batch.lines[0].0.generation, 0);
        assert_eq!(batch.lines[1].0.generation, 1);
        assert!(batch.lines[0].2.contains("b"));
        assert!(batch.lines[1].2.contains("c"));
        assert!(read(&path, batch.next, 1000).unwrap().lines.is_empty());
        let limited = read(&path, cursor, 1).unwrap();
        let rest = read(&path, limited.next, 1000).unwrap();
        assert_eq!(limited.lines.len() + rest.lines.len(), 2);
    }

    #[test]
    fn missed_generation_and_missing_tail_require_resync() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        append(&path, "{}\n", 0).unwrap();
        let cursor = snapshot(&path).unwrap();
        append(&path, "{}\n", 0).unwrap();
        fs::remove_file(path.with_extension("jsonl.1")).unwrap();
        assert!(read(&path, cursor, 1000).unwrap().resync);
        append(&path, "{}\n", 0).unwrap();
        let batch = read(&path, cursor, 1000).unwrap();
        assert!(batch.resync);
        assert_eq!(batch.lines.len(), 1);
        assert_eq!(batch.next.generation, 2);
    }

    #[test]
    fn interrupted_rotation_recovers_pending_header_before_append() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        append(&path, "{}\n", 100).unwrap();
        let cursor = snapshot(&path).unwrap();
        fs::write(path.with_extension("pending"), "{\"generation\":1}\n").unwrap();
        fs::rename(&path, path.with_extension("jsonl.1")).unwrap();
        append(&path, "{}\n", 100).unwrap();
        let batch = read(&path, cursor, 1000).unwrap();
        assert!(!batch.resync);
        assert_eq!(batch.lines.len(), 1);
        assert_eq!(batch.next.generation, 1);
    }

    #[test]
    fn archived_headers_are_skipped_and_torn_tails_require_resync() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        fs::write(
            path.with_extension("jsonl.1"),
            "{\"generation\":1}\n{}\n{\"torn\"",
        )
        .unwrap();
        fs::write(&path, "{\"generation\":2}\n{}\n").unwrap();
        let batch = read(&path, "1:0".parse().unwrap(), 1000).unwrap();
        assert!(batch.resync);
        assert_eq!(batch.lines.len(), 2);
        assert!(batch.lines.iter().all(|line| line.2 == "{}"));
        assert_eq!(batch.next.generation, 2);
    }

    #[test]
    fn four_writers_increment_generation_once_per_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for _ in 0..25 {
                        append(&path, "{}\n", 0).unwrap();
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(snapshot(&path).unwrap().generation, 99);
        assert_eq!(
            header(&path.with_extension("jsonl.1")).unwrap().generation,
            98
        );
        assert_eq!(
            header(&path.with_extension("jsonl.2")).unwrap().generation,
            97
        );
        let batch = read(&path, "98:0".parse().unwrap(), 1000).unwrap();
        assert!(!batch.resync);
        assert_eq!(batch.lines.len(), 2);
    }

    #[test]
    fn incomplete_line_does_not_advance_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        fs::write(&path, "{}\n{\"a\"").unwrap();
        let batch = read(&path, Cursor::default(), 1000).unwrap();
        assert_eq!(batch.next.offset, 3);
        assert_eq!(batch.lines.len(), 1);
    }
}
