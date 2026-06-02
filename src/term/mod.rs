//! Terminal styling, prompts, steps, and tables.
//!
//! Faithful port of `internal/term` from the Go tool `slim`. The Go code used
//! `lipgloss` styles built from `ANSIColor(n)`; here we use `owo-colors` to
//! produce the equivalent ANSI escape sequences.
//!
//! lipgloss `ANSIColor` → ANSI mapping reproduced here:
//! `1=red, 2=green, 3=yellow, 5=magenta, 6=cyan`; `Faint(true)` → dim;
//! `Bold(true)` → bold.

use std::io::{self, BufRead, Write};

use owo_colors::OwoColorize;

pub mod step;
pub mod table;

/// Style a string green (lipgloss `ANSIColor(2)`).
pub fn green<S: AsRef<str>>(s: S) -> String {
    s.as_ref().green().to_string()
}

/// Style a string red (lipgloss `ANSIColor(1)`).
pub fn red<S: AsRef<str>>(s: S) -> String {
    s.as_ref().red().to_string()
}

/// Style a string yellow (lipgloss `ANSIColor(3)`).
pub fn yellow<S: AsRef<str>>(s: S) -> String {
    s.as_ref().yellow().to_string()
}

/// Style a string cyan (lipgloss `ANSIColor(6)`).
pub fn cyan<S: AsRef<str>>(s: S) -> String {
    s.as_ref().cyan().to_string()
}

/// Style a string magenta (lipgloss `ANSIColor(5)`).
pub fn magenta<S: AsRef<str>>(s: S) -> String {
    s.as_ref().magenta().to_string()
}

/// Style a string dim/faint (lipgloss `Faint(true)`).
pub fn dim<S: AsRef<str>>(s: S) -> String {
    s.as_ref().dimmed().to_string()
}

/// Style a string bold (lipgloss `Bold(true)`).
pub fn bold<S: AsRef<str>>(s: S) -> String {
    s.as_ref().bold().to_string()
}

/// A green check mark (`✓`). Mirrors Go's `CheckMark`.
pub fn check_mark() -> String {
    green("✓")
}

/// A red cross mark (`✗`). Mirrors Go's `CrossMark`.
pub fn cross_mark() -> String {
    red("✗")
}

/// A yellow exclamation mark (`!`). Mirrors Go's `WarnMark`.
pub fn warn_mark() -> String {
    yellow("!")
}

/// Print `"{msg} [y/N] "`, read a line from stdin, and return `true` only if
/// the trimmed, lowercased answer is `y` or `yes`.
///
/// Mirrors Go's `ConfirmPrompt`: a failed/EOF read returns `false`.
pub fn confirm_prompt(msg: &str) -> bool {
    print!("{msg} [y/N] ");
    // Ensure the prompt is visible before blocking on input. Go's fmt.Printf
    // writes to stdout which is line/auto-flushed; flush explicitly here.
    let _ = io::stdout().flush();

    let mut line = String::new();
    let stdin = io::stdin();
    let mut handle = stdin.lock();
    // A read error or EOF (0 bytes) means "no input" → false, matching the Go
    // `!scanner.Scan()` early-return.
    match handle.read_line(&mut line) {
        Ok(0) | Err(_) => return false,
        Ok(_) => {}
    }
    let answer = line.trim().to_lowercase();
    answer == "y" || answer == "yes"
}

// Monomorphic, higher-ranked (`for<'a> fn(&'a str) -> String`) wrappers so they
// can be returned as a plain `fn(&str) -> String` pointer. A turbofished generic
// item like `green::<&str>` binds a single concrete lifetime and will not coerce
// to the higher-ranked pointer type, so we wrap each color function here.
fn sty_red(s: &str) -> String {
    red(s)
}
fn sty_yellow(s: &str) -> String {
    yellow(s)
}
fn sty_cyan(s: &str) -> String {
    cyan(s)
}
fn sty_green(s: &str) -> String {
    green(s)
}

/// Return the styling function appropriate for an HTTP status code, mirroring
/// Go's `StyleForStatus`:
/// `>=500` red, `>=400` yellow, `>=300` cyan, else green.
pub fn style_for_status(code: u16) -> fn(&str) -> String {
    if code >= 500 {
        sty_red
    } else if code >= 400 {
        sty_yellow
    } else if code >= 300 {
        sty_cyan
    } else {
        sty_green
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ported from styles_test.go::TestStyleForStatus. The Go test compared
    // lipgloss foreground colors; here we assert the semantic intent — that the
    // returned styling function matches the expected color function for each
    // bucket. We compare via a sentinel string and the corresponding direct
    // color function output (functional parity, not byte-identical escapes).
    #[test]
    fn test_style_for_status() {
        let sentinel = "x";
        assert_eq!(style_for_status(200)(sentinel), green(sentinel));
        assert_eq!(style_for_status(302)(sentinel), cyan(sentinel));
        assert_eq!(style_for_status(404)(sentinel), yellow(sentinel));
        assert_eq!(style_for_status(500)(sentinel), red(sentinel));
    }

    // Boundary checks mirroring the Go switch arms exactly.
    #[test]
    fn test_style_for_status_boundaries() {
        let s = "y";
        assert_eq!(style_for_status(299)(s), green(s));
        assert_eq!(style_for_status(300)(s), cyan(s));
        assert_eq!(style_for_status(399)(s), cyan(s));
        assert_eq!(style_for_status(400)(s), yellow(s));
        assert_eq!(style_for_status(499)(s), yellow(s));
        assert_eq!(style_for_status(500)(s), red(s));
    }

    // Semantic ports of the lipgloss style assertions: each styled string must
    // contain the original text and be wrapped in an ANSI escape (functional
    // parity rather than exact escape bytes).
    #[test]
    fn test_color_functions_wrap_text() {
        let cases: &[(String, &str)] = &[
            (green("hello"), "hello"),
            (red("hello"), "hello"),
            (yellow("hello"), "hello"),
            (cyan("hello"), "hello"),
            (magenta("hello"), "hello"),
            (dim("hello"), "hello"),
            (bold("hello"), "hello"),
        ];
        for (styled, text) in cases {
            assert!(!styled.is_empty(), "styled output must be non-empty");
            assert!(
                styled.contains(text),
                "styled output {styled:?} must contain {text:?}"
            );
            assert!(
                styled.contains('\u{1b}'),
                "styled output {styled:?} must contain an ANSI escape"
            );
        }
    }

    #[test]
    fn test_marks() {
        let check = check_mark();
        let cross = cross_mark();
        let warn = warn_mark();
        assert!(check.contains('✓'));
        assert!(cross.contains('✗'));
        assert!(warn.contains('!'));
        // Each mark is the corresponding glyph wrapped in its color.
        assert_eq!(check, green("✓"));
        assert_eq!(cross, red("✗"));
        assert_eq!(warn, yellow("!"));
    }
}
