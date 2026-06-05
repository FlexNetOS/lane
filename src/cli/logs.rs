//! `lane logs` — tail the access log, optionally filtered by domain.
//!
//! Faithful port of `cmd/logs.go` plus `formatLogLine`. Supports `--flush`
//! (clear the log), an optional domain-name filter, and `--follow` (tail like
//! `tail -f`).

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

use crate::config;
use crate::term;

use super::normalize_name;

/// Run `lane logs`.
pub async fn run(args: &super::LogsArgs) -> Result<()> {
    let log_path = config::log_path();

    let arg_count = usize::from(args.name.is_some());
    validate_logs_flags(args.flush, args.follow, arg_count, args.lines)?;

    if args.flush {
        match std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&log_path)
        {
            Ok(_) => {
                println!("Cleared access logs.");
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                println!("No logs to clear.");
                return Ok(());
            }
            Err(e) => return Err(anyhow::Error::new(e).context("clearing logs")),
        }
    }

    let file = match std::fs::File::open(&log_path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("No logs yet. Start a domain first with 'lane start'.");
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };

    let filter = args.name.as_deref().map(normalize_name).unwrap_or_default();

    // Render one matched line in the active output format (NDJSON when --json,
    // otherwise the colorized human line). Shared by the tail block and the
    // stream loop so both honor --json identically.
    let render = |line: &str| {
        if args.json {
            format_log_line_json(line)
        } else {
            format_log_line(line)
        }
    };

    let mut reader = BufReader::new(file);

    if let Some(n) = args.lines {
        // `n` is already validated > 0 by validate_logs_flags; narrow to usize.
        let n = n as usize;
        // Read from the start, retaining only the last `n` matching lines.
        let tail = collect_tail(reader_lines(&mut reader)?, &filter, n);
        for line in &tail {
            println!("{}", render(line));
        }
        if !args.follow {
            return Ok(());
        }
        // --follow: the full read above left `reader` at EOF, so fall through to
        // the existing stream loop, which now reads only newly-appended lines.
        // (Do NOT seek-to-EOF separately — we already reached EOF by reading.)
    } else if args.follow {
        let _ = reader.seek(SeekFrom::End(0));
    }

    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).context("reading logs")?;
        if n == 0 {
            // EOF.
            if !args.follow {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        }

        let line = line.trim_end_matches('\n');
        if !filter.is_empty() && !line.contains(&filter) {
            continue;
        }

        println!("{}", render(line));
    }

    Ok(())
}

/// Validate the `--flush` / `--lines` flag combinations, mirroring Go's
/// `validateLogsFlags` and extending it for `--lines`.
///
/// `--flush` cannot be combined with `--follow`, a domain-filter argument, or
/// `--lines`. When `flush` is false, the flush-only checks are skipped, but
/// `--lines` (if present) must still be a positive integer.
fn validate_logs_flags(
    flush: bool,
    follow: bool,
    arg_count: usize,
    lines: Option<i64>,
) -> Result<()> {
    if flush {
        if follow {
            return Err(anyhow!("--flush cannot be used with --follow"));
        }
        if arg_count > 0 {
            return Err(anyhow!("--flush does not support domain filter"));
        }
        if lines.is_some() {
            return Err(anyhow!("--flush cannot be used with --lines"));
        }
        return Ok(());
    }
    if let Some(n) = lines {
        if n <= 0 {
            return Err(anyhow!("--lines must be a positive integer"));
        }
    }
    Ok(())
}

/// Read lines from `reader` into a `String` iterator, draining the reader to
/// EOF. Newlines are preserved on each item (matching the stream loop's
/// `read_line` semantics); `collect_tail` trims them. Lines are yielded lazily
/// so memory stays bounded by `collect_tail`'s ring buffer, not the file size.
fn reader_lines<R: BufRead>(reader: &mut R) -> Result<impl Iterator<Item = String> + '_> {
    Ok(std::iter::from_fn(move || {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => None,
            Ok(_) => Some(line),
            Err(_) => None,
        }
    }))
}

/// Collect the last `n` lines that pass `filter` into a bounded ring buffer.
/// Retains at most `n` matching lines at once (memory is O(n), not O(file)).
/// Lines are returned oldest→newest. `filter` matching mirrors the stream
/// loop: empty filter matches everything, otherwise substring containment,
/// after trimming a trailing newline.
fn collect_tail<I>(lines: I, filter: &str, n: usize) -> VecDeque<String>
where
    I: Iterator<Item = String>,
{
    let mut buf: VecDeque<String> = VecDeque::with_capacity(n);
    for line in lines {
        let line = line.trim_end_matches('\n');
        if !filter.is_empty() && !line.contains(filter) {
            continue;
        }
        if buf.len() == n {
            buf.pop_front();
        }
        buf.push_back(line.to_string());
    }
    buf
}

/// Return the status-color styling function for a status string, keyed on its
/// first digit: `5`->red, `4`->yellow, `3`->cyan, else green. Mirrors the
/// inline switch in `cmd/logs.go`.
fn status_style(status: &str) -> fn(&str) -> String {
    match status.as_bytes().first() {
        Some(b'5') => |s: &str| term::red(s),
        Some(b'4') => |s: &str| term::yellow(s),
        Some(b'3') => |s: &str| term::cyan(s),
        _ => |s: &str| term::green(s),
    }
}

/// Format one TAB-separated access-log line for display, mirroring Go's
/// `formatLogLine`.
///
/// - 4 fields (minimal): `ts  domain  status  dur`
/// - 7+ fields (full):   `ts  domain  method path → upstream  status  dur`
/// - anything else: returned unchanged.
fn format_log_line(line: &str) -> String {
    let parts: Vec<&str> = line.split('\t').collect();

    if parts.len() == 4 {
        let ts = parts[0];
        let domain = parts[1];
        let status = parts[2];
        let duration = parts[3];

        let style = status_style(status);
        return format!(
            "{} {} {} {}",
            term::dim(ts),
            term::magenta(domain),
            style(status),
            term::dim(duration),
        );
    }

    if parts.len() < 7 {
        return line.to_string();
    }

    let ts = parts[0];
    let domain = parts[1];
    let method = parts[2];
    let path = parts[3];
    let upstream = parts[4];
    let status = parts[5];
    let duration = parts[6];

    let style = status_style(status);
    format!(
        "{} {} {} {} → {} {} {}",
        term::dim(ts),
        term::magenta(domain),
        method,
        path,
        term::dim(upstream),
        style(status),
        term::dim(duration),
    )
}

/// JSON analogue of `format_log_line`: render one TAB-separated access-log line
/// as a single compact NDJSON object. Shapes mirror `format_log_line` exactly:
/// - 4 fields  -> minimal 4-key object
/// - 7+ fields -> full 7-key object (first 7 fields; extras dropped)
/// - anything else -> `{"raw": "<line>"}` (passthrough analogue)
///
/// All values are emitted as JSON strings (the access log stores everything as
/// text). Output is compact (`to_string`, not `to_string_pretty`) so each record
/// is exactly one line — required for the `--follow` NDJSON streaming contract.
fn format_log_line_json(line: &str) -> String {
    let parts: Vec<&str> = line.split('\t').collect();

    if parts.len() == 4 {
        return serde_json::json!({
            "ts": parts[0],
            "domain": parts[1],
            "status": parts[2],
            "duration": parts[3],
        })
        .to_string();
    }

    if parts.len() >= 7 {
        return serde_json::json!({
            "ts": parts[0],
            "domain": parts[1],
            "method": parts[2],
            "path": parts[3],
            "upstream": parts[4],
            "status": parts[5],
            "duration": parts[6],
        })
        .to_string();
    }

    serde_json::json!({ "raw": line }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Port of TestValidateLogsFlags — all four table cases.
    #[test]
    fn validate_logs_flags_cases() {
        // flush with follow -> error.
        let err = validate_logs_flags(true, true, 0, None).expect_err("expected error");
        assert!(
            err.to_string()
                .contains("--flush cannot be used with --follow"),
            "got {err}"
        );

        // flush with filter arg -> error.
        let err = validate_logs_flags(true, false, 1, None).expect_err("expected error");
        assert!(
            err.to_string()
                .contains("--flush does not support domain filter"),
            "got {err}"
        );

        // flush valid -> ok.
        validate_logs_flags(true, false, 0, None).expect("flush valid should be ok");

        // not flushing ignores follow and args -> ok.
        validate_logs_flags(false, true, 1, None).expect("non-flush should ignore flags");
    }

    // --flush combined with --lines is rejected with the exact error string.
    #[test]
    fn validate_logs_flags_flush_with_lines() {
        let err = validate_logs_flags(true, false, 0, Some(3)).expect_err("expected error");
        assert!(
            err.to_string()
                .contains("--flush cannot be used with --lines"),
            "got {err}"
        );
    }

    // N <= 0 (zero and negative) is rejected with the exact error string;
    // positive N is accepted. Follows the i64-validate-then-use convention.
    #[test]
    fn validate_logs_flags_lines_must_be_positive() {
        let err = validate_logs_flags(false, false, 0, Some(0)).expect_err("expected error");
        assert!(
            err.to_string()
                .contains("--lines must be a positive integer"),
            "got {err}"
        );

        let err = validate_logs_flags(false, false, 0, Some(-1)).expect_err("expected error");
        assert!(
            err.to_string()
                .contains("--lines must be a positive integer"),
            "got {err}"
        );

        validate_logs_flags(false, false, 0, Some(5)).expect("positive lines should be ok");
    }

    // collect_tail returns the last N matching lines, oldest->newest, and
    // excludes non-matching lines from the count.
    #[test]
    fn collect_tail_returns_last_n_matching_in_order() {
        let lines = vec![
            "12:00:00\tmyapp.test\t200\t1ms".to_string(),
            "12:00:01\tother.test\t200\t1ms".to_string(),
            "12:00:02\tmyapp.test\t404\t1ms".to_string(),
            "12:00:03\tmyapp.test\t500\t1ms".to_string(),
        ];
        let tail = collect_tail(lines.into_iter(), "myapp.test", 2);
        let got: Vec<&str> = tail.iter().map(String::as_str).collect();
        assert_eq!(
            got,
            vec![
                "12:00:02\tmyapp.test\t404\t1ms",
                "12:00:03\tmyapp.test\t500\t1ms",
            ]
        );
    }

    // N larger than the number of available matching lines prints all of them
    // (no error, no padding).
    #[test]
    fn collect_tail_n_larger_than_available_returns_all() {
        let lines = vec![
            "a\tmyapp.test\t200\t1ms".to_string(),
            "b\tother.test\t200\t1ms".to_string(),
            "c\tmyapp.test\t200\t1ms".to_string(),
            "d\tmyapp.test\t200\t1ms".to_string(),
        ];
        let tail = collect_tail(lines.into_iter(), "myapp.test", 100);
        assert_eq!(tail.len(), 3, "expected all 3 matching lines, got {tail:?}");
    }

    // Fewer-than-N matching lines yields exactly that many.
    #[test]
    fn collect_tail_fewer_than_n() {
        let lines = vec![
            "a\tmyapp.test\t200\t1ms".to_string(),
            "b\tother.test\t200\t1ms".to_string(),
        ];
        let tail = collect_tail(lines.into_iter(), "myapp.test", 5);
        let got: Vec<&str> = tail.iter().map(String::as_str).collect();
        assert_eq!(got, vec!["a\tmyapp.test\t200\t1ms"]);
    }

    // An empty filter matches every line; the tail caps at N.
    #[test]
    fn collect_tail_empty_filter_matches_all() {
        let lines = vec![
            "a\tone.test\t200\t1ms".to_string(),
            "b\ttwo.test\t200\t1ms".to_string(),
            "c\tthree.test\t200\t1ms".to_string(),
        ];
        let tail = collect_tail(lines.into_iter(), "", 2);
        let got: Vec<&str> = tail.iter().map(String::as_str).collect();
        assert_eq!(
            got,
            vec!["b\ttwo.test\t200\t1ms", "c\tthree.test\t200\t1ms"]
        );
    }

    // Empty input yields an empty deque.
    #[test]
    fn collect_tail_empty_input() {
        let tail = collect_tail(std::iter::empty::<String>(), "myapp.test", 5);
        assert!(tail.is_empty());
    }

    // Port of TestFormatLogLineMinimal — 4-field lines keep domain + status.
    #[test]
    fn format_log_line_minimal() {
        let cases: &[(&str, &str)] = &[
            ("12:00:00\tmyapp.test\t500\t10ms", "500"),
            ("12:00:00\tmyapp.test\t404\t10ms", "404"),
            ("12:00:00\tmyapp.test\t301\t10ms", "301"),
            ("12:00:00\tmyapp.test\t200\t10ms", "200"),
        ];
        for (line, status) in cases {
            let got = format_log_line(line);
            assert!(
                got.contains("myapp.test"),
                "expected domain in output, got: {got:?}"
            );
            assert!(
                got.contains(status),
                "expected status {status:?} in output, got: {got:?}"
            );
        }
    }

    // Port of TestFormatLogLineFull — 7-field lines keep method, path, upstream.
    #[test]
    fn format_log_line_full() {
        let line = "12:00:00\tmyapp.test\tGET\t/api/health\t3000\t200\t12ms";
        let got = format_log_line(line);
        assert!(got.contains("GET"), "expected method, got: {got:?}");
        assert!(got.contains("/api/health"), "expected path, got: {got:?}");
        assert!(got.contains("3000"), "expected upstream port, got: {got:?}");
    }

    // Port of TestFormatLogLineMalformedPassthrough — non 4/7-field lines pass
    // through unchanged.
    #[test]
    fn format_log_line_malformed_passthrough() {
        let line = "malformed";
        assert_eq!(format_log_line(line), line);
    }

    // The status digit selects the color family (verified via direct term fns).
    #[test]
    fn status_style_buckets() {
        assert_eq!(status_style("500")("x"), term::red("x"));
        assert_eq!(status_style("404")("x"), term::yellow("x"));
        assert_eq!(status_style("301")("x"), term::cyan("x"));
        assert_eq!(status_style("200")("x"), term::green("x"));
        assert_eq!(status_style("")("x"), term::green("x"));
    }

    // Parse the formatter output as a JSON object map for key/value assertions.
    fn parse_json_obj(line: &str) -> serde_json::Map<String, serde_json::Value> {
        let value: serde_json::Value =
            serde_json::from_str(&format_log_line_json(line)).expect("formatter emits valid JSON");
        value
            .as_object()
            .expect("formatter emits a JSON object")
            .clone()
    }

    // AC-2: a 4-field line produces exactly the 4-key minimal shape.
    #[test]
    fn format_log_line_json_minimal_shape() {
        let obj = parse_json_obj("12:00:00\tmyapp.test\t200\t10ms");
        let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["domain", "duration", "status", "ts"].into_iter().collect(),
            "expected exactly the minimal keys, got {keys:?}"
        );
        assert_eq!(obj["ts"], serde_json::json!("12:00:00"));
        assert_eq!(obj["domain"], serde_json::json!("myapp.test"));
        assert_eq!(obj["status"], serde_json::json!("200"));
        assert_eq!(obj["duration"], serde_json::json!("10ms"));
    }

    // AC-3: a 7-field line produces exactly the 7-key full shape (no `raw`).
    #[test]
    fn format_log_line_json_full_shape() {
        let obj = parse_json_obj("12:00:00\tmyapp.test\tGET\t/api/health\t3000\t200\t12ms");
        let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["domain", "duration", "method", "path", "status", "ts", "upstream"]
                .into_iter()
                .collect(),
            "expected exactly the full keys, got {keys:?}"
        );
        assert!(!obj.contains_key("raw"), "full shape must not have `raw`");
        assert_eq!(obj["ts"], serde_json::json!("12:00:00"));
        assert_eq!(obj["domain"], serde_json::json!("myapp.test"));
        assert_eq!(obj["method"], serde_json::json!("GET"));
        assert_eq!(obj["path"], serde_json::json!("/api/health"));
        assert_eq!(obj["upstream"], serde_json::json!("3000"));
        assert_eq!(obj["status"], serde_json::json!("200"));
        assert_eq!(obj["duration"], serde_json::json!("12ms"));
    }

    // AC-4: an 8-field line yields the 7-key full shape, dropping the trailing field.
    #[test]
    fn format_log_line_json_full_drops_extra_fields() {
        let obj = parse_json_obj("12:00:00\tmyapp.test\tGET\t/api/health\t3000\t200\t12ms\textra");
        assert_eq!(obj.len(), 7, "expected exactly 7 keys, got {}", obj.len());
        assert!(!obj.contains_key("raw"));
        // Only the first 7 fields are consumed; `extra` must not appear anywhere.
        assert_eq!(obj["duration"], serde_json::json!("12ms"));
        for value in obj.values() {
            assert_ne!(value, &serde_json::json!("extra"), "extra field leaked in");
        }
    }

    // AC-5: a 1-field line passes through as `{"raw": "<line>"}`.
    #[test]
    fn format_log_line_json_malformed_passthrough() {
        let obj = parse_json_obj("malformed");
        assert_eq!(obj.len(), 1);
        assert_eq!(obj["raw"], serde_json::json!("malformed"));
    }

    // AC-6: 5-field and 6-field lines are malformed -> `{"raw": "<line>"}`.
    #[test]
    fn format_log_line_json_five_and_six_fields_are_raw() {
        let five = "a\tb\tc\td\te";
        let obj = parse_json_obj(five);
        assert_eq!(obj.len(), 1);
        assert_eq!(obj["raw"], serde_json::json!(five));

        let six = "a\tb\tc\td\te\tf";
        let obj = parse_json_obj(six);
        assert_eq!(obj.len(), 1);
        assert_eq!(obj["raw"], serde_json::json!(six));
    }

    // AC-7: the empty string (0 fields) yields `{"raw": ""}`.
    #[test]
    fn format_log_line_json_empty_line_is_raw() {
        let obj = parse_json_obj("");
        assert_eq!(obj.len(), 1);
        assert_eq!(obj["raw"], serde_json::json!(""));
    }

    // AC-8: numeric-looking values stay JSON strings, not numbers.
    #[test]
    fn format_log_line_json_values_are_strings() {
        let minimal = format_log_line_json("12:00:00\tmyapp.test\t200\t10ms");
        assert!(
            minimal.contains("\"status\":\"200\""),
            "status must be a quoted string, got {minimal:?}"
        );
        assert!(
            !minimal.contains("\"status\":200"),
            "status must not be a bare number, got {minimal:?}"
        );

        let full = format_log_line_json("12:00:00\tmyapp.test\tGET\t/api/health\t3000\t200\t12ms");
        assert!(
            full.contains("\"upstream\":\"3000\""),
            "upstream must be a quoted string, got {full:?}"
        );
        assert!(
            !full.contains("\"upstream\":3000"),
            "upstream must not be a bare number, got {full:?}"
        );
    }

    // AC-9: JSON-special characters in a value are escaped and round-trip intact.
    #[test]
    fn format_log_line_json_escapes_special_chars() {
        let path = "/a\"b\\c";
        let line = format!("12:00:00\tmyapp.test\tGET\t{path}\t3000\t200\t12ms");
        let obj = parse_json_obj(&line);
        assert_eq!(
            obj["path"],
            serde_json::json!(path),
            "path must round-trip through serde escaping"
        );
    }

    // AC-10: a single record is compact — no interior newline (guards pretty-print).
    #[test]
    fn format_log_line_json_is_compact_single_line() {
        let out = format_log_line_json("12:00:00\tmyapp.test\tGET\t/api/health\t3000\t200\t12ms");
        assert!(
            !out.contains('\n'),
            "record must contain no interior newline, got {out:?}"
        );
    }
}
