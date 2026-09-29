//! Bounds on data produced by an untrusted agent; keep draining after truncation.
use std::fs::File;
use std::io::{self, Write};
use std::path::Path;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt};

pub(super) const LINE_BYTES: usize = 4 << 20;
const STDERR_BYTES: u64 = 1 << 20;

pub(super) struct BoundedLines<R> {
    reader: R,
}

impl<R: AsyncBufRead + Unpin> BoundedLines<R> {
    pub(super) fn new(reader: R) -> Self {
        Self { reader }
    }

    async fn next(&mut self) -> io::Result<Option<(String, bool)>> {
        let mut bytes = Vec::new();
        let mut cut = false;
        loop {
            let buf = self.reader.fill_buf().await?;
            if buf.is_empty() {
                if bytes.is_empty() && !cut {
                    return Ok(None);
                }
                break;
            }
            let newline = buf.iter().position(|b| *b == b'\n');
            let n = newline.unwrap_or(buf.len());
            let keep = n.min(LINE_BYTES - bytes.len());
            bytes.extend_from_slice(&buf[..keep]);
            cut |= keep < n;
            self.reader.consume(n + usize::from(newline.is_some()));
            if newline.is_some() {
                break;
            }
        }
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
        Ok(Some((String::from_utf8_lossy(&bytes).into_owned(), cut)))
    }

    pub(super) async fn next_uncut(&mut self, log: &mut CappedLog) -> io::Result<Option<String>> {
        while let Some((text, cut)) = self.next().await? {
            if !cut {
                return Ok(Some(text));
            }
            log.line(
                &text,
                Some("{\"type\":\"forge_line_truncated\",\"limit_bytes\":4194304}"),
            )?;
        }
        Ok(None)
    }
}

pub(super) async fn read_stderr(mut reader: impl AsyncRead + Unpin) -> String {
    let mut bytes = Vec::new();
    let _ = (&mut reader)
        .take(STDERR_BYTES)
        .read_to_end(&mut bytes)
        .await;
    // Draining also prevents a verbose child's stderr pipe from blocking it.
    let _ = tokio::io::copy(&mut reader, &mut tokio::io::sink()).await;
    String::from_utf8_lossy(&bytes).into_owned()
}

pub(super) struct CappedLog {
    file: File,
    cap: u64,
    written: u64,
    truncated: bool,
    at_line_start: bool,
}

impl CappedLog {
    pub(super) fn new(file: File, cap: u64) -> Self {
        Self {
            file,
            cap,
            written: 0,
            truncated: false,
            at_line_start: true,
        }
    }

    pub(super) fn create(path: &Path) -> anyhow::Result<Self> {
        let home = crate::ctx::Paths::compute_home()?;
        let cap = crate::config::load_home(&home)?.limits.log_bytes;
        Ok(Self::new(File::create(path)?, cap))
    }

    fn truncate(&mut self) -> io::Result<()> {
        let prefix = if self.at_line_start { "" } else { "\n" };
        let frame = format!(
            "{prefix}{{\"type\":\"forge_log_truncated\",\"limit_bytes\":{}}}\n",
            self.cap
        );
        self.file.write_all(frame.as_bytes())?;
        self.truncated = true;
        Ok(())
    }

    pub(super) fn line(&mut self, text: &str, cut: Option<&str>) -> io::Result<()> {
        if self.truncated {
            return Ok(());
        }
        let size = text.len() as u64 + 1 + cut.map_or(0, |marker| marker.len() as u64 + 1);
        if size > self.cap.saturating_sub(self.written) {
            return self.truncate();
        }
        let mut bytes = Vec::with_capacity(size as usize);
        bytes.extend_from_slice(text.as_bytes());
        bytes.push(b'\n');
        if let Some(marker) = cut {
            bytes.extend_from_slice(marker.as_bytes());
            bytes.push(b'\n');
        }
        self.write_all(&bytes)
    }
}

impl Write for CappedLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.truncated {
            if buf.len() as u64 > self.cap.saturating_sub(self.written) {
                self.truncate()?;
            } else {
                self.file.write_all(buf)?;
                self.written += buf.len() as u64;
                if let Some(last) = buf.last() {
                    self.at_line_start = *last == b'\n';
                }
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Seek};

    fn contents(log: &mut CappedLog) -> String {
        log.file.rewind().unwrap();
        let mut text = String::new();
        log.file.read_to_string(&mut text).unwrap();
        text
    }

    #[test]
    fn oversized_line_cannot_cross_log_cap() {
        let mut log = CappedLog::new(tempfile::tempfile().unwrap(), 1 << 20);
        log.line(&"x".repeat(LINE_BYTES), None).unwrap();
        let text = contents(&mut log);
        let frame = "{\"type\":\"forge_log_truncated\",\"limit_bytes\":1048576}";
        assert!(text.len() <= (1 << 20) + frame.len() + 1);
        assert_eq!(text.lines().last(), Some(frame));
        assert_eq!(text, format!("{frame}\n"));
    }

    #[test]
    fn preserves_fitting_lines_and_drops_crossing_line_and_later_writes() {
        let mut log = CappedLog::new(tempfile::tempfile().unwrap(), 10);
        log.line("one", None).unwrap();
        log.line("two", None).unwrap();
        log.line("three", None).unwrap();
        log.line("ignored", None).unwrap();
        assert_eq!(
            contents(&mut log),
            "one\ntwo\n{\"type\":\"forge_log_truncated\",\"limit_bytes\":10}\n"
        );
    }

    #[test]
    fn marker_counts_toward_cap_and_partial_writes_get_a_newline() {
        let mut log = CappedLog::new(tempfile::tempfile().unwrap(), 10);
        log.write_all(b"partial").unwrap();
        log.line("a", Some("marker")).unwrap();
        assert_eq!(
            contents(&mut log),
            "partial\n{\"type\":\"forge_log_truncated\",\"limit_bytes\":10}\n"
        );
        let mut log = CappedLog::new(tempfile::tempfile().unwrap(), 3);
        log.write_all(b"1234").unwrap();
        assert_eq!(
            contents(&mut log),
            "{\"type\":\"forge_log_truncated\",\"limit_bytes\":3}\n"
        );
    }

    #[tokio::test]
    async fn hostile_agent_with_100_mib_line_finishes_with_bounded_log() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = CappedLog::new(tempfile::tempfile().unwrap(), 64 << 20);
        let report = crate::report::Reporter::new(false, None);
        let argv = vec!["/bin/sh".into(), "-c".into(), "head -c 104857600 /dev/zero | tr '\\000' x; printf '\\n{\"type\":\"system\",\"session_id\":\"after\"}\\n'".into()];
        let (out, _) = super::super::run_once(super::super::AgentRun {
            sandbox: None,
            worktree: dir.path(),
            argv: &argv,
            identity: &[],
            prompt: "",
            bin: "sh",
            timeout: std::time::Duration::from_secs(20),
            writes: false,
            early_ending: super::super::tests::thresholds(0, 0, 0, 0),
            task_id: 1,
            report: &report,
            log: &mut log,
        })
        .await
        .unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert_eq!(out.session_id.as_deref(), Some("after"));
        assert!(log.file.metadata().unwrap().len() < LINE_BYTES as u64 + 1024);
        assert!(contents(&mut log).contains("forge_line_truncated"));
    }

    #[tokio::test]
    async fn stderr_is_bounded_and_drained() {
        let input = tokio::io::repeat(b'x').take(2 * STDERR_BYTES);
        assert_eq!(read_stderr(input).await.len(), STDERR_BYTES as usize);
    }
}
