//! `lane logs` — tail the access log, optionally filtered by domain.
//!
//! Faithful port of `cmd/logs.go` plus `formatLogLine`. Supports `--flush`
//! (clear the log), an optional domain-name filter, and `--follow` (tail like
//! `tail -f`).

use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

use crate::config;
use crate::term;

use super::normalize_name;

/// Run `lane logs`.
pub async fn run(args: &super::LogsArgs) -> Result<()> {
    let log_path = config::log_path();

    if args.flush {
        let arg_count = usize::from(args.name.is_some());
        validate_logs_flags(args.flush, args.follow, arg_count)?;

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

    let mut reader = BufReader::new(file);

    if args.follow {
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

        println!("{}", format_log_line(line));
    }

    Ok(())
}

/// Validate the `--flush` flag combination, mirroring Go's `validateLogsFlags`.
///
/// `--flush` cannot be combined with `--follow` or a domain-filter argument; if
/// `flush` is false, the other flags are ignored.
fn validate_logs_flags(flush: bool, follow: bool, arg_count: usize) -> Result<()> {
    if !flush {
        return Ok(());
    }
    if follow {
        return Err(anyhow!("--flush cannot be used with --follow"));
    }
    if arg_count > 0 {
        return Err(anyhow!("--flush does not support domain filter"));
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    // Port of TestValidateLogsFlags — all four table cases.
    #[test]
    fn validate_logs_flags_cases() {
        // flush with follow -> error.
        let err = validate_logs_flags(true, true, 0).expect_err("expected error");
        assert!(
            err.to_string()
                .contains("--flush cannot be used with --follow"),
            "got {err}"
        );

        // flush with filter arg -> error.
        let err = validate_logs_flags(true, false, 1).expect_err("expected error");
        assert!(
            err.to_string()
                .contains("--flush does not support domain filter"),
            "got {err}"
        );

        // flush valid -> ok.
        validate_logs_flags(true, false, 0).expect("flush valid should be ok");

        // not flushing ignores follow and args -> ok.
        validate_logs_flags(false, true, 1).expect("non-flush should ignore flags");
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
}
