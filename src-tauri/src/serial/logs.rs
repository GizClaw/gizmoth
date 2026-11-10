//! Bounded per-port log ring. Device bytes are assembled into lines; partial
//! lines are flushed after a short idle period so prompts still appear.
use serde::Serialize;
use std::collections::VecDeque;

const CAPACITY: usize = 5000;
const MAX_LINE: usize = 2000;
const PARTIAL_FLUSH_MS: u64 = 250;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Received from the device.
    Rx,
    /// Sent by GizMoth.
    Tx,
    /// Service events: connection, detection, job phases.
    System,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub seq: u64,
    pub at: u64,
    pub source: Source,
    pub text: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub entries: Vec<Entry>,
    /// Pass back as `after` to continue.
    pub next: u64,
    /// True when entries after the requested cursor were already evicted.
    pub truncated: bool,
    /// Changes on clear even when the sequence cursor has not advanced.
    pub generation: u64,
}

#[derive(Default)]
pub struct Ring {
    entries: VecDeque<Entry>,
    next_seq: u64,
    generation: u64,
    partial: Vec<u8>,
    partial_at: u64,
}

/// Removes ANSI escape sequences and other control bytes except tab.
fn clean(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for n in chars.by_ref() {
                    if ('@'..='~').contains(&n) {
                        break;
                    }
                }
            }
            continue;
        }
        if c == '\t' || !c.is_control() {
            out.push(c);
        }
    }
    out
}

/// Includes invalid bytes whose replacement is already determined, but keeps
/// a trailing incomplete code point until the next read (or the terminal drain).
fn complete_utf8_prefix(bytes: &[u8]) -> usize {
    let mut offset = 0;
    loop {
        match std::str::from_utf8(&bytes[offset..]) {
            Ok(_) => return bytes.len(),
            Err(error) => match error.error_len() {
                Some(invalid) => offset += error.valid_up_to() + invalid,
                None => return offset + error.valid_up_to(),
            },
        }
    }
}

impl Ring {
    pub fn last_seq(&self) -> u64 {
        self.next_seq
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    fn emit(&mut self, source: Source, at: u64, text: String) {
        // Evict as chunks are emitted, including for a single very large input.
        if self.entries.len() == CAPACITY {
            self.entries.pop_front();
        }
        self.next_seq += 1;
        self.entries.push_back(Entry {
            seq: self.next_seq,
            at,
            source,
            text,
        });
    }
    pub fn push(&mut self, source: Source, at: u64, text: impl Into<String>) {
        let text = text.into();
        let mut start = 0;
        let mut count = 0;
        for (index, _) in text.char_indices() {
            if count == MAX_LINE {
                self.emit(source, at, text[start..index].to_owned());
                start = index;
                count = 0;
            }
            count += 1;
        }
        // Also emits one empty entry for empty input, but no extra entry when
        // nonempty input ends exactly at a chunk boundary.
        self.emit(source, at, text[start..].to_owned());
    }
    fn drain_partial(&mut self, now: u64, terminal: bool, skip_blank: bool) -> bool {
        let end = if terminal {
            self.partial.len()
        } else {
            complete_utf8_prefix(&self.partial)
        };
        if end == 0 {
            return false;
        }
        let bytes = std::mem::take(&mut self.partial);
        self.partial.extend_from_slice(&bytes[end..]);
        let line = clean(&bytes[..end]);
        if skip_blank && line.trim().is_empty() {
            return false;
        }
        self.push(Source::Rx, now, line);
        true
    }
    /// Appends received bytes; returns true when at least one line completed.
    pub fn push_rx(&mut self, bytes: &[u8], at: u64) -> bool {
        let mut emitted = false;
        for &byte in bytes {
            if byte == b'\n' {
                let line = std::mem::take(&mut self.partial);
                let line = line.strip_suffix(b"\r").unwrap_or(&line);
                self.push(Source::Rx, at, clean(line));
                emitted = true;
            } else {
                self.partial_at = at;
                self.partial.push(byte);
                if self.partial.len() >= MAX_LINE {
                    emitted |= self.drain_partial(at, false, false);
                }
            }
        }
        emitted
    }
    pub fn flush_stale(&mut self, now: u64) -> bool {
        if self.partial.is_empty() || now.saturating_sub(self.partial_at) < PARTIAL_FLUSH_MS {
            return false;
        }
        self.drain_partial(now, false, true)
    }
    /// Unconditionally finishes a connection's remaining bytes. An incomplete
    /// terminal code point is replaced once, rather than carried to a new peer.
    pub fn flush_partial(&mut self, now: u64) -> bool {
        self.drain_partial(now, true, false)
    }
    pub fn page(&self, after: u64, limit: usize) -> Page {
        let first = self
            .entries
            .front()
            .map(|e| e.seq)
            .unwrap_or(self.next_seq.saturating_add(1));
        let entries: Vec<Entry> = self
            .entries
            .iter()
            .filter(|e| e.seq > after)
            .take(limit)
            .cloned()
            .collect();
        Page {
            next: entries
                .last()
                .map(|e| e.seq)
                .unwrap_or(after.max(first.saturating_sub(1))),
            truncated: after.saturating_add(1) < first && after != 0,
            generation: self.generation,
            entries,
        }
    }
    /// Newest `limit` entries, for initial display.
    pub fn tail(&self, limit: usize) -> Page {
        let skip = self.entries.len().saturating_sub(limit);
        let entries: Vec<Entry> = self.entries.iter().skip(skip).cloned().collect();
        Page {
            next: entries.last().map(|e| e.seq).unwrap_or(self.next_seq),
            truncated: skip > 0,
            generation: self.generation,
            entries,
        }
    }
    pub fn clear(&mut self) {
        self.entries.clear();
        self.partial.clear();
        self.generation = self.generation.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assembles_lines_strips_ansi_and_pages() {
        let mut ring = Ring::default();
        assert!(!ring.push_rx(b"\x1b[0;32mI (12) boot", 1));
        assert!(ring.push_rx(b": ok\r\nnext", 2));
        assert!(!ring.flush_stale(100));
        assert!(ring.flush_stale(400));
        let page = ring.page(0, 10);
        let texts: Vec<_> = page.entries.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, ["I (12) boot: ok", "next"]);
        assert_eq!(ring.page(page.next, 10).entries.len(), 0);
        assert_eq!(ring.page(1, 10).entries.len(), 1);
    }

    #[test]
    fn partial_flush_waits_for_idle_after_latest_byte() {
        let mut ring = Ring::default();
        assert!(!ring.push_rx(b"\x1b[", 0));
        assert!(!ring.push_rx(b"32mhello", 240));
        assert!(!ring.flush_stale(250));
        assert!(!ring.flush_stale(489));
        assert!(ring.flush_stale(490));
        assert_eq!(ring.tail(1).entries[0].text, "hello");
    }

    #[test]
    fn chunks_unicode_empty_and_exact_boundaries() {
        let mut ring = Ring::default();
        ring.push(Source::System, 10, "");
        ring.push(Source::Tx, 11, "中".repeat(MAX_LINE));
        ring.push(Source::Tx, 12, format!("{}💡", "é".repeat(MAX_LINE)));
        let page = ring.page(0, 10);
        assert_eq!(page.entries.len(), 4);
        assert_eq!(page.entries[0].text, "");
        assert_eq!(page.entries[1].text, "中".repeat(MAX_LINE));
        assert_eq!(page.entries[2].text, "é".repeat(MAX_LINE));
        assert_eq!(page.entries[3].text, "💡");
        assert_eq!(page.entries[3].at, 12);
        assert_eq!(page.entries[3].source, Source::Tx);
    }

    #[test]
    fn one_large_push_retains_only_bounded_tail() {
        let mut ring = Ring::default();
        let chunks = CAPACITY + 3;
        ring.push(Source::Tx, 1, "a".repeat(MAX_LINE * chunks));
        assert_eq!(ring.entries.len(), CAPACITY);
        assert_eq!(ring.last_seq(), chunks as u64);
        assert_eq!(ring.entries.front().unwrap().seq, 4);
        assert!(ring.entries.iter().all(|e| e.text.len() == MAX_LINE));
    }

    #[test]
    fn byte_limit_does_not_split_utf8() {
        for padding in (MAX_LINE - 3)..MAX_LINE {
            let mut ring = Ring::default();
            let text = format!("{}中💡", "a".repeat(padding));
            assert!(ring.push_rx(text.as_bytes(), 1));
            ring.push_rx(b"\n", 2);
            let output: String = ring
                .page(0, 10)
                .entries
                .iter()
                .map(|e| e.text.as_str())
                .collect();
            assert_eq!(output, text);
            assert!(ring.partial.is_empty());
        }
    }

    #[test]
    fn stale_flush_retains_incomplete_codepoints_and_replaces_invalid_bytes() {
        let mut ring = Ring::default();
        assert!(!ring.push_rx(b"prompt\xe4\xb8", 1));
        assert!(ring.flush_stale(300));
        assert_eq!(ring.tail(1).entries[0].text, "prompt");
        assert_eq!(ring.partial, b"\xe4\xb8");
        assert!(!ring.flush_stale(600));
        assert!(!ring.push_rx(b"\xad\xff\xf0\x9f", 601));
        assert!(ring.flush_stale(900));
        assert_eq!(ring.tail(1).entries[0].text, "中�");
        assert_eq!(ring.partial, b"\xf0\x9f");
        assert!(ring.push_rx(b"\x92\xa1\n", 901));
        assert_eq!(ring.tail(1).entries[0].text, "💡");
    }

    #[test]
    fn terminal_flush_drains_once_without_waiting() {
        let mut ring = Ring::default();
        ring.push_rx(b"prompt \xe4\xb8", 10);
        assert!(ring.flush_partial(11));
        assert_eq!(ring.tail(1).entries[0].text, "prompt �");
        assert!(!ring.flush_partial(12));
        assert!(!ring.flush_stale(500));
        ring.push_rx(b"new peer\n", 501);
        assert_eq!(ring.tail(1).entries[0].text, "new peer");
    }

    #[test]
    fn clear_changes_generation_without_reusing_sequences() {
        let mut ring = Ring::default();
        ring.push(Source::System, 1, "old");
        ring.push_rx(b"partial", 2);
        let before = ring.page(0, 10);
        ring.clear();
        assert_eq!(ring.last_seq(), before.next);
        assert_eq!(ring.generation(), before.generation + 1);
        assert_eq!(ring.tail(10).generation, ring.generation());
        assert!(ring.page(before.next, 10).entries.is_empty());
        assert!(!ring.flush_partial(3));
        ring.push(Source::System, 4, "new");
        let after = ring.page(before.next, 10);
        assert_eq!(after.entries[0].seq, before.next + 1);
        assert_eq!(after.generation, before.generation + 1);
        assert_eq!(serde_json::to_value(after).unwrap()["generation"], 1);
        assert!(ring.page(u64::MAX, 10).entries.is_empty());
    }

    #[test]
    fn evicts_oldest_and_reports_truncation() {
        let mut ring = Ring::default();
        for i in 0..(CAPACITY + 10) {
            ring.push(Source::System, 0, format!("{i}"));
        }
        let page = ring.page(1, 5);
        assert!(page.truncated);
        assert_eq!(page.entries[0].seq, 11);
        assert_eq!(ring.tail(3).entries.len(), 3);
    }
}
