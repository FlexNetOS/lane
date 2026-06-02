//! protocol — tunnel wire format.
//!
//! Faithful port of `protocol/protocol.go`. Provides the registration
//! JSON messages exchanged when a tunnel client connects, the binary frame
//! envelope used to multiplex requests over a single websocket connection,
//! and helpers to (de)serialize raw HTTP/1.x request/response bytes.
//!
//! The Go original leaned on `net/http`'s `httputil.DumpRequest`/`DumpResponse`
//! and `http.ReadRequest`/`http.ReadResponse`. Here we model the same wire
//! semantics with explicit `WireRequest`/`WireResponse` structs so the proxy
//! and tunnel layers can forward bytes without dragging in a full HTTP stack.

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};

/// Registration request sent by the client as a JSON text frame when it
/// connects to the tunnel server. JSON field names match the Go `json` tags.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistrationRequest {
    pub token: String,
    pub subdomain: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub domain: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub password: String,
    #[serde(default, rename = "ttl", skip_serializing_if = "String::is_empty")]
    pub ttl: String,
}

/// Registration response returned by the tunnel server.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistrationResponse {
    pub ok: bool,
    pub url: String,
    pub subdomain: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub domain: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
}

/// A raw HTTP/1.x request decoded from the wire.
///
/// `uri` is the request-target exactly as it appeared on the request line
/// (e.g. `/api/test?q=1`). Header order is preserved.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WireRequest {
    pub method: String,
    pub uri: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// A raw HTTP/1.x response decoded from the wire.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WireResponse {
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// Encode a binary frame: a 4-byte big-endian request id followed by `data`.
pub fn encode_frame(request_id: u32, data: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(4 + data.len());
    frame.extend_from_slice(&request_id.to_be_bytes());
    frame.extend_from_slice(data);
    frame
}

/// Decode a binary frame into its request id and payload.
///
/// Returns an error matching the Go `"frame too short: %d bytes"` text when
/// the frame is shorter than the 4-byte id prefix.
pub fn decode_frame(frame: &[u8]) -> Result<(u32, Vec<u8>)> {
    if frame.len() < 4 {
        bail!("frame too short: {} bytes", frame.len());
    }
    let request_id = u32::from_be_bytes([frame[0], frame[1], frame[2], frame[3]]);
    Ok((request_id, frame[4..].to_vec()))
}

/// Serialize a raw HTTP/1.1 request to bytes (request line + headers + body).
///
/// Mirrors Go's `httputil.DumpRequest(r, true)` wire output closely enough to
/// round-trip through `deserialize_request`. When the body is non-empty and no
/// explicit `Content-Length`/`Transfer-Encoding` header is present, a
/// `Content-Length` header is appended so the body length is unambiguous.
pub fn serialize_request(
    method: &str,
    uri: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(method.as_bytes());
    out.push(b' ');
    out.extend_from_slice(uri.as_bytes());
    out.extend_from_slice(b" HTTP/1.1\r\n");

    let has_framing = headers.iter().any(|(k, _)| {
        k.eq_ignore_ascii_case("content-length") || k.eq_ignore_ascii_case("transfer-encoding")
    });

    for (k, v) in headers {
        out.extend_from_slice(k.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(v.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    if !has_framing && !body.is_empty() {
        out.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(body);
    out
}

/// Parse a raw HTTP/1.x request: request line + headers via `httparse`, then
/// the body, honoring `Content-Length` or `Transfer-Encoding: chunked`.
pub fn deserialize_request(data: &[u8]) -> Result<WireRequest> {
    let mut header_buf = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut header_buf);
    let status = req.parse(data).map_err(|e| anyhow!("parse request: {e}"))?;
    let header_len = match status {
        httparse::Status::Complete(n) => n,
        httparse::Status::Partial => bail!("incomplete HTTP request"),
    };

    let method = req
        .method
        .ok_or_else(|| anyhow!("missing request method"))?
        .to_string();
    let uri = req
        .path
        .ok_or_else(|| anyhow!("missing request target"))?
        .to_string();

    let headers = collect_headers(req.headers);
    let body = read_body(&headers, &data[header_len..])?;

    Ok(WireRequest {
        method,
        uri,
        headers,
        body,
    })
}

/// Serialize an HTTP/1.1 response: status line + headers + CRLF + body.
///
/// Mirrors Go's `httputil.DumpResponse(resp, true)`.
pub fn serialize_response(
    status: u16,
    reason: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(format!("HTTP/1.1 {status} {reason}\r\n").as_bytes());
    for (k, v) in headers {
        out.extend_from_slice(k.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(v.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(body);
    out
}

/// Parse a raw HTTP/1.x response: status line + headers via `httparse`, then
/// the body, honoring `Content-Length` or `Transfer-Encoding: chunked`.
pub fn deserialize_response(data: &[u8]) -> Result<WireResponse> {
    let mut header_buf = [httparse::EMPTY_HEADER; 64];
    let mut resp = httparse::Response::new(&mut header_buf);
    let status = resp
        .parse(data)
        .map_err(|e| anyhow!("parse response: {e}"))?;
    let header_len = match status {
        httparse::Status::Complete(n) => n,
        httparse::Status::Partial => bail!("incomplete HTTP response"),
    };

    let code = resp
        .code
        .ok_or_else(|| anyhow!("missing response status code"))?;
    let reason = resp.reason.unwrap_or("").to_string();

    let headers = collect_headers(resp.headers);
    let body = read_body(&headers, &data[header_len..])?;

    Ok(WireResponse {
        status: code,
        reason,
        headers,
        body,
    })
}

/// Collect parsed `httparse` headers into owned `(name, value)` pairs,
/// preserving order and original header-name casing.
fn collect_headers(headers: &[httparse::Header<'_>]) -> Vec<(String, String)> {
    headers
        .iter()
        .filter(|h| !h.name.is_empty())
        .map(|h| {
            (
                h.name.to_string(),
                String::from_utf8_lossy(h.value).into_owned(),
            )
        })
        .collect()
}

/// Read the message body from the bytes following the header block, honoring
/// `Transfer-Encoding: chunked` (dechunked) or `Content-Length`. With neither,
/// the remainder is taken as-is (the framing the caller chose to send).
fn read_body(headers: &[(String, String)], rest: &[u8]) -> Result<Vec<u8>> {
    if let Some((_, te)) = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("transfer-encoding"))
    {
        if te.to_ascii_lowercase().contains("chunked") {
            return dechunk(rest);
        }
    }

    if let Some((_, cl)) = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
    {
        let n: usize = cl
            .trim()
            .parse()
            .map_err(|_| anyhow!("invalid Content-Length: {cl}"))?;
        if rest.len() < n {
            bail!("body shorter than Content-Length: {} < {}", rest.len(), n);
        }
        return Ok(rest[..n].to_vec());
    }

    Ok(rest.to_vec())
}

/// Decode a `Transfer-Encoding: chunked` body into its underlying bytes.
fn dechunk(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    loop {
        let line_end = find_crlf(&data[pos..]).ok_or_else(|| anyhow!("malformed chunk size"))?;
        let size_line = &data[pos..pos + line_end];
        // A chunk-size line may carry chunk extensions after a ';'.
        let size_str = match size_line.iter().position(|&b| b == b';') {
            Some(i) => &size_line[..i],
            None => size_line,
        };
        let size_str = std::str::from_utf8(size_str)
            .map_err(|_| anyhow!("malformed chunk size"))?
            .trim();
        let size = usize::from_str_radix(size_str, 16)
            .map_err(|_| anyhow!("malformed chunk size: {size_str}"))?;
        pos += line_end + 2; // skip the size line + CRLF

        if size == 0 {
            break;
        }
        if pos + size > data.len() {
            bail!("chunk extends past buffer");
        }
        out.extend_from_slice(&data[pos..pos + size]);
        pos += size;
        // Each chunk's data is followed by a CRLF.
        if data.len() >= pos + 2 && &data[pos..pos + 2] == b"\r\n" {
            pos += 2;
        } else {
            bail!("missing CRLF after chunk data");
        }
    }
    Ok(out)
}

/// Find the index of the next CRLF in `data`, if any.
fn find_crlf(data: &[u8]) -> Option<usize> {
    data.windows(2).position(|w| w == b"\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Port of TestFrameEncodeDecode.
    #[test]
    fn frame_encode_decode() {
        let data = b"hello world";
        let request_id: u32 = 42;

        let frame = encode_frame(request_id, data);
        let (got_id, got_data) = decode_frame(&frame).expect("decode_frame");
        assert_eq!(got_id, request_id, "requestID");
        assert_eq!(got_data, data, "data");
    }

    // Port of TestFrameDecodeError.
    #[test]
    fn frame_decode_error() {
        let err = decode_frame(&[0, 1]).expect_err("expected error for short frame");
        assert!(err.to_string().contains("frame too short: 2 bytes"));
    }

    // Port of TestFrameDecodeEmptyPayload.
    #[test]
    fn frame_decode_empty_payload() {
        let frame = encode_frame(1, &[]);
        let (id, data) = decode_frame(&frame).expect("decode_frame");
        assert_eq!(id, 1, "requestID");
        assert!(
            data.is_empty(),
            "expected empty payload, got {} bytes",
            data.len()
        );
    }

    // Port of TestSerializeDeserializeRequest.
    #[test]
    fn serialize_deserialize_request() {
        let headers = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("X-Custom".to_string(), "value".to_string()),
        ];
        let data = serialize_request("POST", "/api/test?q=1", &headers, b"body");

        let restored = deserialize_request(&data).expect("deserialize_request");

        assert_eq!(restored.method, "POST", "method");
        // uri carries the request-target; path is /api/test, query is q=1.
        let (path, query) = match restored.uri.split_once('?') {
            Some((p, q)) => (p, q),
            None => (restored.uri.as_str(), ""),
        };
        assert_eq!(path, "/api/test", "path");
        assert_eq!(query, "q=1", "query");

        assert_eq!(
            header_value(&restored.headers, "Content-Type"),
            Some("application/json"),
            "Content-Type"
        );
        assert_eq!(
            header_value(&restored.headers, "X-Custom"),
            Some("value"),
            "X-Custom"
        );
        assert_eq!(restored.body, b"body", "body round-trip");
    }

    // Port of TestSerializeDeserializeResponse.
    #[test]
    fn serialize_deserialize_response() {
        let headers = vec![("Content-Type".to_string(), "text/plain".to_string())];
        let data = serialize_response(200, "OK", &headers, &[]);

        let restored = deserialize_response(&data).expect("deserialize_response");

        assert_eq!(restored.status, 200, "status");
        assert_eq!(
            header_value(&restored.headers, "Content-Type"),
            Some("text/plain"),
            "Content-Type"
        );
    }

    #[test]
    fn response_with_body_round_trips() {
        let headers = vec![("Content-Type".to_string(), "text/plain".to_string())];
        let data = serialize_response(404, "Not Found", &headers, b"missing");
        let restored = deserialize_response(&data).expect("deserialize_response");
        assert_eq!(restored.status, 404);
        assert_eq!(restored.reason, "Not Found");
        assert_eq!(restored.body, b"missing");
    }

    #[test]
    fn deserialize_request_dechunks_body() {
        // "Wiki" + "pedia" chunked, terminated by a zero-length chunk.
        let raw = b"POST /upload HTTP/1.1\r\n\
            Transfer-Encoding: chunked\r\n\
            \r\n\
            4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n";
        let req = deserialize_request(raw).expect("deserialize_request");
        assert_eq!(req.body, b"Wikipedia");
    }

    #[test]
    fn deserialize_request_honors_content_length() {
        let raw = b"POST /x HTTP/1.1\r\nContent-Length: 5\r\n\r\nhelloEXTRA";
        let req = deserialize_request(raw).expect("deserialize_request");
        assert_eq!(req.body, b"hello");
    }

    #[test]
    fn registration_request_skips_empty_optionals() {
        let req = RegistrationRequest {
            token: "tok".to_string(),
            subdomain: "sub".to_string(),
            ..Default::default()
        };
        let json = serde_json::to_string(&req).expect("serialize");
        assert!(json.contains("\"token\":\"tok\""));
        assert!(json.contains("\"subdomain\":\"sub\""));
        // NB: check the quoted *key* `"domain"` — a bare `domain` substring also
        // matches inside `"subdomain"`.
        assert!(!json.contains("\"domain\""));
        assert!(!json.contains("\"password\""));
        assert!(!json.contains("\"ttl\""));
    }

    #[test]
    fn registration_request_ttl_uses_lowercase_tag() {
        let req = RegistrationRequest {
            token: "t".to_string(),
            subdomain: "s".to_string(),
            ttl: "30m".to_string(),
            ..Default::default()
        };
        let json = serde_json::to_string(&req).expect("serialize");
        assert!(json.contains("\"ttl\":\"30m\""), "json was {json}");
    }

    #[test]
    fn registration_response_round_trips() {
        let resp = RegistrationResponse {
            ok: true,
            url: "https://x.lane.show".to_string(),
            subdomain: "x".to_string(),
            ..Default::default()
        };
        let json = serde_json::to_string(&resp).expect("serialize");
        let back: RegistrationResponse = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, resp);
        // Optional empty fields are omitted.
        assert!(!json.contains("\"error\""));
        assert!(!json.contains("\"domain\""));
    }

    fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}
