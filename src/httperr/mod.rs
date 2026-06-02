//! httperr — HTTP and network error helpers.
//!
//! Faithful port of `internal/httperr` from the Go tool `slim`. Produces the
//! same human-facing error strings so the CLI surfaces identical messages.

mod network;
pub use network::*;

use anyhow::anyhow;

/// JSON shape the API uses for error payloads. Either `error` or `message`
/// (in that precedence) is surfaced.
#[derive(serde::Deserialize, Default)]
struct ApiError {
    #[serde(default)]
    error: String,
    #[serde(default)]
    message: String,
}

/// Build an error from an HTTP response status and (already-read) body bytes.
///
/// Mirrors Go's `FromResponse`, which reads at most 1024 bytes of the body and
/// tries to parse `{"error":...}` / `{"message":...}`; falls back to a status
/// hint. The Go version reads up to 1024 bytes via `io.LimitReader`; we apply
/// the same cap to the bytes the caller passes in.
pub fn from_response_blocking(status: u16, body: &[u8]) -> anyhow::Error {
    // io.ReadAll(io.LimitReader(resp.Body, 1024))
    let limited = &body[..body.len().min(1024)];

    if let Ok(api_err) = serde_json::from_slice::<ApiError>(limited) {
        if !api_err.error.is_empty() {
            return anyhow!("server error: {} (HTTP {})", api_err.error, status);
        }
        if !api_err.message.is_empty() {
            return anyhow!("server error: {} (HTTP {})", api_err.message, status);
        }
    }

    let hint = status_hint(status);
    if !hint.is_empty() {
        return anyhow!("server returned HTTP {} — {}", status, hint);
    }

    anyhow!("server returned HTTP {}", status)
}

/// Map an HTTP status code to a human-friendly hint, or `""` when none applies.
///
/// Exact port of Go's `StatusHint` (with the slim→lane rename in the 404 hint).
pub fn status_hint(code: u16) -> &'static str {
    match code {
        401 => "unauthorized, please try logging in again",
        403 => "access denied",
        404 => "endpoint not found, you may need to update lane",
        429 => "too many requests, please wait a moment and try again",
        500 => "internal server error, please try again later",
        502 | 503 | 521 | 522 | 523 => {
            "the server is temporarily unavailable, please try again later"
        }
        504 | 524 => "the server took too long to respond, please try again later",
        _ => {
            if code >= 500 {
                "server error, please try again later"
            } else {
                ""
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Port of TestFromResponse_JSONErrorField
    #[test]
    fn from_response_json_error_field() {
        let err = from_response_blocking(400, br#"{"error":"invalid email"}"#);
        assert_eq!(err.to_string(), "server error: invalid email (HTTP 400)");
    }

    // Port of TestFromResponse_JSONMessageField
    #[test]
    fn from_response_json_message_field() {
        let err = from_response_blocking(422, br#"{"message":"email is required"}"#);
        assert_eq!(
            err.to_string(),
            "server error: email is required (HTTP 422)"
        );
    }

    // Port of TestFromResponse_StatusHint
    #[test]
    fn from_response_status_hint() {
        let cases: &[(u16, &str)] = &[
            (
                521,
                "server returned HTTP 521 — the server is temporarily unavailable, please try again later",
            ),
            (
                500,
                "server returned HTTP 500 — internal server error, please try again later",
            ),
            (
                429,
                "server returned HTTP 429 — too many requests, please wait a moment and try again",
            ),
            (
                401,
                "server returned HTTP 401 — unauthorized, please try logging in again",
            ),
            (418, "server returned HTTP 418"),
        ];

        for &(code, want) in cases {
            let err = from_response_blocking(code, b"");
            assert_eq!(err.to_string(), want, "code {code}");
        }
    }

    // Port of TestFromResponse_JSONErrorTakesPrecedence
    #[test]
    fn from_response_json_error_takes_precedence() {
        let err = from_response_blocking(500, br#"{"error":"database connection failed"}"#);
        assert_eq!(
            err.to_string(),
            "server error: database connection failed (HTTP 500)"
        );
    }

    // Port of TestStatusHint_5xxFallback
    #[test]
    fn status_hint_5xx_fallback() {
        assert_eq!(status_hint(599), "server error, please try again later");
    }

    // Port of TestStatusHint_4xxNoHint
    #[test]
    fn status_hint_4xx_no_hint() {
        assert_eq!(status_hint(418), "");
    }
}
