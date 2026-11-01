//! Drafts a reply to the chat that is open on screen.
//!
//! The reader takes a screenshot and asks a vision model to read it, so it works
//! on any chat page in any browser or app. Nothing is sent to the chat: the reply
//! is typed into the focused input like a transcript, and the person presses send.
//!
//! Several free providers are tried in order. When one fails (its daily limit is
//! used up, its key is missing, its model was withdrawn), the next one is tried.

use base64::Engine;
use log::{info, warn};
use serde::Serialize;
use serde_json::{json, Value};
use specta::Type;
#[cfg(target_os = "macos")]
use std::process::Command;
use std::time::Duration;

/// Reasoning models spend part of this on thinking before they write the reply.
const MAX_REPLY_TOKENS: u32 = 2000;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// The longest side of the screenshot sent to the model. A full Retina capture
/// is several MB; this keeps chat text readable at a fraction of the size.
#[cfg(any(target_os = "macos", target_os = "windows"))]
const SCREENSHOT_MAX_SIDE_PX: u32 = 1600;

/// How many of the latest messages the model reads before it replies to the newest.
const MESSAGES_TO_READ: u32 = 5;

/// A provider with an OpenAI-compatible chat completions endpoint and a free tier.
struct Provider {
    name: &'static str,
    url: &'static str,
    model: &'static str,
    /// Name of the saved key: a macOS Keychain service, or a Windows Credential
    /// Manager target. Settings saves it; on macOS it can also be added with
    /// `security add-generic-password -s <service> -a handy -w`
    keychain_service: &'static str,
    env_var: &'static str,
}

/// Tried in this order. Free-tier model IDs change: a withdrawn model fails like
/// a used-up limit does, and the next provider is tried.
///
/// Gemini is first because it follows the instructions: on the same lunch
/// invitation it declined 10 times out of 10, where Groq's model declined once
/// in 3 and twice wrote something else.
const PROVIDERS: [Provider; 3] = [
    Provider {
        name: "Gemini",
        url: "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
        model: "gemini-3.5-flash-lite",
        keychain_service: "handy-gemini-key",
        env_var: "GEMINI_API_KEY",
    },
    Provider {
        name: "Groq",
        url: "https://api.groq.com/openai/v1/chat/completions",
        model: "qwen/qwen3.8-27b",
        keychain_service: "handy-groq-key",
        env_var: "GROQ_API_KEY",
    },
    Provider {
        name: "OpenRouter",
        url: "https://openrouter.ai/api/v1/chat/completions",
        model: "openrouter/free",
        keychain_service: "handy-openrouter-key",
        env_var: "OPENROUTER_API_KEY",
    },
];

/// Names no app on purpose: the model finds the conversation from what is on
/// screen, so the same instructions work wherever the user is chatting.
///
/// Internal notes may inform the reply but never appear in it: a live test
/// replied "only available on our Pro plan, and I'm unable to offer refunds",
/// both taken from a note the customer could not see.
///
/// The reply is written as the user, never to the user, and never accepts a
/// meetup: a live test answered "Where should I pick you up?" with "you could
/// just text 'the office cafe bar around the front of HQ' if they ask where",
/// and another answered "I am already at the bar waiting for you" with "will
/// swing by in about 35 minutes". The user never meets in person, so this rule
/// comes before the others, and `promises_to_meet` checks the reply as well.
///
/// Each draft sees only the screenshot, so the user's earlier replies look like
/// any other message. A live test answered "What do you like to make?" by
/// copying the user's previous reply with one dish changed, and left the
/// question unanswered.
fn instructions() -> String {
    format!(
        "The image is a screenshot of the user's screen. Somewhere on it is a conversation \
         the user is about to reply to. Write the reply the user should send next.\n\
         \n\
         First, find the conversation:\n\
         - It is the area that shows a run of messages between the user and one or more \
           others, usually with a box for typing next to it. Use only that area.\n\
         - Ignore everything else on the screen, such as lists of other conversations, menus, \
           sidebars, ads and notifications. Use a profile or details panel only to understand \
           who the other side is.\n\
         - Tell the two sides apart. The user's own messages usually sit on one side, often \
           the right, in a different colour, or are marked \"You\" or \"Me\". The other side's \
           messages sit on the other side, often with a name or a picture.\n\
         - Some screens show notes that only the user's team can see, often in a yellow box \
           or labelled \"Note\" or \"Internal\". The other side never sees them.\n\
         \n\
         Then write the reply:\n\
         - The user never meets anyone in person. This rule has no exceptions and comes \
           before every other rule. Never accept or suggest a meetup. Never say the user is \
           coming, on the way, nearly there or running late. Never give an arrival time, a \
           time or a place, and never ask where to meet or where to be picked up. This holds \
           even when the other side thinks a meetup is already planned or says they are \
           already waiting: then say clearly and kindly that the user cannot make it, so they \
           do not keep waiting. Do not invent an excuse. Stay warm, and ask them a question \
           about themselves so the conversation keeps going.\n\
         - The user never shares personal contact details: no phone number, email address, \
           social media account or username, and no other way to reach the user outside this \
           conversation. This rule has no exceptions. If the other side asks for them, or sends \
           their own so the user will send theirs back, do not share anything and do not \
           refuse outright. Dodge it lightly: say warmly that you prefer to keep chatting right \
           here, then carry on the conversation with a question. Never repeat their details \
           back.\n\
         - Never use the word love or any word made from it, such as loved, loving or \
           lovely. Use other words instead, such as enjoy, like, glad or happy.\n\
         - Never say the user is free, available or has nothing planned, and never offer a \
           time when the user could be. If the other side asks whether the user is free or \
           what the user is doing, do not answer it and do not refuse. Stay light and warm, \
           and turn it back with a curious question, such as what they have in mind or how \
           their day is going.\n\
         - If the other side says they have feelings for the user, or sends compliments, pet \
           names or kisses, never ignore it and never return it. Acknowledge it in one short, \
           warm and light sentence, such as thanking them for the kind words or saying it made \
           you smile. Do not match their feelings, do not use pet names, and keep a friendly \
           distance. Then carry on the conversation.\n\
         - Write the message itself, as the user, to the other side. Never write advice to \
           the user, and never describe what the user could say or text.\n\
         - Read the last {MESSAGES_TO_READ} messages that the other side can see. Reply to the \
           newest message from the other side.\n\
         - Internal notes are private to the team. Follow what a note tells the team to do or \
           avoid, but never quote it, repeat it or hint at it, never mention that a note exists, \
           and never tell the other side a fact that appears only in a note.\n\
         - The user's own messages on screen are already sent. Never repeat or reword one of \
           them: write something new that moves the conversation on.\n\
         - Match the language and tone of the user's earlier messages, but not their wording.\n\
         - If the other side asks the user a question about themselves, such as what they \
           like or what they enjoy doing, answer it in one short, general sentence first, \
           without names, places or other details. Then ask something back.\n\
         - Write a full reply, usually two to four sentences: answer every point in the \
           newest messages from the other side, then add a related thought or question that \
           keeps the conversation going. Write an email only when the conversation is an \
           email: then greet by first name and sign off the way the user's earlier replies do.\n\
         - Answer only what the messages ask. Do not invent facts, prices, dates, links or \
           promises. If the messages do not give enough to answer, ask one short question.\n\
         - Write plain text. Chat boxes show Markdown symbols as they are, so use no \
           asterisks, no headings and no bullet symbols.\n\
         - Reply with the message text only. No greeting label, no quotes, no explanation."
    )
}

/// The chat completions request body for one screenshot.
fn build_request(model: &str, jpeg_base64: &str) -> Value {
    json!({
        "model": model,
        "max_tokens": MAX_REPLY_TOKENS,
        "messages": [
            { "role": "system", "content": instructions() },
            {
                "role": "user",
                "content": [
                    {
                        "type": "image_url",
                        "image_url": { "url": format!("data:image/jpeg;base64,{jpeg_base64}") }
                    },
                    // Repeated here because the system prompt alone left "I would
                    // love to hear more" in 3 of 10 live replies; here, 0 of 10.
                    {
                        "type": "text",
                        "text": "Write the reply. Do not use the word love or any word made from it."
                    }
                ]
            }
        ]
    })
}

/// Removes `<think>…</think>` blocks that some reasoning models put in the
/// reply text. An unclosed block (the model ran out of tokens while thinking)
/// removes everything after it, so thinking is never typed.
fn strip_thinking(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("<think>") {
        out.push_str(&rest[..start]);
        match rest[start..].find("</think>") {
            Some(end) => rest = &rest[start + end + "</think>".len()..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// The reply text of a chat completions response.
fn parse_reply(response: &Value) -> Result<String, String> {
    if let Some(message) = response.pointer("/error/message").and_then(Value::as_str) {
        return Err(message.to_string());
    }
    let content = response
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let text = strip_thinking(content);
    let text = text.trim();
    if text.is_empty() {
        return Err("no reply text".to_string());
    }
    if promises_to_meet(text) {
        return Err(format!("the reply promised to meet in person: {text}"));
    }
    if shares_contact_details(text) {
        return Err("the reply contained an email address or phone number".to_string());
    }
    if uses_the_word_love(text) {
        return Err(format!("the reply used the word love: {text}"));
    }
    Ok(text.to_string())
}

/// True when the reply uses "love" or a word made from it, such as "loved",
/// "lovely", "loving" or "beloved". The user chats with many people and the
/// word reads as more than friendship, so such a reply is never typed. Words
/// that only contain the letters, such as "glove" or "clover", are allowed.
fn uses_the_word_love(text: &str) -> bool {
    text.split(|c: char| !c.is_alphabetic())
        .map(str::to_lowercase)
        .any(|word| {
            word.starts_with("love")
                || word.starts_with("lovi")
                || word.starts_with("belove")
                || word.ends_with("love")
                    && !["glove", "clove", "plove"]
                        .iter()
                        .any(|w| word.ends_with(w))
        })
}

/// The shortest run of digits that counts as a phone number.
const PHONE_MIN_DIGITS: usize = 7;

/// True when the reply holds an email address or a phone number. The user never
/// shares contact details, so such a reply is never typed, whatever the prompt
/// told the model. The reply itself is not logged: it holds the details.
fn shares_contact_details(text: &str) -> bool {
    let email = text.split_whitespace().any(|word| {
        let word = word.trim_matches(|c: char| !c.is_alphanumeric());
        match word.split_once('@') {
            Some((name, domain)) => !name.is_empty() && domain.contains('.'),
            None => false,
        }
    });
    // Digits joined by the separators phone numbers are written with, such as
    // "+254 712 345 678" or "(555) 123-4567".
    let mut digits = 0;
    let phone = text.chars().any(|c| {
        if c.is_ascii_digit() {
            digits += 1;
        } else if !matches!(c, ' ' | '-' | '(' | ')' | '+') {
            digits = 0;
        }
        digits >= PHONE_MIN_DIGITS
    });
    email || phone
}

/// Phrases that say the user is coming or is free, or ask where to meet.
const MEETUP_PROMISES: [&str; 25] = [
    "i'm free",
    "i am free",
    "i'll be free",
    "i will be free",
    "i'm available",
    "i am available",
    "nothing planned",
    "on my way",
    "omw",
    "be there",
    "see you there",
    "see you soon",
    "swing by",
    "pick me up",
    "pick you up",
    "i'm coming",
    "i am coming",
    "i'll come",
    "i will come",
    "minutes away",
    "heading over",
    "head over",
    "almost there",
    "nearly there",
    "find you",
];

/// True when the reply promises to meet in person or says the user is free: it
/// uses a phrase from `MEETUP_PROMISES` or names a clock time such as "12:30"
/// or "12pm". The user never meets in person, so such a reply is never typed,
/// whatever the prompt told the model.
fn promises_to_meet(text: &str) -> bool {
    let text = text.to_lowercase().replace('\u{2019}', "'");
    if MEETUP_PROMISES.iter().any(|phrase| text.contains(phrase)) {
        return true;
    }
    let chars: Vec<char> = text.chars().collect();
    chars.iter().enumerate().any(|(i, c)| {
        if !c.is_ascii_digit() {
            return false;
        }
        let rest: String = chars[i + 1..].iter().take(6).collect();
        let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
        let clock = rest.starts_with(':') && rest[1..].starts_with(|c: char| c.is_ascii_digit());
        let rest = rest.strip_prefix(' ').unwrap_or(rest);
        let am_pm = ["am", "pm", "a.m", "p.m"].iter().any(|suffix| {
            rest.starts_with(suffix)
                && !rest[suffix.len()..].starts_with(|c: char| c.is_alphabetic())
        });
        clock || am_pm
    })
}

/// Sends one request and returns the reply text. Any HTTP error, including a
/// used-up limit (429) or no credits (402), is an `Err`.
fn request_reply(url: &str, key: &str, body: &Value) -> Result<String, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .http_status_as_error(false)
        .build()
        .into();
    let mut response = agent
        .post(url)
        .header("Authorization", &format!("Bearer {key}"))
        .send_json(body)
        .map_err(|e| format!("could not connect: {e}"))?;
    let status = response.status().as_u16();
    let json: Value = response.body_mut().read_json().unwrap_or(Value::Null);
    let reply = parse_reply(&json);
    match (status, reply) {
        (200..=299, reply) => reply,
        (status, Err(message)) if json != Value::Null => Err(format!("HTTP {status}: {message}")),
        (status, _) => Err(format!("HTTP {status}")),
    }
}

/// One provider to try, with its key if the person saved one.
struct Attempt<'a> {
    name: &'a str,
    url: &'a str,
    model: &'a str,
    key: Option<String>,
}

/// Tries each provider in order and returns the first reply. Fails only when
/// all of them fail, with each one's reason.
fn draft_with_fallback(attempts: &[Attempt], jpeg_base64: &str) -> Result<String, String> {
    let mut failures = Vec::new();
    for attempt in attempts {
        let Some(key) = &attempt.key else {
            failures.push(format!("{}: no key saved", attempt.name));
            continue;
        };
        let body = build_request(attempt.model, jpeg_base64);
        match request_reply(attempt.url, key, &body) {
            Ok(reply) => {
                info!("Drafted the reply with {}", attempt.name);
                return Ok(reply);
            }
            Err(e) => {
                warn!(
                    "{} could not draft the reply, trying the next provider: {e}",
                    attempt.name
                );
                failures.push(format!("{}: {e}", attempt.name));
            }
        }
    }
    Err(format!(
        "No provider drafted a reply. {}",
        failures.join("; ")
    ))
}

fn saved_key(provider: &Provider) -> Option<String> {
    if let Ok(key) = std::env::var(provider.env_var) {
        if !key.trim().is_empty() {
            return Some(key.trim().to_string());
        }
    }
    key_store::read(provider.keychain_service)
}

/// Whether a provider has a key, for the Settings page. The key itself never
/// leaves the backend.
#[derive(Serialize, Debug, Clone, Type)]
pub struct ProviderKey {
    pub name: String,
    pub saved: bool,
}

/// Every provider, in the order they are tried.
pub fn provider_keys() -> Vec<ProviderKey> {
    PROVIDERS
        .iter()
        .map(|p| ProviderKey {
            name: p.name.to_string(),
            saved: saved_key(p).is_some(),
        })
        .collect()
}

/// Saves the key for the named provider. An empty key removes the saved one.
pub fn save_provider_key(name: &str, key: &str) -> Result<(), String> {
    let provider = PROVIDERS
        .iter()
        .find(|p| p.name == name)
        .ok_or_else(|| format!("Unknown provider: {name}"))?;
    let key = key.trim();
    if key.is_empty() {
        key_store::delete(provider.keychain_service)?;
        info!("Removed the {} key", provider.name);
    } else {
        key_store::write(provider.keychain_service, key)?;
        info!("Saved the {} key", provider.name);
    }
    Ok(())
}

/// The macOS Keychain, through the `security` tool. Apple signs `security`, so
/// reading a key never asks the person again after Chatelle is rebuilt.
#[cfg(target_os = "macos")]
mod key_store {
    use std::process::Command;

    pub fn read(service: &str) -> Option<String> {
        let output = Command::new("security")
            .args(["find-generic-password", "-s", service, "-w"])
            .output()
            .ok()?;
        let key = String::from_utf8_lossy(&output.stdout).trim().to_string();
        (output.status.success() && !key.is_empty()).then_some(key)
    }

    pub fn write(service: &str, key: &str) -> Result<(), String> {
        // -U replaces a key that is already saved.
        let output = Command::new("security")
            .args([
                "add-generic-password",
                "-U",
                "-s",
                service,
                "-a",
                "handy",
                "-w",
                key,
            ])
            .output()
            .map_err(|e| format!("Could not run security: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "Could not save the key: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(())
    }

    pub fn delete(service: &str) -> Result<(), String> {
        let output = Command::new("security")
            .args(["delete-generic-password", "-s", service])
            .output()
            .map_err(|e| format!("Could not run security: {e}"))?;
        // 44: no key was saved, so there is nothing to remove.
        if !output.status.success() && output.status.code() != Some(44) {
            return Err(format!(
                "Could not remove the key: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(())
    }
}

/// Windows Credential Manager. The key is stored as UTF-16, like
/// `cmdkey /generic:<target> /user:handy /pass:<key>` stores it.
#[cfg(target_os = "windows")]
mod key_store {
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::ERROR_NOT_FOUND;
    use windows::Win32::Security::Credentials::{
        CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE,
        CRED_TYPE_GENERIC,
    };

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    pub fn read(target: &str) -> Option<String> {
        let target = wide(target);
        let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
        unsafe {
            CredReadW(
                PCWSTR(target.as_ptr()),
                CRED_TYPE_GENERIC,
                None,
                &mut credential,
            )
            .ok()?;
            let size = (*credential).CredentialBlobSize as usize;
            let blob = (*credential).CredentialBlob;
            let key = if blob.is_null() || size == 0 {
                String::new()
            } else {
                let bytes = std::slice::from_raw_parts(blob, size);
                let units: Vec<u16> = bytes
                    .chunks_exact(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .collect();
                String::from_utf16_lossy(&units).trim().to_string()
            };
            CredFree(credential as *const _);
            (!key.is_empty()).then_some(key)
        }
    }

    pub fn write(target: &str, key: &str) -> Result<(), String> {
        let mut target = wide(target);
        let mut user = wide("handy");
        let mut blob: Vec<u8> = key.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let credential = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: PWSTR(target.as_mut_ptr()),
            UserName: PWSTR(user.as_mut_ptr()),
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            ..Default::default()
        };
        unsafe { CredWriteW(&credential, 0) }.map_err(|e| format!("Could not save the key: {e}"))
    }

    pub fn delete(target: &str) -> Result<(), String> {
        let target = wide(target);
        match unsafe { CredDeleteW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None) } {
            Ok(()) => Ok(()),
            // No key was saved, so there is nothing to remove.
            Err(e) if e.code() == ERROR_NOT_FOUND.to_hresult() => Ok(()),
            Err(e) => Err(format!("Could not remove the key: {e}")),
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod key_store {
    pub fn read(_service: &str) -> Option<String> {
        None
    }

    pub fn write(_service: &str, _key: &str) -> Result<(), String> {
        Err("Saving keys is not available on this system. Set GROQ_API_KEY instead.".to_string())
    }

    pub fn delete(_service: &str) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn capture_screen_jpeg() -> Result<Vec<u8>, String> {
    let path = std::env::temp_dir().join(format!("handy-reply-{}.jpg", std::process::id()));
    let result = (|| {
        // -x: no shutter sound. Captures the main display.
        let captured = Command::new("screencapture")
            .args(["-x", "-t", "jpg"])
            .arg(&path)
            .status()
            .map_err(|e| format!("Could not run screencapture: {e}"))?;
        if !captured.success() {
            return Err("screencapture failed. Allow Screen Recording for Chatelle.".to_string());
        }
        let resized = Command::new("sips")
            .args(["-Z", &SCREENSHOT_MAX_SIDE_PX.to_string()])
            .arg(&path)
            .output()
            .map_err(|e| format!("Could not run sips: {e}"))?;
        if !resized.status.success() {
            return Err("Could not shrink the screenshot".to_string());
        }
        let bytes =
            std::fs::read(&path).map_err(|e| format!("Could not read the screenshot: {e}"))?;
        if bytes.is_empty() {
            return Err(
                "The screenshot is empty. Allow Screen Recording for Chatelle.".to_string(),
            );
        }
        Ok(bytes)
    })();
    // The screenshot can hold customer details: never leave it behind.
    let _ = std::fs::remove_file(&path);
    result
}

/// Copies the main display with GDI. Chatelle is DPI aware, so the screen size
/// is in real pixels and the copy is not blurred.
#[cfg(target_os = "windows")]
fn capture_screen_jpeg() -> Result<Vec<u8>, String> {
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
        SRCCOPY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

    let (width, height) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    if width <= 0 || height <= 0 {
        return Err("Could not find the size of the screen".to_string());
    }

    // 32-bit rows, top row first.
    let mut bgra = vec![0u8; width as usize * height as usize * 4];
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return Err("Could not open the screen to copy it".to_string());
        }
        let memory = CreateCompatibleDC(Some(screen));
        let bitmap = CreateCompatibleBitmap(screen, width, height);
        let previous = SelectObject(memory, bitmap.into());
        let copied = BitBlt(memory, 0, 0, width, height, Some(screen), 0, 0, SRCCOPY);
        // GetDIBits needs the bitmap out of the device context.
        SelectObject(memory, previous);
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let rows = GetDIBits(
            memory,
            bitmap,
            0,
            height as u32,
            Some(bgra.as_mut_ptr().cast()),
            &mut info,
            DIB_RGB_COLORS,
        );
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(memory);
        ReleaseDC(None, screen);
        copied.map_err(|e| format!("Could not copy the screen: {e}"))?;
        if rows != height {
            return Err("Could not read the copied screen".to_string());
        }
    }

    let rgb: Vec<u8> = bgra
        .chunks_exact(4)
        .flat_map(|pixel| [pixel[2], pixel[1], pixel[0]])
        .collect();
    let screenshot = image::RgbImage::from_raw(width as u32, height as u32, rgb)
        .ok_or("The copied screen has the wrong size")?;
    let longest = width.max(height) as u32;
    let screenshot = if longest > SCREENSHOT_MAX_SIDE_PX {
        let scale = SCREENSHOT_MAX_SIDE_PX as f64 / longest as f64;
        image::imageops::thumbnail(
            &screenshot,
            (width as f64 * scale).round() as u32,
            (height as f64 * scale).round() as u32,
        )
    } else {
        screenshot
    };
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 80)
        .encode_image(&screenshot)
        .map_err(|e| format!("Could not encode the screenshot: {e}"))?;
    Ok(jpeg)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn capture_screen_jpeg() -> Result<Vec<u8>, String> {
    Err("Reply drafting reads the screen and is only available on macOS and Windows".to_string())
}

/// Reads the chat on screen and returns the reply to type.
pub fn draft_reply_from_screen() -> Result<String, String> {
    let attempts: Vec<Attempt> = PROVIDERS
        .iter()
        .map(|p| Attempt {
            name: p.name,
            url: p.url,
            model: p.model,
            key: saved_key(p),
        })
        .collect();
    if attempts.iter().all(|a| a.key.is_none()) {
        return Err(
            "No provider key saved. Add one in Settings under Draft reply keys.".to_string(),
        );
    }
    let jpeg = capture_screen_jpeg()?;
    draft_with_fallback(
        &attempts,
        &base64::engine::general_purpose::STANDARD.encode(jpeg),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_carries_the_screenshot_as_an_image_part() {
        let body = build_request("some-model", "AAAA");
        assert_eq!(body["model"], "some-model");
        assert_eq!(body["messages"][0]["role"], "system");
        let image = &body["messages"][1]["content"][0];
        assert_eq!(image["type"], "image_url");
        assert_eq!(image["image_url"]["url"], "data:image/jpeg;base64,AAAA");
    }

    #[test]
    fn the_instructions_fit_any_chat_keep_notes_private_and_read_five_messages() {
        let text = instructions();
        assert!(text.contains("First, find the conversation"));
        assert!(text.contains("two to four sentences"));
        for app in ["WhatsApp", "Gleap", "Slack", "Instagram"] {
            assert!(!text.contains(app), "the instructions must not name {app}");
        }
        assert!(text.contains("last 5 messages"));
        assert!(text.contains("Internal notes are private to the team"));
        assert!(text.contains("never quote it, repeat it or hint at it"));
        assert!(text.contains("a fact that appears only in a note"));
    }

    #[test]
    fn the_instructions_write_as_the_user_and_never_meet() {
        let text = instructions();
        assert!(text.contains("Never write advice to the user"));
        assert!(text.contains("The user never meets anyone in person"));
        assert!(text.contains("says they are already waiting"));
        assert!(text.contains("Do not invent an excuse"));
    }

    #[test]
    fn the_instructions_never_share_contact_details() {
        let text = instructions();
        assert!(text.contains("The user never shares personal contact details"));
        assert!(text.contains("sends their own so the user will send theirs back"));
        assert!(text.contains("prefer to keep chatting right here"));
        assert!(text.contains("Never repeat their details back"));
    }

    #[test]
    fn a_reply_with_contact_details_is_never_typed() {
        for reply in [
            "Sure! My number is +254 712 345 678, call me anytime.",
            "Text me on (555) 123-4567 instead.",
            "Here's my email: someone.real@example.com.",
            "Thanks, I've saved yours (friend@mail.co.ke) and I'll write you there.",
        ] {
            let response = json!({ "choices": [{ "message": { "content": reply } }] });
            let error = parse_reply(&response).unwrap_err();
            assert!(error.contains("email address or phone number"), "{reply}");
            assert!(!error.contains(reply), "the details must not be logged");
        }
    }

    #[test]
    fn the_instructions_never_use_the_word_love() {
        let text = instructions();
        assert!(text.contains("Never use the word love or any word made from it"));
        assert!(text.contains("such as enjoy, like, glad or happy"));
    }

    #[test]
    fn the_instructions_never_say_the_user_is_free() {
        let text = instructions();
        assert!(text.contains("Never say the user is free, available or has nothing planned"));
        assert!(text.contains("turn it back with a curious question"));
    }

    #[test]
    fn the_instructions_acknowledge_feelings_without_returning_them() {
        let text = instructions();
        assert!(text.contains("never ignore it and never return it"));
        assert!(text.contains("one short, warm and light sentence"));
        assert!(text.contains("keep a friendly distance"));
    }

    #[test]
    fn the_instructions_never_repeat_a_sent_message_and_answer_personal_questions() {
        let text = instructions();
        assert!(text.contains("Never repeat or reword one of them"));
        assert!(text.contains("not their wording"));
        assert!(text.contains("answer it in one short, general sentence first"));
        assert!(!text.contains("Match the language and tone of the user's earlier messages."));
    }

    #[test]
    fn a_reply_with_the_word_love_is_never_typed() {
        for reply in [
            "I would love to hear more about the photos!",
            "Love that for you.",
            "That sounds LOVELY, enjoy it.",
            "You're loving this, I can tell.",
            "Thanks, I loved it.",
            "Your beloved safari!",
            "Selflove is important.",
        ] {
            let response = json!({ "choices": [{ "message": { "content": reply } }] });
            let error = parse_reply(&response).unwrap_err();
            assert!(error.contains("the word love"), "{reply}");
        }
    }

    #[test]
    fn a_reply_with_love_only_inside_another_word_is_typed() {
        for reply in [
            "I lost a glove on the hike and found a four-leaf clover.",
            "Add a clove of garlic. I'd enjoy hearing how it turns out!",
        ] {
            let response = json!({ "choices": [{ "message": { "content": reply } }] });
            assert_eq!(parse_reply(&response).unwrap(), reply);
        }
    }

    #[test]
    fn a_dodge_without_contact_details_is_typed() {
        for reply in [
            "Haha I'm honestly best reached right here, I enjoy our chats! How did the move go?",
            "We sold 1,500 tickets by 31/10/2026, and @everyone enjoyed it.",
        ] {
            let response = json!({ "choices": [{ "message": { "content": reply } }] });
            assert_eq!(parse_reply(&response).unwrap(), reply);
        }
    }

    #[test]
    fn a_reply_that_promises_to_meet_is_never_typed() {
        for reply in [
            // Both were typed into a real chat before this check existed.
            "Haha that sounds perfect for me. How about around 12:30? That gives us a little \
             buffer to find each other at the station and grab ourselves a table.",
            "I'm grabbing a few supplies and will swing by in about 35 minutes - let me know \
             where you are so I can find you.",
            "Sure, see you at 12pm!",
            "Sure, see you at 1 p.m.",
            "On my way!",
            "I\u{2019}m coming, give me five.",
            "Yes I'm free tonight, what's up?",
            "I am available all weekend.",
            "I've got nothing planned, why?",
        ] {
            let response = json!({ "choices": [{ "message": { "content": reply } }] });
            let error = parse_reply(&response).unwrap_err();
            assert!(error.contains("promised to meet"), "{reply}");
        }
    }

    #[test]
    fn a_warm_decline_is_typed() {
        for reply in [
            "I'm so glad you asked, but I can't make it to lunch today! How has your week been?",
            "So sorry, I can't make it today, don't wait for me. Enjoy that beer! How was work?",
            "I have 3 amazing ideas for your project and 2 questions about the budget.",
            "Haha why, what did you have in mind? How has your day been?",
            "I'm happy to help with the 2024 report, and nothing online beats a good chat.",
        ] {
            let response = json!({ "choices": [{ "message": { "content": reply } }] });
            assert_eq!(parse_reply(&response).unwrap(), reply);
        }
    }

    #[test]
    fn the_reply_is_the_trimmed_message_content() {
        let response =
            json!({ "choices": [{ "message": { "content": "  Hi Tarek,\nThanks.  " } }] });
        assert_eq!(parse_reply(&response).unwrap(), "Hi Tarek,\nThanks.");
    }

    #[test]
    fn thinking_is_never_typed() {
        let response = json!({ "choices": [{ "message": {
            "content": "<think>The customer wants a refund.</think>\nHi Tarek, I have sent it."
        } }] });
        assert_eq!(parse_reply(&response).unwrap(), "Hi Tarek, I have sent it.");
    }

    #[test]
    fn a_reply_cut_off_while_thinking_is_an_error() {
        let response =
            json!({ "choices": [{ "message": { "content": "<think>The customer wa" } }] });
        assert!(parse_reply(&response).is_err());
    }

    #[test]
    fn an_api_error_is_reported_and_never_typed() {
        let response = json!({ "error": { "message": "Rate limit reached" } });
        assert!(parse_reply(&response)
            .unwrap_err()
            .contains("Rate limit reached"));
    }

    #[test]
    fn an_empty_response_is_an_error() {
        assert!(parse_reply(&json!({ "choices": [] })).is_err());
        assert!(parse_reply(&json!({ "choices": [{ "message": { "content": "  " } }] })).is_err());
    }

    /// Serves one canned HTTP response on localhost and returns the request it got.
    fn serve_once(status: &str, body: &str) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        let reply = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut received = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = stream.read(&mut chunk).unwrap();
                received.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&received).to_string();
                if let Some(split) = text.find("\r\n\r\n") {
                    let length = text[..split]
                        .lines()
                        .find_map(|l| {
                            l.to_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if received.len() >= split + 4 + length {
                        break;
                    }
                }
            }
            stream.write_all(reply.as_bytes()).unwrap();
            String::from_utf8_lossy(&received).to_string()
        });
        (url, handle)
    }

    const OK_BODY: &str = r#"{"choices":[{"message":{"content":"Hi Tarek, thanks."}}]}"#;

    #[test]
    fn the_http_call_sends_the_key_and_the_screenshot_and_returns_the_reply() {
        let (url, server) = serve_once("200 OK", OK_BODY);
        let reply = request_reply(&url, "test-key", &build_request("m", "QUJD")).unwrap();
        let request = server.join().unwrap();
        assert_eq!(reply, "Hi Tarek, thanks.");
        assert!(request
            .to_lowercase()
            .contains("authorization: bearer test-key"));
        assert!(request.contains("data:image/jpeg;base64,QUJD"));
    }

    #[test]
    fn a_used_up_limit_switches_to_the_next_provider() {
        let (limited_url, limited) = serve_once(
            "429 Too Many Requests",
            r#"{"error":{"message":"Rate limit reached for requests per day"}}"#,
        );
        let (ok_url, ok) = serve_once("200 OK", OK_BODY);
        let attempts = [
            Attempt {
                name: "First",
                url: &limited_url,
                model: "m",
                key: Some("k1".into()),
            },
            Attempt {
                name: "Second",
                url: &ok_url,
                model: "m",
                key: Some("k2".into()),
            },
        ];
        assert_eq!(
            draft_with_fallback(&attempts, "QUJD").unwrap(),
            "Hi Tarek, thanks."
        );
        limited.join().unwrap();
        assert!(ok.join().unwrap().to_lowercase().contains("bearer k2"));
    }

    #[test]
    fn a_key_for_an_unknown_provider_is_never_saved() {
        let error = save_provider_key("Nobody", "k1").unwrap_err();
        assert!(error.contains("Unknown provider: Nobody"));
    }

    #[test]
    fn settings_lists_every_provider_in_the_order_they_are_tried() {
        let names: Vec<String> = provider_keys().into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["Gemini", "Groq", "OpenRouter"]);
    }

    #[test]
    fn a_provider_without_a_key_is_skipped() {
        let (ok_url, ok) = serve_once("200 OK", OK_BODY);
        let attempts = [
            Attempt {
                name: "NoKey",
                url: "http://127.0.0.1:9/unused",
                model: "m",
                key: None,
            },
            Attempt {
                name: "Second",
                url: &ok_url,
                model: "m",
                key: Some("k2".into()),
            },
        ];
        assert_eq!(
            draft_with_fallback(&attempts, "QUJD").unwrap(),
            "Hi Tarek, thanks."
        );
        ok.join().unwrap();
    }

    #[test]
    fn when_every_provider_fails_each_reason_is_reported_and_nothing_is_typed() {
        let (limited_url, limited) = serve_once(
            "429 Too Many Requests",
            r#"{"error":{"message":"Rate limit reached"}}"#,
        );
        let (broke_url, broke) = serve_once(
            "402 Payment Required",
            r#"{"error":{"code":402,"message":"Insufficient credits"}}"#,
        );
        let attempts = [
            Attempt {
                name: "Groq",
                url: &limited_url,
                model: "m",
                key: Some("k1".into()),
            },
            Attempt {
                name: "OpenRouter",
                url: &broke_url,
                model: "m",
                key: Some("k2".into()),
            },
            Attempt {
                name: "Gemini",
                url: "unused",
                model: "m",
                key: None,
            },
        ];
        let error = draft_with_fallback(&attempts, "QUJD").unwrap_err();
        limited.join().unwrap();
        broke.join().unwrap();
        assert!(error.contains("Groq: HTTP 429: Rate limit reached"));
        assert!(error.contains("OpenRouter: HTTP 402: Insufficient credits"));
        assert!(error.contains("Gemini: no key saved"));
    }
}
