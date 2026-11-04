//! Messages between the phone page and the desktop app, and the page itself.
//!
//! Binary WebSocket messages are audio: 16-bit signed little-endian PCM,
//! 16 000 Hz, mono. Text messages are JSON.

use serde::Serialize;

/// What the desktop app is doing. The phone page shows this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PhoneState {
    Idle,
    Recording,
    Transcribing,
}

/// A text message from the phone page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientMessage {
    Toggle,
}

pub fn parse_client_message(text: &str) -> Option<ClientMessage> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    match value.get("type")?.as_str()? {
        "toggle" => Some(ClientMessage::Toggle),
        _ => None,
    }
}

/// The JSON text message that tells the phone page the current state.
pub fn state_message(state: PhoneState) -> String {
    #[derive(Serialize)]
    struct StateMessage {
        r#type: &'static str,
        state: PhoneState,
    }
    serde_json::to_string(&StateMessage {
        r#type: "state",
        state,
    })
    .expect("state message serializes")
}

/// The parts of an HTTP request that the phone server needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestHead {
    pub path: String,
    pub token: Option<String>,
    /// Set only for a WebSocket upgrade request.
    pub websocket_key: Option<String>,
}

pub fn parse_request_head(head: &str) -> Option<RequestHead> {
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split_whitespace();
    if request_line.next()? != "GET" {
        return None;
    }
    let target = request_line.next()?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let token = query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == "t")
        .map(|(_, v)| v.to_string());

    let mut upgrade = false;
    let mut key = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("upgrade") && value.eq_ignore_ascii_case("websocket") {
            upgrade = true;
        } else if name.eq_ignore_ascii_case("sec-websocket-key") {
            key = Some(value.to_string());
        }
    }
    Some(RequestHead {
        path: path.to_string(),
        token,
        websocket_key: if upgrade { key } else { None },
    })
}

/// Compares two tokens in constant time when the lengths are equal.
pub fn token_matches(expected: &str, given: &str) -> bool {
    let (a, b) = (expected.as_bytes(), given.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Makes a JSON string safe inside an HTML `<script>` block.
fn script_safe(json: String) -> String {
    json.replace("</", "<\\/").replace("<!--", "<\\!--")
}

/// The phone page with the token and the UI strings filled in.
pub fn render_page(token: &str) -> String {
    let translations: serde_json::Value = serde_json::from_str(include_str!(
        "../../../src/i18n/locales/en/translation.json"
    ))
    .expect("translation.json is valid JSON");
    let strings = translations["phonePage"].clone();
    let token_json = script_safe(serde_json::Value::String(token.to_string()).to_string());
    include_str!("page.html")
        .replace("__TOKEN__", &token_json)
        .replace("__STRINGS__", &script_safe(strings.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_page_request() {
        let h = parse_request_head("GET /?t=abc HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        assert_eq!(h.path, "/");
        assert_eq!(h.token.as_deref(), Some("abc"));
        assert_eq!(h.websocket_key, None);
    }

    #[test]
    fn parses_websocket_request() {
        let h = parse_request_head(
            "GET /ws?t=abc HTTP/1.1\r\nHost: x\r\nUpgrade: WebSocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
        )
        .unwrap();
        assert_eq!(h.path, "/ws");
        assert_eq!(h.token.as_deref(), Some("abc"));
        assert_eq!(h.websocket_key.as_deref(), Some("dGhlIHNhbXBsZSBub25jZQ=="));
    }

    #[test]
    fn rejects_post() {
        assert!(parse_request_head("POST /?t=abc HTTP/1.1\r\nHost: x\r\n\r\n").is_none());
    }

    #[test]
    fn token_mismatch_and_length() {
        assert!(!token_matches("abcd", "abce"));
        assert!(!token_matches("abcd", "abc"));
        assert!(token_matches("abcd", "abcd"));
    }

    #[test]
    fn toggle_parses() {
        assert_eq!(
            parse_client_message(r#"{"type":"toggle"}"#),
            Some(ClientMessage::Toggle)
        );
    }

    #[test]
    fn unknown_type_is_none() {
        assert_eq!(parse_client_message(r#"{"type":"nope"}"#), None);
        assert_eq!(parse_client_message("not json"), None);
    }

    #[test]
    fn state_message_shape() {
        assert_eq!(
            state_message(PhoneState::Recording),
            r#"{"type":"state","state":"recording"}"#
        );
    }

    #[test]
    fn page_has_token_and_strings() {
        let page = render_page("tok");
        assert!(page.contains("\"tok\""));
        assert!(page.contains("\"Record\""));
        assert!(page.contains("Opened in another tab"));
        assert!(page.contains("Could not start the microphone"));
        // No favicon request, so no rejected request in the log.
        assert!(page.contains(r#"<link rel="icon" href="data:,">"#));
        assert!(!page.contains("__TOKEN__"));
        assert!(!page.contains("__STRINGS__"));
    }

    #[test]
    fn page_escapes_script_close() {
        let page = render_page("</script>");
        assert!(!page.contains("\"</script>\""));
    }
}
