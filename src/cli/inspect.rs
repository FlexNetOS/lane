//! `lane inspect` — a live request-inspector TUI (ngrok-web-UI pattern, in the
//! terminal). It tails the proxy daemon's access log and shows requests in a
//! scrollable table with a detail pane, updating as new requests arrive.
//!
//! The pure data model + selection logic live in [`crate::inspect`]; this file
//! is the interactive shell (crossterm alternate screen + raw mode + key
//! events, comfy-table rendering). When stdout is not a TTY (piped/CI), it
//! prints a one-shot snapshot table instead of entering the interactive loop.

use std::io::{Read, Seek, SeekFrom, Write};
use std::time::Duration;

use anyhow::{Context, Result};
use comfy_table::{Cell, Table};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::tty::IsTty;
use crossterm::{cursor, execute};

use crate::config;
use crate::inspect::{Entry, State};

/// Normalize a `--name` filter the way the log filter does: trim, lowercase,
/// drop a trailing dot. Empty ⇒ no filtering.
fn normalize_filter(name: &str) -> String {
    name.trim().trim_end_matches('.').to_lowercase()
}

/// Does this entry pass the (already-normalized) domain filter?
fn keep(entry: &Entry, filter: &str) -> bool {
    filter.is_empty() || entry.domain.trim().trim_end_matches('.').to_lowercase() == filter
}

/// `lane inspect [name]`.
pub async fn run(args: &super::InspectArgs) -> Result<()> {
    let log_path = config::log_path();
    let filter = args
        .name
        .as_deref()
        .map(normalize_filter)
        .unwrap_or_default();

    let mut file = match std::fs::File::open(&log_path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("No logs yet. Start a domain first with 'lane start'.");
            return Ok(());
        }
        Err(e) => return Err(e).context("opening access log"),
    };

    // Load the existing log into state and remember where we stopped reading.
    let mut buf = String::new();
    file.read_to_string(&mut buf)
        .context("reading access log")?;
    let mut pos = buf.len() as u64;
    let mut state = State::new();
    for line in buf.lines() {
        if let Some(e) = Entry::parse(line) {
            if keep(&e, &filter) {
                state.push(e);
            }
        }
    }
    // Start focused on the most recent request.
    state.selected = state.entries.len().saturating_sub(1);

    // Non-interactive (piped / not a terminal): print a snapshot and exit.
    if !std::io::stdout().is_tty() {
        print!("{}", render_table(&state));
        return Ok(());
    }

    run_interactive(&mut state, &mut file, &mut pos, &filter)
}

/// The interactive alternate-screen loop. Restores the terminal via a guard.
fn run_interactive(
    state: &mut State,
    file: &mut std::fs::File,
    pos: &mut u64,
    filter: &str,
) -> Result<()> {
    let _guard = TerminalGuard::enter()?;

    loop {
        draw(state)?;

        // Poll for a keystroke; on timeout, tail the log for new requests.
        if event::poll(Duration::from_millis(500)).context("polling terminal events")? {
            if let Event::Key(key) = event::read().context("reading terminal event")? {
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => state.select_prev(),
                    KeyCode::Down | KeyCode::Char('j') => state.select_next(),
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                    _ => {}
                }
            }
        } else {
            tail_into(state, file, pos, filter)?;
        }
    }
    Ok(())
}

/// Read any bytes appended to the log since `pos`, parse them, and append the
/// matching ones. Handles truncation (file shorter than `pos`) by reloading.
fn tail_into(
    state: &mut State,
    file: &mut std::fs::File,
    pos: &mut u64,
    filter: &str,
) -> Result<()> {
    let len = file.metadata().map(|m| m.len()).unwrap_or(*pos);
    if len < *pos {
        // The log was rotated/truncated; reset and reload from the top.
        *pos = 0;
        state.entries.clear();
        state.selected = 0;
    }
    file.seek(SeekFrom::Start(*pos))
        .context("seeking access log")?;
    let mut chunk = String::new();
    let read = file
        .read_to_string(&mut chunk)
        .context("tailing access log")?;
    *pos += read as u64;
    if chunk.is_empty() {
        return Ok(());
    }
    let was_at_end = state.selected + 1 >= state.entries.len();
    for line in chunk.lines() {
        if let Some(e) = Entry::parse(line) {
            if keep(&e, filter) {
                state.push(e);
            }
        }
    }
    // Keep following the tail if we were already at the newest row.
    if was_at_end {
        state.selected = state.entries.len().saturating_sub(1);
    }
    Ok(())
}

/// Render one full frame (table + detail + footer) and write it to the screen.
fn draw(state: &State) -> Result<()> {
    let mut out = std::io::stdout();
    let frame = render_frame(state);
    // In raw mode `\n` does not return the carriage; translate to `\r\n`.
    let frame = frame.replace('\n', "\r\n");
    execute!(out, Clear(ClearType::All), cursor::MoveTo(0, 0)).context("clearing screen")?;
    write!(out, "{frame}").context("drawing frame")?;
    out.flush().context("flushing frame")?;
    Ok(())
}

/// Build the full text frame: header, request table, selected-request detail,
/// and the key-hint footer. Pure (no I/O) so it is unit-testable.
fn render_frame(state: &State) -> String {
    let mut s = String::new();
    s.push_str("lane inspect — live requests\n\n");
    s.push_str(&render_table(state));
    s.push('\n');
    s.push_str(&render_detail(state));
    s.push_str(&format!(
        "\n  ↑/↓ select · q quit · {} request(s)\n",
        state.entries.len()
    ));
    s
}

/// Render the request list as a borderless table, marking the selected row.
fn render_table(state: &State) -> String {
    let mut table = Table::new();
    table.load_preset(comfy_table::presets::NOTHING);
    table.set_header(vec![
        " ", "Time", "Domain", "Method", "Path", "Status", "Dur",
    ]);
    if state.entries.is_empty() {
        table.add_row(vec![Cell::new(""), Cell::new("(waiting for requests…)")]);
    }
    for (i, e) in state.entries.iter().enumerate() {
        let marker = if i == state.selected { ">" } else { " " };
        table.add_row(vec![
            Cell::new(marker),
            Cell::new(&e.ts),
            Cell::new(&e.domain),
            Cell::new(&e.method),
            Cell::new(&e.path),
            Cell::new(&e.status),
            Cell::new(&e.duration),
        ]);
    }
    table.to_string()
}

/// Render the detail block for the selected request.
fn render_detail(state: &State) -> String {
    match state.selected() {
        None => String::new(),
        Some(e) => {
            let mut d = String::from("\n  ── selected ──\n");
            d.push_str(&format!("  time:     {}\n", e.ts));
            d.push_str(&format!("  domain:   {}\n", e.domain));
            if !e.method.is_empty() {
                d.push_str(&format!("  request:  {} {}\n", e.method, e.path));
                d.push_str(&format!("  upstream: {}\n", e.upstream));
            }
            d.push_str(&format!("  status:   {}\n", e.status));
            d.push_str(&format!("  duration: {}\n", e.duration));
            d
        }
    }
}

/// RAII guard: enter the alternate screen + raw mode on construction, and
/// always restore the terminal on drop (even on error/panic).
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("enabling raw mode")?;
        execute!(std::io::stdout(), EnterAlternateScreen, cursor::Hide)
            .context("entering alternate screen")?;
        Ok(TerminalGuard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen, cursor::Show);
        let _ = disable_raw_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inspect::State;

    fn state_with(lines: &[&str]) -> State {
        let mut s = State::new();
        for l in lines {
            s.push_line(l);
        }
        s
    }

    #[test]
    fn table_has_headers_and_rows() {
        let s = state_with(&[
            "12:00:01\tapp.test\tGET\t/api\tlocalhost:8080\t200\t12ms",
            "12:00:02\tapp.test\tPOST\t/login\tlocalhost:8080\t302\t40ms",
        ]);
        let t = render_table(&s);
        assert!(t.contains("Domain"), "header present: {t}");
        assert!(t.contains("app.test"));
        assert!(t.contains("/login"));
        assert!(t.contains("302"));
    }

    #[test]
    fn table_marks_selected_row() {
        let mut s = state_with(&[
            "12:00:01\tapp.test\t200\t1ms",
            "12:00:02\tapp.test\t200\t2ms",
        ]);
        s.selected = 1;
        let t = render_table(&s);
        // The marker '>' appears exactly once (on the selected row).
        assert_eq!(t.matches('>').count(), 1, "{t}");
    }

    #[test]
    fn empty_table_shows_waiting() {
        let t = render_table(&State::new());
        assert!(t.contains("waiting for requests"), "{t}");
    }

    #[test]
    fn detail_shows_full_request_fields() {
        let s = state_with(&["12:00:01\tapp.test\tGET\t/api\tlocalhost:8080\t200\t12ms"]);
        let d = render_detail(&s);
        assert!(d.contains("GET /api"));
        assert!(d.contains("upstream: localhost:8080"));
        assert!(d.contains("status:   200"));
    }

    #[test]
    fn detail_minimal_omits_request_line() {
        let s = state_with(&["12:00:01\tapp.test\t404\t3ms"]);
        let d = render_detail(&s);
        assert!(
            !d.contains("request:"),
            "minimal mode has no method/path: {d}"
        );
        assert!(d.contains("status:   404"));
    }

    #[test]
    fn frame_includes_footer_count() {
        let s = state_with(&["12:00:01\tapp.test\t200\t1ms"]);
        let f = render_frame(&s);
        assert!(f.contains("1 request(s)"), "{f}");
        assert!(f.contains("q quit"));
    }

    #[test]
    fn filter_matches_domain_case_insensitively() {
        let e = Entry::parse("12:00:01\tApp.Test\t200\t1ms").unwrap();
        assert!(keep(&e, &normalize_filter("app.test")));
        assert!(keep(&e, "")); // empty filter keeps everything
        assert!(!keep(&e, &normalize_filter("other.test")));
    }
}
