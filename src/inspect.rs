//! Data model and pure logic for `lane inspect` — the live request-inspector
//! TUI (ngrok-web-UI pattern, in the terminal). The interactive shell lives in
//! `src/cli/inspect.rs`; this module holds the testable parts: parsing the
//! daemon's tab-separated access-log lines into [`Entry`]s and the selection
//! [`State`] the TUI drives.
//!
//! The access log is the proxy daemon's per-request record (written by
//! [`crate::log`]); `lane inspect` tails it. Two on-disk shapes (see
//! [`crate::log::request`]):
//! - full:    `ts \t domain \t method \t path \t upstream \t status \t dur`
//! - minimal: `ts \t domain \t status \t dur`

/// One inspected request, parsed from an access-log line. In `minimal` log mode
/// the per-request fields (`method`/`path`/`upstream`) are empty.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Entry {
    pub ts: String,
    pub domain: String,
    pub method: String,
    pub path: String,
    pub upstream: String,
    pub status: String,
    pub duration: String,
}

impl Entry {
    /// Parse one tab-separated access-log line. Returns `None` for a line that
    /// matches neither the 4-column (minimal) nor 7-column (full) shape —
    /// mirroring `cli::logs::format_log_line_json`.
    pub fn parse(line: &str) -> Option<Entry> {
        let parts: Vec<&str> = line.split('\t').collect();
        match parts.as_slice() {
            [ts, domain, status, duration] => Some(Entry {
                ts: (*ts).to_string(),
                domain: (*domain).to_string(),
                status: (*status).to_string(),
                duration: (*duration).to_string(),
                ..Default::default()
            }),
            [ts, domain, method, path, upstream, status, duration] => Some(Entry {
                ts: (*ts).to_string(),
                domain: (*domain).to_string(),
                method: (*method).to_string(),
                path: (*path).to_string(),
                upstream: (*upstream).to_string(),
                status: (*status).to_string(),
                duration: (*duration).to_string(),
            }),
            _ => None,
        }
    }
}

/// The inspector's UI state: the parsed request list plus the selected index.
/// New requests append; the selection is clamped to the list.
#[derive(Debug, Default)]
pub struct State {
    pub entries: Vec<Entry>,
    pub selected: usize,
}

impl State {
    /// An empty inspector state.
    pub fn new() -> Self {
        State::default()
    }

    /// Append a parsed entry.
    pub fn push(&mut self, entry: Entry) {
        self.entries.push(entry);
    }

    /// Parse a line and append it when it is a valid access-log record.
    /// Returns whether an entry was added.
    pub fn push_line(&mut self, line: &str) -> bool {
        match Entry::parse(line) {
            Some(e) => {
                self.push(e);
                true
            }
            None => false,
        }
    }

    /// Move the selection one row toward newer entries (down), clamped.
    pub fn select_next(&mut self) {
        if !self.entries.is_empty() && self.selected + 1 < self.entries.len() {
            self.selected += 1;
        }
    }

    /// Move the selection one row toward older entries (up), clamped.
    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// The currently selected entry, if any.
    pub fn selected(&self) -> Option<&Entry> {
        self.entries.get(self.selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_line() {
        let e = Entry::parse("12:00:01\tapp.test\tGET\t/api/health\tlocalhost:8080\t200\t12ms")
            .expect("full line parses");
        assert_eq!(e.ts, "12:00:01");
        assert_eq!(e.domain, "app.test");
        assert_eq!(e.method, "GET");
        assert_eq!(e.path, "/api/health");
        assert_eq!(e.upstream, "localhost:8080");
        assert_eq!(e.status, "200");
        assert_eq!(e.duration, "12ms");
    }

    #[test]
    fn parses_minimal_line() {
        let e = Entry::parse("12:00:02\tapp.test\t404\t3ms").expect("minimal line parses");
        assert_eq!(e.domain, "app.test");
        assert_eq!(e.status, "404");
        assert_eq!(e.duration, "3ms");
        assert!(e.method.is_empty() && e.path.is_empty() && e.upstream.is_empty());
    }

    #[test]
    fn rejects_malformed_lines() {
        assert!(Entry::parse("").is_none());
        assert!(Entry::parse("just one column").is_none());
        assert!(Entry::parse("a\tb\tc").is_none()); // 3 cols: neither shape
    }

    #[test]
    fn selection_moves_and_clamps() {
        let mut s = State::new();
        assert!(s.selected().is_none());
        s.select_next(); // no-op on empty
        s.select_prev();
        assert_eq!(s.selected, 0);

        for i in 0..3 {
            assert!(s.push_line(&format!("12:00:0{i}\tapp.test\t200\t1ms")));
        }
        assert_eq!(s.entries.len(), 3);
        assert_eq!(s.selected, 0);
        s.select_prev(); // clamp at 0
        assert_eq!(s.selected, 0);
        s.select_next();
        s.select_next();
        assert_eq!(s.selected, 2);
        s.select_next(); // clamp at last
        assert_eq!(s.selected, 2);
        assert_eq!(s.selected().unwrap().ts, "12:00:02");
    }

    #[test]
    fn push_line_ignores_garbage() {
        let mut s = State::new();
        assert!(!s.push_line("nonsense"));
        assert!(s.entries.is_empty());
    }
}
