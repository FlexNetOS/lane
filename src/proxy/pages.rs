//! Error pages served by the proxy.
//!
//! Faithful port of `internal/proxy/pages.go`. Go embedded `upstream_down.html`
//! and rendered it with `html/template`. Here the same template is embedded via
//! `include_str!` and the two placeholders are substituted manually, applying
//! the same HTML escaping `html/template` would apply to the (untrusted) host.

/// The embedded "upstream unavailable" page. Matches the asset Go embedded.
const UPSTREAM_DOWN_HTML: &str = include_str!("../../assets/upstream_down.html");

/// Render the 502 "waiting for server" page shown when the local upstream on
/// `port` for `host` is unreachable.
///
/// Mirrors Go's `upstreamDownTmpl.Execute` with `upstreamDownData{Host, Port}`:
/// `{{.Host}}` is replaced with the (HTML-escaped) host and `{{.Port}}` with the
/// port. The port appears in two places in the template, matching Go.
pub fn render_upstream_down(host: &str, port: u16) -> String {
    UPSTREAM_DOWN_HTML
        .replace("{{.Host}}", &html_escape(host))
        .replace("{{.Port}}", &port.to_string())
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
    fn renders_host_and_port() {
        let html = render_upstream_down("myapp.test", 3000);
        assert!(
            html.contains("Waiting for myapp.test"),
            "host not substituted: {html}"
        );
        assert!(html.contains("port 3000"), "port not substituted: {html}");
        assert!(html.contains("localhost:3000"));
        // The template placeholders must be gone.
        assert!(!html.contains("{{.Host}}"));
        assert!(!html.contains("{{.Port}}"));
    }

    #[test]
    fn escapes_host() {
        let html = render_upstream_down("a<b>&c", 8080);
        assert!(html.contains("a&lt;b&gt;&amp;c"));
        assert!(!html.contains("a<b>&c"));
    }
}
