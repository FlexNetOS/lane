//! Global async access-log writer.
//!
//! Faithful port of `internal/log` from the Go tool `slim`. Go used a buffered
//! channel fed from the proxy hot path and drained by a dedicated goroutine that
//! flushed a `bufio.Writer` on a ticker and on shutdown. Here we mirror that with
//! a dedicated `std::thread` fed by a bounded [`std::sync::mpsc::SyncSender`].
//!
//! [`request`] performs a non-blocking [`SyncSender::try_send`] and **drops** the
//! line if the buffer is full — mirroring Go's `select { case ch <- line: default: }`
//! so the proxy hot path never blocks.
//!
//! Global state (current mode + sender + writer join handle) lives in a
//! `Mutex` inside a `OnceLock`.

use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::term;

/// 10 MB — files larger than this are truncated on (re)configure.
const MAX_LOG_SIZE: u64 = 10 << 20;

/// Log mode: full per-request lines (method/path/upstream included).
const LOG_MODE_FULL: &str = "full";
/// Log mode: minimal per-request lines (domain/status/duration only).
const LOG_MODE_MINIMAL: &str = "minimal";
/// Log mode: logging disabled.
const LOG_MODE_OFF: &str = "off";

/// Channel buffer depth (mirrors Go's `logBufferSize`).
const LOG_BUFFER_SIZE: usize = 4096;
/// Flush cadence for the buffered writer (mirrors Go's `logFlushPeriod`).
const LOG_FLUSH_PERIOD: Duration = Duration::from_millis(250);
/// Writer buffer size (mirrors Go's `bufio.NewWriterSize(file, 64*1024)`).
const WRITER_BUF_SIZE: usize = 64 * 1024;

/// Global writer state. A `None` sender/handle means no active writer.
struct State {
    mode: String,
    sender: Option<SyncSender<String>>,
    handle: Option<JoinHandle<()>>,
}

impl Default for State {
    fn default() -> Self {
        // Mirrors Go's `var logMode = logModeFull`.
        State {
            mode: LOG_MODE_FULL.to_string(),
            sender: None,
            handle: None,
        }
    }
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(State::default()))
}

/// Normalize a requested log mode, mirroring Go's `normalizeMode`:
/// `""` and `"full"` → full; `"minimal"` → minimal; `"off"` → off; anything
/// else → full.
fn normalize_mode(mode: &str) -> &'static str {
    match mode {
        LOG_MODE_FULL | "" => LOG_MODE_FULL,
        LOG_MODE_MINIMAL => LOG_MODE_MINIMAL,
        LOG_MODE_OFF => LOG_MODE_OFF,
        _ => LOG_MODE_FULL,
    }
}

/// Shut down the active writer (close the sender so the thread drains and exits,
/// then join it). Mirrors Go's `shutdownWriterLocked`.
///
/// Caller must hold the state lock.
fn shutdown_writer_locked(st: &mut State) {
    // Dropping the sender closes the channel; the writer loop sees the channel
    // disconnect, drains any remaining lines, flushes, and returns.
    st.sender = None;
    if let Some(handle) = st.handle.take() {
        let _ = handle.join();
    }
}

/// Configure (or reconfigure) the access-log output.
///
/// Shuts down any existing writer first, normalizes `mode`, and returns early
/// (logging disabled) when the mode is `off`. Otherwise rotates the file by
/// truncating it when it already exceeds 10 MB, opens it for append (mode
/// `0644`), and spawns the writer thread.
///
/// Mirrors Go's `SetOutput`.
pub fn set_output(path: &Path, mode: &str) -> anyhow::Result<()> {
    let mut st = state().lock().unwrap();

    shutdown_writer_locked(&mut st);
    st.mode = normalize_mode(mode).to_string();
    if st.mode == LOG_MODE_OFF {
        return Ok(());
    }

    // Rotate: if the file already exceeds the cap, truncate it.
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() > MAX_LOG_SIZE {
            let _ = std::fs::OpenOptions::new()
                .write(true)
                .open(path)
                .and_then(|f| f.set_len(0));
        }
    }

    let file = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o644)
        .open(path)?;

    let (tx, rx) = sync_channel::<String>(LOG_BUFFER_SIZE);
    let handle = std::thread::spawn(move || {
        writer_loop(BufWriter::with_capacity(WRITER_BUF_SIZE, file), rx)
    });

    st.sender = Some(tx);
    st.handle = Some(handle);
    Ok(())
}

/// Stop the active writer, flushing and closing the file.
///
/// Mirrors Go's `Close`.
pub fn close() {
    let mut st = state().lock().unwrap();
    shutdown_writer_locked(&mut st);
}

/// The dedicated writer thread body. Mirrors Go's `writerLoop`: it appends
/// incoming lines to a buffered writer, flushes periodically, and on channel
/// disconnect drains any remaining lines, flushes, and exits.
fn writer_loop<W: Write>(mut buffered: BufWriter<W>, entries: Receiver<String>) {
    loop {
        match entries.recv_timeout(LOG_FLUSH_PERIOD) {
            Ok(line) => {
                if buffered.write_all(line.as_bytes()).is_err() {
                    let _ = buffered.flush();
                    return;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let _ = buffered.flush();
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                // Drain anything still queued, then flush and close.
                while let Ok(line) = entries.try_recv() {
                    if buffered.write_all(line.as_bytes()).is_err() {
                        let _ = buffered.flush();
                        return;
                    }
                }
                let _ = buffered.flush();
                return;
            }
        }
    }
}

/// Record one proxied request to the access log.
///
/// Builds a TAB-separated line and non-blockingly enqueues it; the line is
/// silently dropped if the buffer is full or if logging is off. Mirrors Go's
/// `Request`.
///
/// - full:    `HH:MM:SS\tdomain\tmethod\tpath\tupstream\tstatus\tdur\n`
/// - minimal: `HH:MM:SS\tdomain\tstatus\tdur\n`
pub fn request(
    domain: &str,
    method: &str,
    path: &str,
    upstream: u16,
    status: u16,
    duration: Duration,
) {
    // Snapshot mode + sender under the lock, then release before sending so the
    // hot path holds the lock only briefly (mirrors Go's RLock window).
    let (mode, sender) = {
        let st = state().lock().unwrap();
        if st.mode == LOG_MODE_OFF {
            return;
        }
        match &st.sender {
            Some(tx) => (st.mode.clone(), tx.clone()),
            None => return,
        }
    };

    let ts = chrono::Local::now().format("%H:%M:%S");
    let dur = format_duration(duration);

    let line = if mode == LOG_MODE_MINIMAL {
        format!("{ts}\t{domain}\t{status}\t{dur}\n")
    } else {
        format!("{ts}\t{domain}\t{method}\t{path}\t{upstream}\t{status}\t{dur}\n")
    };

    // Non-blocking send: drop the line if the buffer is full (mirrors Go's
    // `select { case ch <- line: default: }`). A `Full` or `Disconnected`
    // (writer gone) error is silently ignored.
    let _ = sender.try_send(line);
}

/// Print an informational console line: cyan `[lane]` prefix + message.
///
/// Mirrors Go's `Info` (callers pass a preformatted string).
pub fn info(msg: &str) {
    println!("{} {msg}", term::cyan("[lane]"));
}

/// Print an error console line: red `[lane]` prefix + message.
///
/// Mirrors Go's `Error` (callers pass a preformatted string).
pub fn error(msg: &str) {
    println!("{} {msg}", term::red("[lane]"));
}

/// Format a duration compactly, mirroring Go's `FormatDuration`:
/// `<1ms` → `{µs}µs`; `<1s` → `{ms}ms`; else `{:.1}s`.
pub fn format_duration(d: Duration) -> String {
    if d < Duration::from_millis(1) {
        return format!("{}µs", d.as_micros());
    }
    if d < Duration::from_secs(1) {
        return format!("{}ms", d.as_millis());
    }
    format!("{:.1}s", d.as_secs_f64())
}

/// Format a timestamp as a relative "time ago" string, mirroring Go's
/// `FormatTimeAgo`: `just now` / `{N}m ago` / `{N}h ago` / `{N}d ago`.
pub fn format_time_ago(t: chrono::DateTime<chrono::Local>) -> String {
    let d = chrono::Local::now().signed_duration_since(t);
    if d < chrono::Duration::minutes(1) {
        return "just now".to_string();
    }
    if d < chrono::Duration::hours(1) {
        return format!("{}m ago", d.num_minutes());
    }
    if d < chrono::Duration::hours(24) {
        return format!("{}h ago", d.num_hours());
    }
    format!("{}d ago", d.num_hours() / 24)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // Ported from helpers_test.go::TestFormatDuration.
    #[test]
    fn test_format_duration() {
        let cases: &[(Duration, &str)] = &[
            (Duration::from_micros(800), "800µs"),
            (Duration::from_micros(1250), "1ms"),
            (Duration::from_millis(125), "125ms"),
            (Duration::from_millis(1500), "1.5s"),
        ];
        for (input, want) in cases {
            let got = format_duration(*input);
            assert_eq!(
                &got, want,
                "format_duration({input:?}) = {got:?}, want {want:?}"
            );
        }
    }

    // Ported from log_test.go::TestRequestWritesFullMode.
    #[test]
    #[serial_test::serial]
    fn test_request_writes_full_mode() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("access.log");
        set_output(&path, "full").expect("set_output");

        request(
            "myapp.test",
            "GET",
            "/health",
            3000,
            200,
            Duration::from_millis(12),
        );
        close();

        let data = std::fs::read_to_string(&path).expect("read log");
        let lines: Vec<&str> = data.trim().split('\n').collect();
        assert_eq!(lines.len(), 1, "expected 1 line, got {}", lines.len());

        let fields: Vec<&str> = lines[0].split('\t').collect();
        assert_eq!(
            fields.len(),
            7,
            "expected 7 fields in full mode, got {}: {:?}",
            fields.len(),
            lines[0]
        );
    }

    // Ported from log_test.go::TestRequestWritesMinimalMode.
    #[test]
    #[serial_test::serial]
    fn test_request_writes_minimal_mode() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("access.log");
        set_output(&path, "minimal").expect("set_output");

        request(
            "myapp.test",
            "GET",
            "/health",
            3000,
            200,
            Duration::from_millis(12),
        );
        close();

        let data = std::fs::read_to_string(&path).expect("read log");
        let lines: Vec<&str> = data.trim().split('\n').collect();
        assert_eq!(lines.len(), 1, "expected 1 line, got {}", lines.len());

        let fields: Vec<&str> = lines[0].split('\t').collect();
        assert_eq!(
            fields.len(),
            4,
            "expected 4 fields in minimal mode, got {}: {:?}",
            fields.len(),
            lines[0]
        );
    }

    // Ported from log_test.go::TestRequestOffModeWritesNothing.
    #[test]
    #[serial_test::serial]
    fn test_request_off_mode_writes_nothing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("access.log");
        set_output(&path, "off").expect("set_output");

        request(
            "myapp.test",
            "GET",
            "/health",
            3000,
            200,
            Duration::from_millis(12),
        );
        close();

        // No file should have been created in off mode.
        assert!(!path.exists(), "expected no log file, but {path:?} exists");
    }

    // Ported from log_test.go::TestSetOutputReconfigureFlushesPreviousWriter.
    #[test]
    #[serial_test::serial]
    fn test_set_output_reconfigure_flushes_previous_writer() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("access.log");
        set_output(&path, "full").expect("set_output full");

        request(
            "myapp.test",
            "GET",
            "/one",
            3000,
            200,
            Duration::from_millis(10),
        );
        // Reconfiguring must shut down (and thus flush+drain) the previous writer
        // before installing the new one.
        set_output(&path, "minimal").expect("set_output minimal");
        request(
            "myapp.test",
            "GET",
            "/two",
            3000,
            200,
            Duration::from_millis(10),
        );
        close();

        let data = std::fs::read_to_string(&path).expect("read log");
        let lines: Vec<&str> = data.trim().split('\n').collect();
        assert_eq!(
            lines.len(),
            2,
            "expected 2 lines after reconfigure, got {}",
            lines.len()
        );

        let first: Vec<&str> = lines[0].split('\t').collect();
        let second: Vec<&str> = lines[1].split('\t').collect();
        assert_eq!(
            first.len(),
            7,
            "expected full first line, got {}",
            first.len()
        );
        assert_eq!(
            second.len(),
            4,
            "expected minimal second line, got {}",
            second.len()
        );
    }
}
