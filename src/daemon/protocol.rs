//! IPC protocol types exchanged over the unix domain socket.
//!
//! Faithful port of `internal/daemon/protocol.go`. The CLI sends a [`Request`]
//! as a single JSON object; the daemon answers with a [`Response`]. Status
//! responses carry a [`StatusData`] payload in the `data` field.

use serde::{Deserialize, Serialize};

/// The kind of IPC message a [`Request`] carries.
///
/// JSON values are the lowercase verbs (`"shutdown"`, `"status"`, `"reload"`),
/// matching the Go `MessageType` string constants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageType {
    /// Ask the daemon to shut down gracefully.
    Shutdown,
    /// Ask the daemon for its current status.
    Status,
    /// Ask the daemon to reload its configuration.
    Reload,
}

/// A request sent from the CLI to the daemon.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    /// The message kind (`json:"type"`).
    #[serde(rename = "type")]
    pub msg_type: MessageType,
    /// Optional opaque payload (`json:"data,omitempty"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// A response returned from the daemon to the CLI.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Response {
    /// Whether the operation succeeded (`json:"ok"`).
    pub ok: bool,
    /// Error message when `ok` is false (`json:"error,omitempty"`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    /// Optional opaque payload (`json:"data,omitempty"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// Status payload returned in a [`Response`] `data` field.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StatusData {
    /// Whether the daemon is running (`json:"running"`).
    pub running: bool,
    /// The daemon's process id (`json:"pid"`).
    pub pid: i32,
    /// The configured domains and their health (`json:"domains"`).
    pub domains: Vec<DomainInfo>,
}

/// Health/info for one configured domain.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DomainInfo {
    /// The domain name (`json:"name"`).
    pub name: String,
    /// The upstream port (`json:"port"`).
    pub port: u16,
    /// Whether the upstream is reachable (`json:"healthy"`).
    pub healthy: bool,
    /// Path routes attached to this domain (`json:"routes,omitempty"`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<RouteInfo>,
}

/// Health/info for one path route.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RouteInfo {
    /// The route path prefix (`json:"path"`).
    pub path: String,
    /// The upstream port (`json:"port"`).
    pub port: u16,
    /// Whether the upstream is reachable (`json:"healthy"`).
    pub healthy: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Port of TestProtocolRoundTripJSON.
    #[test]
    fn protocol_round_trip_json() {
        let req = Request {
            msg_type: MessageType::Reload,
            data: Some(serde_json::json!({ "log_mode": "minimal" })),
        };

        let data = serde_json::to_vec(&req).expect("Marshal Request");

        let got: Request = serde_json::from_slice(&data).expect("Unmarshal Request");

        assert_eq!(got.msg_type, MessageType::Reload, "unexpected request type");
        assert_eq!(
            got.data,
            Some(serde_json::json!({ "log_mode": "minimal" })),
            "unexpected request data"
        );
    }

    // Port of TestStatusDataJSONTags.
    #[test]
    fn status_data_json_tags() {
        let status = StatusData {
            running: true,
            pid: 1234,
            domains: vec![DomainInfo {
                name: "myapp".to_string(),
                port: 3000,
                healthy: true,
                routes: Vec::new(),
            }],
        };

        let data = serde_json::to_vec(&status).expect("Marshal StatusData");

        let decoded: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(&data).expect("Unmarshal StatusData");

        assert!(decoded.contains_key("running"), "expected running key");
        assert!(decoded.contains_key("pid"), "expected pid key");
        assert!(decoded.contains_key("domains"), "expected domains key");
    }

    // Verifies the MessageType verbs serialize to the exact Go string constants.
    #[test]
    fn message_type_lowercase_tags() {
        assert_eq!(
            serde_json::to_string(&MessageType::Shutdown).unwrap(),
            "\"shutdown\""
        );
        assert_eq!(
            serde_json::to_string(&MessageType::Status).unwrap(),
            "\"status\""
        );
        assert_eq!(
            serde_json::to_string(&MessageType::Reload).unwrap(),
            "\"reload\""
        );
    }
}
