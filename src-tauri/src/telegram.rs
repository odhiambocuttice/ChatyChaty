//! Telegram Bot client for remote alerts and replies.
//!
//! Enables watching a chat on the computer while away and receiving notifications
//! on Telegram, with the ability to view screenshots and trigger replies.

use log::info;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use specta::Type;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

pub const TELEGRAM_BOT_TOKEN_KEY: &str = "chatelle-telegram-bot-token";
pub const TELEGRAM_CHAT_ID_KEY: &str = "chatelle-telegram-chat-id";

const TELEGRAM_API_BASE: &str = "https://api.telegram.org";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(35);

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct TelegramStatus {
    pub configured: bool,
    pub bot_username: Option<String>,
    pub paired: bool,
    pub paired_chat_id: Option<i64>,
    pub pairing_code: Option<String>,
}

pub struct TelegramState {
    pub bot_token: Mutex<Option<String>>,
    pub paired_chat_id: Mutex<Option<i64>>,
    pub pairing_code: Mutex<String>,
    pub polling_active: AtomicBool,
}

impl TelegramState {
    pub fn new() -> Self {
        let token = crate::key_store::read(TELEGRAM_BOT_TOKEN_KEY);
        let chat_id =
            crate::key_store::read(TELEGRAM_CHAT_ID_KEY).and_then(|s| s.parse::<i64>().ok());
        let code = generate_code();

        Self {
            bot_token: Mutex::new(token),
            paired_chat_id: Mutex::new(chat_id),
            pairing_code: Mutex::new(code),
            polling_active: AtomicBool::new(false),
        }
    }

    pub fn get_status(&self) -> TelegramStatus {
        let token = self.bot_token.lock().unwrap().clone();
        let chat_id = *self.paired_chat_id.lock().unwrap();
        let code = self.pairing_code.lock().unwrap().clone();

        let bot_username = if let Some(ref tok) = token {
            get_bot_username(tok).ok()
        } else {
            None
        };

        TelegramStatus {
            configured: token.is_some(),
            bot_username,
            paired: chat_id.is_some(),
            paired_chat_id: chat_id,
            pairing_code: if chat_id.is_none() { Some(code) } else { None },
        }
    }
}

fn generate_code() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let num: u32 = rng.random_range(100_000..=999_999);
    num.to_string()
}

fn ureq_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .http_status_as_error(false)
        .build()
        .into()
}

pub fn get_bot_username(token: &str) -> Result<String, String> {
    let url = format!("{TELEGRAM_API_BASE}/bot{token}/getMe");
    let mut resp = ureq_agent()
        .get(&url)
        .call()
        .map_err(|e| format!("Could not reach Telegram: {e}"))?;
    let json: Value = resp.body_mut().read_json().map_err(|e| e.to_string())?;
    if json["ok"].as_bool() != Some(true) {
        return Err(json["description"]
            .as_str()
            .unwrap_or("Unknown error")
            .to_string());
    }
    json["result"]["username"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "Bot has no username".to_string())
}

pub fn send_message(
    token: &str,
    chat_id: i64,
    text: &str,
    reply_markup: Option<Value>,
) -> Result<i64, String> {
    let url = format!("{TELEGRAM_API_BASE}/bot{token}/sendMessage");
    let mut body = json!({
        "chat_id": chat_id,
        "text": text,
        "parse_mode": "Markdown",
    });
    if let Some(markup) = reply_markup {
        body["reply_markup"] = markup;
    }

    let mut resp = ureq_agent()
        .post(&url)
        .send_json(body.clone())
        .map_err(|e| format!("Failed to send message: {e}"))?;
    let mut json: Value = resp.body_mut().read_json().map_err(|e| e.to_string())?;

    // If Markdown parsing fails (e.g. unescaped entities in drafted text), retry as plain text
    if json["ok"].as_bool() != Some(true) {
        if let Some(obj) = body.as_object_mut() {
            obj.remove("parse_mode");
        }
        if let Ok(mut retry_resp) = ureq_agent().post(&url).send_json(body) {
            if let Ok(retry_json) = retry_resp.body_mut().read_json::<Value>() {
                if retry_json["ok"].as_bool() == Some(true) {
                    json = retry_json;
                }
            }
        }
    }

    if json["ok"].as_bool() != Some(true) {
        return Err(json["description"]
            .as_str()
            .unwrap_or("Unknown error")
            .to_string());
    }
    json["result"]["message_id"]
        .as_i64()
        .ok_or_else(|| "Missing message_id".to_string())
}

pub fn edit_message_text(
    token: &str,
    chat_id: i64,
    message_id: i64,
    text: &str,
    reply_markup: Option<Value>,
) -> Result<(), String> {
    let url = format!("{TELEGRAM_API_BASE}/bot{token}/editMessageText");
    let mut body = json!({
        "chat_id": chat_id,
        "message_id": message_id,
        "text": text,
        "parse_mode": "Markdown",
    });
    if let Some(markup) = reply_markup {
        body["reply_markup"] = markup;
    }

    let mut resp = ureq_agent()
        .post(&url)
        .send_json(body.clone())
        .map_err(|e| format!("Failed to edit message: {e}"))?;
    let mut json: Value = resp.body_mut().read_json().map_err(|e| e.to_string())?;

    // If Markdown parsing fails, retry as plain text
    if json["ok"].as_bool() != Some(true) {
        if let Some(obj) = body.as_object_mut() {
            obj.remove("parse_mode");
        }
        if let Ok(mut retry_resp) = ureq_agent().post(&url).send_json(body) {
            if let Ok(retry_json) = retry_resp.body_mut().read_json::<Value>() {
                if retry_json["ok"].as_bool() == Some(true) {
                    json = retry_json;
                }
            }
        }
    }

    if json["ok"].as_bool() != Some(true) {
        return Err(json["description"]
            .as_str()
            .unwrap_or("Unknown error")
            .to_string());
    }
    Ok(())
}

pub fn send_photo(
    token: &str,
    chat_id: i64,
    jpeg_bytes: &[u8],
    caption: Option<&str>,
    reply_markup: Option<Value>,
) -> Result<(), String> {
    let url = format!("{TELEGRAM_API_BASE}/bot{token}/sendPhoto");
    let boundary = "---------------------------ChatelleTelegramPhotoBoundary738192";
    let mut body = Vec::new();

    // chat_id field
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"chat_id\"\r\n\r\n{chat_id}\r\n"
        )
        .as_bytes(),
    );

    // caption field if any
    if let Some(cap) = caption {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"caption\"\r\n\r\n{cap}\r\n"
            )
            .as_bytes(),
        );
    }

    // reply_markup if any
    if let Some(markup) = reply_markup {
        let markup_str = serde_json::to_string(&markup).unwrap_or_default();
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"reply_markup\"\r\n\r\n{markup_str}\r\n").as_bytes());
    }

    // photo file field
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"photo\"; filename=\"screen.jpg\"\r\nContent-Type: image/jpeg\r\n\r\n").as_bytes());
    body.extend_from_slice(jpeg_bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let content_type = format!("multipart/form-data; boundary={boundary}");
    let mut resp = ureq_agent()
        .post(&url)
        .header("Content-Type", &content_type)
        .send(&body)
        .map_err(|e| format!("Failed to send photo: {e}"))?;
    let json: Value = resp.body_mut().read_json().map_err(|e| e.to_string())?;
    if json["ok"].as_bool() != Some(true) {
        return Err(json["description"]
            .as_str()
            .unwrap_or("Unknown error")
            .to_string());
    }
    Ok(())
}

pub fn answer_callback_query(
    token: &str,
    query_id: &str,
    text: Option<&str>,
) -> Result<(), String> {
    let url = format!("{TELEGRAM_API_BASE}/bot{token}/answerCallbackQuery");
    let mut body = json!({
        "callback_query_id": query_id,
    });
    if let Some(msg) = text {
        body["text"] = json!(msg);
    }
    let _ = ureq_agent().post(&url).send_json(body);
    Ok(())
}

/// Spawns the Telegram polling worker if a token is present and not already polling.
pub fn start_polling(app: AppHandle) {
    let state = app.state::<Arc<TelegramState>>().inner().clone();
    if state.polling_active.swap(true, Ordering::SeqCst) {
        return; // Already polling
    }

    std::thread::spawn(move || {
        info!("Telegram polling loop started");
        let mut offset: i64 = 0;

        loop {
            let token = {
                let guard = state.bot_token.lock().unwrap();
                guard.clone()
            };

            let Some(tok) = token else {
                std::thread::sleep(Duration::from_secs(2));
                continue;
            };

            let url = format!("{TELEGRAM_API_BASE}/bot{tok}/getUpdates?offset={offset}&timeout=20");
            match ureq_agent().get(&url).call() {
                Ok(mut resp) => {
                    if let Ok(json) = resp.body_mut().read_json::<Value>() {
                        if let Some(updates) = json["result"].as_array() {
                            for update in updates {
                                if let Some(up_id) = update["update_id"].as_i64() {
                                    offset = up_id + 1;
                                }
                                handle_update(&app, &tok, &state, update);
                            }
                        }
                    }
                }
                Err(_e) => {
                    // Timeout or network drop is normal during long polling
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
        }
    });
}

fn handle_update(app: &AppHandle, token: &str, state: &TelegramState, update: &Value) {
    // 1. Check for incoming messages
    if let Some(message) = update.get("message") {
        let from_id = message["from"]["id"].as_i64().unwrap_or(0);
        let chat_id = message["chat"]["id"].as_i64().unwrap_or(0);
        let text = message["text"].as_str().unwrap_or("").trim();

        let paired_id = *state.paired_chat_id.lock().unwrap();

        // Pairing logic
        if paired_id.is_none() {
            let expected_code = state.pairing_code.lock().unwrap().clone();
            let is_match = text == format!("/pair {expected_code}")
                || text == format!("/start {expected_code}")
                || text == expected_code;

            if is_match {
                *state.paired_chat_id.lock().unwrap() = Some(chat_id);
                let _ = crate::key_store::write(TELEGRAM_CHAT_ID_KEY, &chat_id.to_string());
                // The chat ID identifies the Telegram account, so it stays out of the log.
                info!("Telegram bot paired");
                let _ = send_message(
                    token,
                    chat_id,
                    "🎉 *Paired with Chatelle!*\n\nYou'll receive incoming messages and drafted replies here when Watch Mode is active.\n\nCommands:\n• /start — Start watching chat\n• /stop — Stop watching\n• /status — Check current status\n• /screen — View current chat screen",
                    None,
                );
                let _ = app.emit("telegram-paired", true);
            } else {
                let _ = send_message(
                    token,
                    chat_id,
                    &format!("🔒 Send `/pair {expected_code}` to link this chat with Chatelle."),
                    None,
                );
            }
            return;
        }

        // Only accept commands from the paired user
        if Some(chat_id) != paired_id && Some(from_id) != paired_id {
            let _ = send_message(
                token,
                chat_id,
                "⛔ This bot is paired with another account.",
                None,
            );
            return;
        }

        // Handle commands from paired user
        match text {
            "/status" => {
                let is_watching = crate::watch::is_watching();
                let status_msg = if is_watching {
                    let info = crate::watch::watched_info();
                    format!("👀 *Watching:* {}\nWaiting for new messages...", info)
                } else {
                    "💤 *Watch Mode is inactive.*\nPress your Watch shortcut or send /start to begin watching.".to_string()
                };
                let _ = send_message(token, chat_id, &status_msg, None);
            }
            "/start" => {
                let res = crate::watch::start_watch_session(app.clone());
                match res {
                    Ok(info) => {
                        let _ = send_message(
                            token,
                            chat_id,
                            &format!("👀 *Now watching:* {}\nI'll notify you here when a new message arrives.", info),
                            None,
                        );
                    }
                    Err(e) => {
                        let _ = send_message(
                            token,
                            chat_id,
                            &format!("⚠️ Could not start watch: {e}"),
                            None,
                        );
                    }
                }
            }
            "/stop" => {
                crate::watch::stop_watch_session();
                let _ = send_message(token, chat_id, "⏹️ *Watch Mode stopped.*", None);
            }
            "/screen" => {
                if let Ok(jpeg) = crate::watch::capture_watched_window() {
                    let _ = send_photo(token, chat_id, &jpeg, Some("🖥️ Current chat window"), None);
                } else {
                    let _ = send_message(token, chat_id, "⚠️ Could not capture window.", None);
                }
            }
            _ => {
                // Unknown command or edit message text
            }
        }
    }

    // 2. Handle callback buttons (inline keyboards)
    if let Some(query) = update.get("callback_query") {
        let query_id = query["id"].as_str().unwrap_or("");
        let chat_id = query["message"]["chat"]["id"].as_i64().unwrap_or(0);
        let message_id = query["message"]["message_id"].as_i64().unwrap_or(0);
        let data = query["data"].as_str().unwrap_or("");

        let paired_id = *state.paired_chat_id.lock().unwrap();
        if Some(chat_id) != paired_id {
            let _ = answer_callback_query(token, query_id, Some("Unauthorized"));
            return;
        }

        match data {
            "show_screen" => {
                let _ = answer_callback_query(token, query_id, None);
                if let Ok(jpeg) = crate::watch::capture_watched_window() {
                    let _ = send_photo(token, chat_id, &jpeg, Some("🖥️ Chat window capture"), None);
                } else {
                    let _ = send_message(token, chat_id, "⚠️ Could not capture window.", None);
                }
            }
            "ignore" => {
                let _ = answer_callback_query(token, query_id, Some("Ignored"));
                crate::watch::clear_pending_reply();
                let _ = edit_message_text(token, chat_id, message_id, "⚪ _Message ignored_", None);
            }
            "send" => {
                // Phase 2: type and send the draft
                let _ = answer_callback_query(token, query_id, Some("Sending..."));
                let _ = crate::watch::send_pending_reply(app.clone(), message_id);
            }
            _ => {
                let _ = answer_callback_query(token, query_id, None);
            }
        }
    }
}
