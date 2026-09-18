//! Text rendering: turning an operator's raw task/check text into the
//! short, customer-safe lines `view.rs` assembles into documents.

use crate::store::Task;

/// A landed task's own "Done" line: its title, given the day it was
/// filed in the customer's own words (see docs/PORTAL.md), else a line
/// derived from its request text.
pub(crate) fn landed_task_line(t: &Task) -> String {
    if let Some(title) = t.title.as_deref() {
        let title = title.trim();
        if !title.is_empty() {
            return title.to_string();
        }
    }
    derive_landed_line(&t.task)
}

/// A title-less landed task's "Done" line: its first sentence, any
/// path-like token stripped, cut at 120 characters on a word boundary —
/// never the operator's full request (see docs/PORTAL.md).
fn derive_landed_line(task: &str) -> String {
    let sentence = first_sentence(task);
    let stripped = strip_path_like_tokens(&sentence);
    truncate_at_word_boundary(stripped.trim(), 120)
}

/// The first sentence of `text`, whitespace (including newlines)
/// collapsed to single spaces: up to and including the first `.`, `!` or
/// `?` that is followed by whitespace or the end of the text; the whole
/// (flattened) text when none is found.
fn first_sentence(text: &str) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    for (i, c) in flat.char_indices() {
        if matches!(c, '.' | '!' | '?') {
            let after = &flat[i + c.len_utf8()..];
            if after.chars().next().is_none_or(char::is_whitespace) {
                return flat[..i + c.len_utf8()].to_string();
            }
        }
    }
    flat
}

/// Whether `word` has the shape of a path-like token: a slash-separated
/// path, a file extension (`store.rs`, `PORTAL.md`), or a line-number
/// reference (`L154`, `154:10`) — the shapes of the operator's own file
/// tree that a customer's line must never carry (see docs/PORTAL.md).
pub(crate) fn is_path_like_word(word: &str) -> bool {
    let trimmed = word.trim_matches(|c: char| c.is_ascii_punctuation() && c != '/' && c != '.');
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.contains('/') {
        return true;
    }
    if let Some(rest) = trimmed.strip_prefix(['L', 'l'])
        && !rest.is_empty()
        && rest.chars().all(|c| c.is_ascii_digit())
    {
        return true;
    }
    if let Some(dot) = trimmed.rfind('.') {
        let (base, ext) = (&trimmed[..dot], &trimmed[dot + 1..]);
        if !base.is_empty()
            && (1..=5).contains(&ext.len())
            && ext.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return true;
        }
    }
    if let Some((a, b)) = trimmed.split_once(':')
        && !a.is_empty()
        && !b.is_empty()
        && b.chars().all(|c| c.is_ascii_digit())
    {
        return true;
    }
    false
}

/// Strips every path-like token from `text` (see `is_path_like_word`),
/// including a `line 42`/`Line 42` style reference (a "line" word
/// immediately followed by a bare number).
pub(crate) fn strip_path_like_tokens(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut out: Vec<&str> = Vec::with_capacity(words.len());
    let mut i = 0;
    while i < words.len() {
        let bare = words[i].trim_matches(|c: char| !c.is_alphanumeric());
        if bare.eq_ignore_ascii_case("line")
            && let Some(next) = words.get(i + 1)
        {
            let digits = next.trim_matches(|c: char| !c.is_ascii_digit());
            if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                i += 2;
                continue;
            }
        }
        if is_path_like_word(words[i]) {
            i += 1;
            continue;
        }
        out.push(words[i]);
        i += 1;
    }
    out.join(" ")
}

/// Cuts `s` to at most `max` characters, breaking on the last word
/// boundary at or before the limit rather than mid-word; a single word
/// longer than `max` is hard-cut.
pub(crate) fn truncate_at_word_boundary(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out = String::new();
    for word in s.split(' ') {
        let candidate = if out.is_empty() {
            word.to_string()
        } else {
            format!("{out} {word}")
        };
        if candidate.chars().count() > max {
            break;
        }
        out = candidate;
    }
    if out.is_empty() {
        out = s.chars().take(max).collect();
    }
    out
}
