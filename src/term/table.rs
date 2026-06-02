//! Borderless table renderer.
//!
//! Replaces the `charm.land/lipgloss/v2/table` table used by `list`/`domain`
//! in the Go tool. That table was configured with every border disabled and a
//! per-cell `PaddingRight(2)`, with the header row styled `Bold(true).Faint(true)`.
//!
//! This reproduces that layout: columns are left-aligned and padded to the
//! column's maximum *visible* content width plus two trailing spaces. The
//! header cells are wrapped in bold+faint styling.

use super::{bold, dim};

/// Compute the visible (display) width of a string, ignoring ANSI SGR escape
/// sequences. Cells may already contain styled content (e.g. a colored status
/// dot), and lipgloss measures the rendered width, not the escape bytes.
fn visible_width(s: &str) -> usize {
    let mut width = 0usize;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // Skip a CSI escape: ESC '[' params... final byte in '@'..='~'.
            // Consume the leading '[' first — otherwise it is mistaken for the
            // (in-range) terminator and the parameter bytes leak into the count.
            if chars.peek() == Some(&'[') {
                chars.next();
            }
            for next in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&next) {
                    break;
                }
            }
        } else {
            width += 1;
        }
    }
    width
}

/// Render a borderless table. Columns are left-aligned and padded to their
/// maximum visible width plus two trailing spaces; the header row is styled
/// bold + faint. Returns a printable block with no trailing newline.
pub fn render_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let num_cols = headers.len();
    if num_cols == 0 {
        return String::new();
    }

    // Compute the maximum visible width per column across the header and all
    // row cells.
    let mut widths = vec![0usize; num_cols];
    for (i, h) in headers.iter().enumerate() {
        widths[i] = visible_width(h);
    }
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < num_cols {
                let w = visible_width(cell);
                if w > widths[i] {
                    widths[i] = w;
                }
            }
        }
    }

    let mut lines: Vec<String> = Vec::with_capacity(rows.len() + 1);

    // Header row: pad each header to its column width (using visible width so
    // alignment is correct), then style the padded cell bold+faint, and append
    // the two-space right padding.
    let mut header_line = String::new();
    for (i, h) in headers.iter().enumerate() {
        let pad = widths[i].saturating_sub(visible_width(h));
        let padded = format!("{h}{}", " ".repeat(pad));
        // Bold + faint, matching lipgloss `Bold(true).Faint(true)`.
        header_line.push_str(&bold(dim(padded)));
        header_line.push_str("  ");
    }
    lines.push(header_line);

    // Data rows.
    for row in rows {
        let mut line = String::new();
        for (i, width) in widths.iter().enumerate() {
            let cell = row.get(i).map(String::as_str).unwrap_or("");
            let pad = width.saturating_sub(visible_width(cell));
            line.push_str(cell);
            line.push_str(&" ".repeat(pad));
            line.push_str("  ");
        }
        lines.push(line);
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                if chars.peek() == Some(&'[') {
                    chars.next();
                }
                for next in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&next) {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn empty_headers_yields_empty() {
        assert_eq!(render_table(&[], &[]), "");
    }

    #[test]
    fn renders_header_and_rows() {
        let out = render_table(
            &["DOMAIN", "PORT", "STATUS"],
            &[
                vec!["app.test".into(), "3000".into(), "reachable".into()],
                vec!["api.test".into(), "8080".into(), "down".into()],
            ],
        );
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3);

        // Header text is present (after stripping styling escapes).
        let header_plain = strip_ansi(lines[0]);
        assert!(header_plain.contains("DOMAIN"));
        assert!(header_plain.contains("PORT"));
        assert!(header_plain.contains("STATUS"));

        // Header is styled (contains an ANSI escape).
        assert!(lines[0].contains('\u{1b}'));

        // Row content is present and unstyled.
        assert!(lines[1].contains("app.test"));
        assert!(lines[1].contains("3000"));
        assert!(lines[1].contains("reachable"));
    }

    #[test]
    fn columns_are_padded_to_max_visible_width() {
        let out = render_table(
            &["A", "B"],
            &[
                vec!["short".into(), "x".into()],
                vec!["longer-value".into(), "y".into()],
            ],
        );
        let lines: Vec<&str> = out.lines().collect();
        // First column max visible width is len("longer-value") == 12, plus
        // two trailing spaces => the second column starts at the same offset on
        // every row.
        let row1 = strip_ansi(lines[1]);
        let row2 = strip_ansi(lines[2]);
        let idx1 = row1.find('x').unwrap();
        let idx2 = row2.find('y').unwrap();
        assert_eq!(idx1, idx2, "second column must align across rows");
        assert_eq!(idx1, "longer-value".len() + 2);
    }

    #[test]
    fn width_ignores_ansi_escapes_in_cells() {
        // A styled status dot should not throw off column alignment: its
        // visible width is what matters.
        let styled = super::super::green("● reachable");
        let out = render_table(
            &["DOMAIN", "STATUS"],
            &[
                vec!["a".into(), styled.clone()],
                vec!["bb".into(), "down".into()],
            ],
        );
        // Visible width of the styled status is the plain "● reachable".
        assert_eq!(visible_width(&styled), "● reachable".chars().count());
        // Sanity: render produced two data rows + header.
        assert_eq!(out.lines().count(), 3);
    }

    #[test]
    fn no_trailing_newline() {
        let out = render_table(&["X"], &[vec!["1".into()]]);
        assert!(!out.ends_with('\n'));
    }
}
