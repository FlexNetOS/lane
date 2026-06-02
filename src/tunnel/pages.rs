//! Error pages served by the tunnel client.
//!
//! Faithful port of `internal/tunnel/pages.go`. Go embedded `server_down.html`
//! and rendered it with `html/template`. Here the same template is embedded via
//! `include_str!` and the two placeholders are substituted manually, applying
//! the same HTML escaping `html/template` would apply to the (untrusted) error
//! string.

/// The embedded "server unavailable" page. Matches the asset Go embedded.
const SERVER_DOWN_HTML: &str = include_str!("../../assets/server_down.html");

/// Render the 502 "server unavailable" page shown when the local development
/// server behind the tunnel is unreachable.
///
/// Mirrors Go's `serverDownTmpl.Execute` with `serverDownData{Port, Error}`:
/// `{{.Port}}` is replaced with the port, and the `{{if .Error}} {{.Error}}{{end}}`
/// block expands to a leading space plus the (HTML-escaped) error when non-empty,
/// or to nothing when the error is empty.
pub fn render_server_down(port: u16, error: &str) -> String {
    let error_block = if error.is_empty() {
        String::new()
    } else {
        format!(" {}", html_escape(error))
    };

    SERVER_DOWN_HTML
        .replace("{{.Port}}", &port.to_string())
        .replace("{{if .Error}} {{.Error}}{{end}}", &error_block)
}

/// Escape a string for inclusion in HTML text, matching the escaping Go's
/// `html/template` applies to interpolated values (`&`, `<`, `>`, `'`, `"`).
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\'' => out.push_str("&#39;"),
            '"' => out.push_str("&#34;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_port_into_detail_box() {
        let html = render_server_down(3000, "");
        assert!(
            html.contains("The local server on port 3000 is not responding."),
            "port not substituted: {html}"
        );
        // The template placeholder must be gone.
        assert!(!html.contains("{{.Port}}"));
        assert!(!html.contains("{{if .Error}}"));
    }

    #[test]
    fn empty_error_omits_error_block() {
        let html = render_server_down(8080, "");
        // With no error, the detail line ends right after "responding." with no
        // trailing error text (the {{if}} block expands to nothing).
        assert!(html.contains("is not responding.</div>"));
    }

    #[test]
    fn non_empty_error_appends_escaped_message() {
        let html = render_server_down(8080, "dial tcp: connection refused");
        assert!(
            html.contains("is not responding. dial tcp: connection refused"),
            "error not appended: {html}"
        );
    }

    #[test]
    fn error_is_html_escaped() {
        let html = render_server_down(8080, "<script>&\"'");
        assert!(
            html.contains("&lt;script&gt;&amp;&#34;&#39;"),
            "got: {html}"
        );
        assert!(!html.contains("<script>"));
    }
}
