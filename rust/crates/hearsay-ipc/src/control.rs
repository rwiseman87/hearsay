//! NDJSON control-channel codec for the helper<->core IPC.
//!
//! Mirror of the control half of `shared/protocol/ipc.md`, the Swift `HearsayIPC.ControlCodec`,
//! and the Python `hearsay.helper.control`. One UTF-8 JSON object per line, terminated by `\n`:
//! [`Command`] (core -> helper), [`Reply`] (helper -> core, correlated by `id`), and [`Event`]
//! (helper -> core, unsolicited). Keys are serialized in sorted order for a deterministic wire
//! (matches Python `sort_keys` / Swift `.sortedKeys`).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A JSON object payload: a command's `args`, a reply's `result`, or an event's `data`.
pub type JsonObj = Map<String, Value>;

/// A command from the core. `args` defaults to empty when the key is absent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Command {
    pub id: i64,
    pub cmd: String,
    #[serde(default)]
    pub args: JsonObj,
}

/// Error payload carried by a failed [`Reply`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplyError {
    pub code: String,
    pub message: String,
}

/// A reply to a command (correlated by `id`). Exactly one of `result` / `error` is present.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub id: i64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<JsonObj>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ReplyError>,
}

impl Reply {
    /// A success reply carrying `result`.
    pub fn ok(id: i64, result: JsonObj) -> Self {
        Reply {
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    /// A failure reply carrying an error `code` + `message`.
    pub fn fail(id: i64, code: impl Into<String>, message: impl Into<String>) -> Self {
        Reply {
            id,
            ok: false,
            result: None,
            error: Some(ReplyError {
                code: code.into(),
                message: message.into(),
            }),
        }
    }
}

/// An unsolicited event from the helper. `ts` shares the media `host_ts` clock.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub event: String,
    #[serde(default)]
    pub ts: u64,
    #[serde(default)]
    pub data: JsonObj,
}

/// One inbound control line from the helper: a [`Reply`] to a command, or an unsolicited [`Event`].
#[derive(Debug, Clone, PartialEq)]
pub enum Inbound {
    Reply(Reply),
    Event(Event),
}

/// A control line that does not conform to the NDJSON contract.
#[derive(Debug)]
pub enum ControlError {
    /// The line was not valid JSON, or did not match the target message shape.
    Json(serde_json::Error),
    /// A well-formed JSON object that is neither a reply (`id` + `ok`) nor an event (`event`).
    Unrecognized,
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ControlError::Json(e) => write!(f, "invalid control JSON: {e}"),
            ControlError::Unrecognized => {
                write!(f, "unrecognized control line (not a reply or event)")
            }
        }
    }
}

impl std::error::Error for ControlError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ControlError::Json(e) => Some(e),
            ControlError::Unrecognized => None,
        }
    }
}

impl From<serde_json::Error> for ControlError {
    fn from(err: serde_json::Error) -> Self {
        ControlError::Json(err)
    }
}

/// Serialize a control message to one NDJSON line (sorted keys, terminated by `\n`).
///
/// Used for [`Command`], [`Reply`], and [`Event`]. Routing through a [`serde_json::Value`] (whose
/// object is a sorted map) makes the key order deterministic, matching the Python and Swift encoders.
pub fn to_line<T: Serialize>(value: &T) -> Result<Vec<u8>, ControlError> {
    let mut bytes = serde_json::to_vec(&serde_json::to_value(value)?)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Parse one command line (the helper's job; used here by the core's tests and fakes).
pub fn parse_command(line: &[u8]) -> Result<Command, ControlError> {
    Ok(serde_json::from_slice(line)?)
}

/// Parse one inbound line from the helper: a [`Reply`] or an unsolicited [`Event`].
pub fn parse_message(line: &[u8]) -> Result<Inbound, ControlError> {
    let value: Value = serde_json::from_slice(line)?;
    let obj = value.as_object().ok_or(ControlError::Unrecognized)?;
    if obj.contains_key("event") {
        Ok(Inbound::Event(serde_json::from_value(value)?))
    } else if obj.contains_key("id") && obj.contains_key("ok") {
        Ok(Inbound::Reply(serde_json::from_value(value)?))
    } else {
        Err(ControlError::Unrecognized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(value: Value) -> JsonObj {
        value.as_object().unwrap().clone()
    }

    fn line_str(bytes: &[u8]) -> String {
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[test]
    fn command_encodes_sorted_and_roundtrips() {
        let cmd = Command {
            id: 7,
            cmd: "start_capture".to_string(),
            args: obj(json!({"sample_rate": 16000})),
        };
        let line = to_line(&cmd).unwrap();
        assert_eq!(
            line_str(&line),
            "{\"args\":{\"sample_rate\":16000},\"cmd\":\"start_capture\",\"id\":7}\n"
        );
        assert_eq!(parse_command(&line).unwrap(), cmd);
    }

    #[test]
    fn command_args_default_when_absent() {
        let cmd = parse_command(b"{\"id\":1,\"cmd\":\"ping\"}").unwrap();
        assert_eq!(
            cmd,
            Command {
                id: 1,
                cmd: "ping".to_string(),
                args: JsonObj::new(),
            }
        );
    }

    #[test]
    fn reply_ok_omits_error() {
        let reply = Reply::ok(7, obj(json!({"pong": true})));
        assert_eq!(
            line_str(&to_line(&reply).unwrap()),
            "{\"id\":7,\"ok\":true,\"result\":{\"pong\":true}}\n"
        );
    }

    #[test]
    fn reply_fail_omits_result_and_sorts_keys() {
        let reply = Reply::fail(7, "no_permission", "denied");
        assert_eq!(
            line_str(&to_line(&reply).unwrap()),
            "{\"error\":{\"code\":\"no_permission\",\"message\":\"denied\"},\"id\":7,\"ok\":false}\n"
        );
    }

    #[test]
    fn event_encodes_sorted() {
        let event = Event {
            event: "tap_health".to_string(),
            ts: 123,
            data: obj(json!({"state": "ok"})),
        };
        assert_eq!(
            line_str(&to_line(&event).unwrap()),
            "{\"data\":{\"state\":\"ok\"},\"event\":\"tap_health\",\"ts\":123}\n"
        );
    }

    #[test]
    fn parse_message_distinguishes_reply_and_event() {
        let reply_line = to_line(&Reply::ok(3, JsonObj::new())).unwrap();
        assert_eq!(
            parse_message(&reply_line).unwrap(),
            Inbound::Reply(Reply::ok(3, JsonObj::new()))
        );

        let event_line = to_line(&Event {
            event: "hello".to_string(),
            ts: 0,
            data: JsonObj::new(),
        })
        .unwrap();
        assert!(matches!(
            parse_message(&event_line).unwrap(),
            Inbound::Event(_)
        ));
    }

    #[test]
    fn parse_message_rejects_unrecognized_and_non_json() {
        assert!(matches!(
            parse_message(b"{\"foo\":1}"),
            Err(ControlError::Unrecognized)
        ));
        assert!(matches!(
            parse_message(b"not json"),
            Err(ControlError::Json(_))
        ));
    }

    #[test]
    fn event_ts_and_data_default() {
        let event: Event = serde_json::from_slice(b"{\"event\":\"x\"}").unwrap();
        assert_eq!(event.ts, 0);
        assert!(event.data.is_empty());
    }
}
