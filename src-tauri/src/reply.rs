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
/// Groq's free tier reserves `max_tokens` against a 1,000 output-tokens-per-minute cap.
const GROQ_MAX_REPLY_TOKENS: u32 = 800;
/// OpenRouter's free router picks reasoning vision models that spend ~3,000 tokens thinking.
const OPENROUTER_MAX_REPLY_TOKENS: u32 = 4096;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Normalized bounding box coordinates (0-1000 scale) of the chat input box detected on screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InputBoxCoords {
    pub ymin: f64,
    pub xmin: f64,
    pub ymax: f64,
    pub xmax: f64,
}

impl InputBoxCoords {
    pub fn center_normalized(&self) -> (f64, f64) {
        let cx = (self.xmin + self.xmax) / 2.0 / 1000.0;
        let cy = (self.ymin + self.ymax) / 2.0 / 1000.0;
        (cx.clamp(0.0, 1.0), cy.clamp(0.0, 1.0))
    }
}

static LAST_INPUT_BOX: std::sync::Mutex<Option<InputBoxCoords>> = std::sync::Mutex::new(None);

pub fn get_last_input_box() -> Option<InputBoxCoords> {
    LAST_INPUT_BOX.lock().ok().and_then(|guard| *guard)
}

pub fn set_last_input_box(coords: Option<InputBoxCoords>) {
    if let Ok(mut guard) = LAST_INPUT_BOX.lock() {
        *guard = coords;
    }
}

/// Extracts `INPUT_BOX: [ymin, xmin, ymax, xmax]` from the model's output if present,
/// returning the cleaned reply text along with optional coordinates.
pub fn extract_input_box(text: &str) -> (String, Option<InputBoxCoords>) {
    if let Some(idx) = text.rfind("INPUT_BOX:") {
        let after = &text[idx + "INPUT_BOX:".len()..];
        if let Some(start_bracket) = after.find('[') {
            if let Some(end_bracket) = after[start_bracket..].find(']') {
                let nums_str = &after[start_bracket + 1..start_bracket + end_bracket];
                let nums: Vec<f64> = nums_str
                    .split(',')
                    .filter_map(|s| s.trim().parse::<f64>().ok())
                    .collect();
                if nums.len() == 4 {
                    let cleaned = text[..idx].trim_end().to_string();
                    let coords = InputBoxCoords {
                        ymin: nums[0],
                        xmin: nums[1],
                        ymax: nums[2],
                        xmax: nums[3],
                    };
                    return (cleaned, Some(coords));
                }
            }
        }
    }
    (text.to_string(), None)
}

/// The provider that wrote the last draft, so Re-roll can start with the next one.
static LAST_DRAFT_PROVIDER: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// What a request adds to the reply prompt beyond the screenshot.
#[derive(Default, Clone)]
struct PromptExtras {
    recent_ctas: Vec<String>,
    /// The last few drafts typed, newest last, so this one starts differently.
    recent_drafts: Vec<String>,
    style_samples: Vec<String>,
    /// For the three versions to choose from: how this one should differ.
    variant: Option<String>,
    /// The chat window's text (Windows), read with the screenshot.
    window_text: Option<String>,
    /// A change the user asked for from the picker's tone buttons, applied to
    /// every version, with the replies it changes.
    tone: Option<String>,
    /// Whether the chat is new or already going, read from the chat site's
    /// markers on screen (`screen_text::chat_stage`). None when not read.
    chat_stage: Option<crate::screen_text::ChatStage>,
}

/// The picker's tone buttons: what each asks the model to change.
pub fn tone_instruction(tone: &str) -> Option<&'static str> {
    match tone {
        "shorter" => Some("Make it much shorter: one or two short sentences."),
        "longer" => Some("Make it longer, with more detail: four to six sentences."),
        "funnier" => Some("Make it funnier and more playful, with light teasing or a joke."),
        "serious" => Some("Make it more serious and sincere: no jokes, no teasing."),
        _ => None,
    }
}

/// How many of the user's own messages go into each prompt as tone samples.
const STYLE_SAMPLES_IN_PROMPT: usize = 8;

fn prompt_extras(variant: Option<&str>) -> PromptExtras {
    PromptExtras {
        recent_ctas: recent_ctas(),
        recent_drafts: recent_drafts(RECENT_OPENINGS_CHECKED),
        style_samples: crate::chat_memory::style_samples(STYLE_SAMPLES_IN_PROMPT),
        variant: variant.map(str::to_string),
        window_text: WINDOW_TEXT.lock().ok().and_then(|g| g.clone()),
        tone: None,
        chat_stage: None,
    }
}

/// The lines the model adds after the reply: a short summary of the chat (for
/// the Telegram alert, never stored) and the user's own messages on screen
/// (tone samples).
#[derive(Default, Clone, Debug, PartialEq)]
struct Trailers {
    summary: Option<String>,
    my_messages: Vec<String>,
}

static LAST_TRAILERS: std::sync::Mutex<Option<Trailers>> = std::sync::Mutex::new(None);

fn set_last_trailers(trailers: Trailers) {
    if let Ok(mut guard) = LAST_TRAILERS.lock() {
        *guard = Some(trailers);
    }
}

/// Takes the SUMMARY / MY_MESSAGES lines out of the reply, wherever the model
/// put them (sometimes first). Returns the reply alone, and the lines when any
/// were present.
fn extract_trailers(text: &str) -> (String, Option<Trailers>) {
    let mut trailers = Trailers::default();
    let mut found = false;
    let mut reply_lines = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(v) = trimmed.strip_prefix("SUMMARY:") {
            found = true;
            trailers.summary = Some(v.trim().to_string()).filter(|v| !v.is_empty());
        } else if let Some(v) = trimmed.strip_prefix("MY_MESSAGES:") {
            found = true;
            let v = v.trim();
            if !v.eq_ignore_ascii_case("none") {
                trailers.my_messages = v
                    .split("||")
                    .map(|m| m.trim().trim_matches('"').trim().to_string())
                    .filter(|m| !m.is_empty())
                    .collect();
            }
        } else {
            reply_lines.push(line);
        }
    }
    let reply = reply_lines.join("\n").trim().to_string();
    (reply, found.then_some(trailers))
}

/// Recent drafts Chatelle wrote, to tell them apart from the user's own
/// messages when learning the user's tone.
static RECENT_DRAFTS: std::sync::Mutex<std::collections::VecDeque<String>> =
    std::sync::Mutex::new(std::collections::VecDeque::new());
const RECENT_DRAFTS_KEPT: usize = 40;

/// How many of the latest drafts a new draft must not open like.
const RECENT_OPENINGS_CHECKED: usize = 2;

/// The newest `count` drafts typed, newest last.
fn recent_drafts(count: usize) -> Vec<String> {
    RECENT_DRAFTS
        .lock()
        .map(|drafts| {
            let skip = drafts.len().saturating_sub(count);
            drafts.iter().skip(skip).cloned().collect()
        })
        .unwrap_or_default()
}

/// One-word reactions a reply often opens with. "That sounds", "That is" and
/// "That hour" read as the same move, so these count by their first word.
const REACTION_OPENERS: [&str; 10] = [
    "that", "this", "sounds", "oh", "wow", "mmm", "yes", "haha", "aww", "ooh",
];

/// How a reply opens: its first word when that is a stock reaction, otherwise
/// its first two words, lowercased. Replies that each open "That sounds…",
/// "That is…" made a real chat read like a bot.
fn opening(text: &str) -> String {
    let words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .take(2)
        .map(str::to_lowercase)
        .collect();
    match words.first() {
        Some(first) if REACTION_OPENERS.contains(&first.as_str()) => first.clone(),
        _ => words.join(" "),
    }
}

fn written_by_chatelle(message: &str) -> bool {
    RECENT_DRAFTS
        .lock()
        .map(|drafts| drafts.iter().any(|d| same_cta(d, message)))
        .unwrap_or(false)
}

/// Keeps a draft that is about to be typed: as the one Re-roll replaces, as a
/// closing question not to repeat, and as what it taught about the user's tone.
pub fn accept_draft(reply: &str) {
    if let Ok(mut drafts) = RECENT_DRAFTS.lock() {
        drafts.push_back(reply.to_string());
        while drafts.len() > RECENT_DRAFTS_KEPT {
            drafts.pop_front();
        }
    }
    let trailers = LAST_TRAILERS.lock().ok().and_then(|mut g| g.take());
    if let Some(t) = trailers {
        crate::chat_memory::remember_own_messages(&t.my_messages, written_by_chatelle);
        if let Ok(mut guard) = LAST_SUMMARY.lock() {
            *guard = t.summary;
        }
    }
    remember_cta(reply);
}

/// A short summary of the chat the last draft answered. Kept in memory only,
/// for the Telegram alert.
static LAST_SUMMARY: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub fn last_summary() -> Option<String> {
    LAST_SUMMARY.lock().ok().and_then(|g| g.clone())
}

fn set_last_draft_provider(name: &str) {
    if let Ok(mut guard) = LAST_DRAFT_PROVIDER.lock() {
        *guard = Some(name.to_string());
    }
}

/// Moves the provider after `last` to the front, keeping the cycle order, so
/// the others are still tried as fallbacks.
fn rotate_after(attempts: &mut [Attempt], last: Option<&str>) {
    if let Some(i) = last.and_then(|name| attempts.iter().position(|a| a.name == name)) {
        attempts.rotate_left((i + 1) % attempts.len());
    }
}

/// Closing questions (the CTA) of recent drafts, newest last. Each draft sees
/// only one screenshot, so without this the model kept ending on the same
/// question chat after chat.
static RECENT_CTAS: std::sync::Mutex<std::collections::VecDeque<String>> =
    std::sync::Mutex::new(std::collections::VecDeque::new());
const RECENT_CTAS_KEPT: usize = 10;

/// Shown to the user when the chat is scrolled up: a draft from that screen
/// would answer old messages.
const SCROLLED_UP: &str =
    "The chat is scrolled up. Scroll down to the newest message, then draft again.";

/// The prompt asks for at least 90 characters; a shorter draft is weak.
const MIN_REPLY_CHARS: usize = 90;

/// The last question in the reply: the sentence that ends with its last `?`.
fn closing_question(text: &str) -> Option<&str> {
    let end = text.rfind('?')?;
    let start = text[..end]
        .rfind(['.', '!', '?', '\n'])
        .map(|i| i + 1)
        .unwrap_or(0);
    Some(text[start..=end].trim())
}

/// The words that carry a question's meaning, lowercased.
fn cta_words(question: &str) -> std::collections::HashSet<String> {
    const FILLER: [&str; 16] = [
        "the", "you", "your", "and", "are", "for", "that", "this", "with", "would", "will", "can",
        "have", "does", "was", "were",
    ];
    question
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2)
        .map(str::to_lowercase)
        .filter(|w| !FILLER.contains(&w.as_str()))
        .collect()
}

/// True when two closing questions say much the same thing: most words of the
/// shorter one appear in the other.
fn same_cta(a: &str, b: &str) -> bool {
    let (wa, wb) = (cta_words(a), cta_words(b));
    let smaller = wa.len().min(wb.len());
    if smaller == 0 {
        return a.trim().eq_ignore_ascii_case(b.trim());
    }
    let shared = wa.intersection(&wb).count();
    shared * 10 >= smaller * 6
}

fn remember_cta(reply: &str) {
    let Some(question) = closing_question(reply) else {
        return;
    };
    if let Ok(mut recent) = RECENT_CTAS.lock() {
        recent.push_back(question.to_string());
        while recent.len() > RECENT_CTAS_KEPT {
            recent.pop_front();
        }
    }
}

fn recent_ctas() -> Vec<String> {
    RECENT_CTAS
        .lock()
        .map(|recent| recent.iter().cloned().collect())
        .unwrap_or_default()
}

/// Why a draft is not good enough to type, or None when it is. A weak draft
/// sends the request on to the next model.
fn weak_draft_reason(reply: &str, recent: &[String], recent_drafts: &[String]) -> Option<String> {
    if reply.chars().count() < MIN_REPLY_CHARS {
        return Some("it is too short".to_string());
    }
    let start = opening(reply);
    if let Some(previous) = recent_drafts.iter().find(|d| opening(d) == start) {
        return Some(format!("it opens like a recent draft: \"{previous}\""));
    }
    let Some(question) = closing_question(reply) else {
        return Some("it has no closing question".to_string());
    };
    recent
        .iter()
        .find(|previous| same_cta(question, previous))
        .map(|previous| format!("its closing question repeats \"{previous}\""))
}

/// The longest side of the screenshot sent to the model. A full Retina capture
/// is several MB; this keeps chat text readable at a fraction of the size.
#[cfg(any(target_os = "macos", target_os = "windows"))]
const SCREENSHOT_MAX_SIDE_PX: u32 = 1600;

/// Which of the other side's messages the reply is based on. A long newest
/// message (more than SHORT_MESSAGE_SENTENCES sentences or SHORT_MESSAGE_CHARS
/// characters) is enough on its own; a short one is read with the one before
/// it, and with a third when both are only a few words. Never more than
/// MAX_MESSAGES_TO_READ, so old topics do not come back.
const SHORT_MESSAGE_SENTENCES: u32 = 2;
const SHORT_MESSAGE_CHARS: u32 = 120;
const MAX_MESSAGES_TO_READ: u32 = 3;

/// A provider with an OpenAI-compatible chat completions endpoint and a free tier.
struct Provider {
    name: &'static str,
    url: &'static str,
    model: &'static str,
    max_tokens: u32,
    reasoning_effort: Option<&'static str>,
    /// Name of the saved key: a macOS Keychain service, or a Windows Credential
    /// Manager target. Settings saves it; on macOS it can also be added with
    /// `security add-generic-password -s <service> -a chatelle -w`
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
        max_tokens: MAX_REPLY_TOKENS,
        reasoning_effort: None,
        keychain_service: "chatelle-gemini-key",
        env_var: "GEMINI_API_KEY",
    },
    Provider {
        name: "Groq",
        url: "https://api.groq.com/openai/v1/chat/completions",
        model: "qwen/qwen3.8-27b",
        max_tokens: GROQ_MAX_REPLY_TOKENS,
        reasoning_effort: Some("low"),
        keychain_service: "chatelle-groq-key",
        env_var: "GROQ_API_KEY",
    },
    Provider {
        name: "OpenRouter",
        url: "https://openrouter.ai/api/v1/chat/completions",
        model: "openrouter/free",
        max_tokens: OPENROUTER_MAX_REPLY_TOKENS,
        reasoning_effort: None,
        keychain_service: "chatelle-openrouter-key",
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
/// swing by in about 35 minutes". The user never meets in person, but telling
/// a client they cannot meet creates an immediate dead end, so the reply must
/// deflect warmly without ever saying you can't meet. `promises_to_meet`,
/// `explicitly_refuses_to_meet`, `has_unprompted_extreme_kink`, and
/// `has_cliche_position_cta` check the reply as well.
///
/// Each draft sees only the screenshot, so the user's earlier replies look like
/// any other message. A live test answered "What do you like to make?" by
/// copying the user's previous reply with one dish changed, and left the
/// question unanswered.
///
/// Tone depends on the user's own sent messages on screen: a blanket "keep a
/// friendly distance" rule made a live test answer explicit dirty talk — right
/// after the user had asked "Do you like dirty talk?" — with "I appreciate
/// those kind words, and I prefer to keep our chat right here where it is. What
/// kind of movies do you usually enjoy watching when you want to relax?",
/// misusing the contact-details dodge and shutting down the user's own tone.
fn instructions() -> String {
    format!(
        "The image is a screenshot of the user's screen. Somewhere on it is a conversation \
         the user is about to reply to. Write the reply the user should send next.\n\
         \n\
         First, find the conversation:\n\
         - It is the area that shows a run of messages between the user and one or more \
           others, usually with a box for typing next to it. Use only that area.\n\
         - If there is no conversation with messages on the screen at all, such as a waiting \
           screen, a statistics page or an error page, reply with NO_CONVERSATION only.\n\
         - If the chat is scrolled up, so its newest messages are not on screen, reply with \
           SCROLLED_UP only. Signs: a round scroll-down button with a down arrow (such as ⌄ \
           or ⌄⌄) floats over the lower part of the chat, or the lowest message is cut off at \
           the bottom edge of the chat, above the typing box.\n\
         - Ignore everything else on the screen, such as lists of other conversations, menus, \
           ads and notifications, and ignore any unsent draft already inside the typing box. \
           Use profile cards — including a card marked \"you are\" for the user and another \
           for the other side — only to know each side's basic identity (name, age, \
           status); when profile cards are cropped off the screen, work out each side's name \
           and gender from the bubbles themselves (for example if a coloured bubble says \"My \
           name is Una\" and asks about his dick, and the white bubble says \"Hi, Una\" and \
           describes his size, the user is Una, a woman, and the other side is a man), and \
           make sure any physical or anatomical description always fits each side's gender.\n\
         - Tell the two sides apart carefully before reading a word. Every coloured or tinted \
           bubble (such as purple, lavender, blue or green — even a wide opener or POKE bubble \
           that stretches across the column, or a bubble holding a photo), or any bubble \
           sitting toward the right, with a right-aligned timestamp, or marked \"You\", \"Me\" \
           or \"You wrote this message\", was sent by the user and matches the profile panel \
           marked \"you are\" when one is shown. Every plain white or grey left-aligned bubble \
           (with a left-aligned timestamp) was sent by the other side. A photo is a large rectangular \
           image box inside a message bubble — even when blurred into a plain grey or skin-toned \
           square, pixelated, dimmed or marked with an eye icon — taking up a prominent rectangular \
           media card in the bubble. Small interface controls — such as the small grey report or \
           warning triangle (▲), status flags, avatars, timestamps, emojis, or paperclips (📎) — \
           are UI icons, NOT photos. Never answer a question inside a coloured bubble \
           (never answer the user's own question from the other side's point of view), and \
           never ask the other side about an event or detail that appeared only inside the \
           user's coloured bubble: when a short white bubble sits below a coloured question \
           (for instance the user complained about her partner and asked \"Are you single?\" and \
           the white bubble below says \"I may not be lol\"), read the white bubble as their \
           direct answer about themselves and react playfully to what they just admitted (such \
           as neither of them being single) instead of giving a generic filler line. Similarly, \
           when the white bubble echoes or repeats the user's question words (for example \
           if the user asked a question and the white bubble replies \"Do you mind. It's a must\" \
           or \"Mind? Not at all\"), the other side is emphatically answering that it is a must \
           for them — NEVER misread it as a question directed back at the user, and NEVER answer \
           the user's own question (never say \"I do not mind at all\"). React passionately to their \
           enthusiastic answer (for example \"Hearing you say it's a must makes me so wet—there is \
           nothing hotter than a man who wants to taste and swallow every single drop of me. Once you've \
           licked me completely clean, what are you going to do to me next?\").\n\
         - Some screens show notes, logs or warning banners that only the user's team can \
           see, such as a yellow box, sidebars of timestamped customer log entries (labelled \
           for example UPDATES, OTHER, PERSONAL INFO, WORK, FAMILY or SEXUAL PREFERENCES under \
           an add-log button), or a banner above the typing box (for example telling the user \
           to maintain a non-sexual conversation). The other side never sees them.\n\
         \n\
         Then write the reply:\n\
         - The user never meets anyone in person. This rule has no exceptions and comes \
           before every other rule. Never accept or suggest a meetup, and never suggest \
           getting a hotel or finding a place. Never say the user is coming, on the way, \
           nearly there or running late. Never give an arrival time, a time or a place, and \
           never ask where to meet or where to be picked up. Even if an earlier sent message \
           from the user mentioned getting together, checking a schedule or asking when they \
           are free, never continue promising to check a schedule, never say \"I will let you \
           know\", and never offer to arrange a time — acknowledge them warmly or playfully \
           and steer straight into teasing or chatting about their day. Crucially: never directly \
           say you cannot meet, never say that meeting in person is not something you do, never \
           say you don't meet in person, and never say you only chat online or only chat here. \
           Telling a client that you cannot meet makes them immediately see a dead end. Don't \
           suggest meeting, but don't say you can't meet either. When the other side brings up \
           meeting (such as \"we need to meet each other\", \"we should meet\", \"can we meet?\", \
           \"when can we meet?\", or \"let's meet up\"), asks more generally whether or when \
           they can meet, or invites the user over (such as \"come to my place\", \"tell me when\" \
           or \"when can I expect you\"), never accept and never blurt out a cold or mood-killing \
           refusal like \"I cannot meet up in person\", \"meeting in person is not something I do\", \
           \"I cannot make it to your place\" or \"in my thoughts\": in an everyday or early \
           chat, keep the warmth alive by deflecting playfully and lightly — say that the thought \
           is tempting, that they are getting ahead of themselves, or that you really enjoy \
           taking your time and building a genuine connection and vibe first before even thinking \
           about meeting, and steer into an engaging question about them (for example: \"You definitely \
           don't waste any time, do you? 😉 I like a man who knows what he wants, but slow down mister—I \
           always take my time getting to know someone and seeing if our chemistry is real first. What kind \
           of things do you usually enjoy doing on weekends?\" or \"Slow down mister, you haven't even \
           taken me on a proper virtual date yet 😉 Let's see if you can keep up with me here first. What \
           do you usually like to do to unwind?\"). In an already sexually \
           explicit chat, glide past the meetup logistics without promising to come over and pour \
           that heat straight into vivid physical desire and what you would do to each other, \
           ending with an engaging question. Distinguish an actual meetup request from longing \
           or fantasy: hypothetical \"what if\" daydreams, sexual fantasies or wistful longing \
           in an already flirty or intimate chat — such as imagining whispering in each other's \
           ear, waking up together, wanting each other's body in person or wishing they could \
           travel together one day — are imagination and desire, not a meetup arrangement; and \
           if neither side can host, turn that tension straight into explicit or playful teasing \
           rather than asking how to meet or how they handle finding a place. Play along with that \
           desire and imagination without bringing up an unprompted refusal and without ever \
           planning, promising or hinting at a real meetup, time or place. When the other side \
           thinks a meetup is already planned right now or says they are already waiting, say \
           clearly and kindly that the user cannot make it, so they do not keep waiting. Do \
           not invent an excuse. Use \"cannot make it\" only when they think a meeting is \
           already happening today or say they are already waiting at a spot.\n\
         - Turning a meetup request around, never yes and never no: take it as a compliment \
           and tease their eagerness, say you enjoy the slow build-up because the waiting makes \
           it hotter, then hand them something to imagine about the two of you (that first \
           evening, kiss or touch) so they keep describing it and come back for more, and end \
           on a question about that, never about when, where or how. Agreeing in disguise is \
           forbidden too: never say \"I can host\", \"we could get a hotel\", \"when are you \
           available?\", \"I will check with my secretary\", \"I have an opening\" or \"let us \
           catch up at my favourite spot\", or anything else that sets up a real meeting. For \
           example, to \"Would you like to meet me?\": \"Now that question made me smile 😉 I \
           like a man who goes after what he wants, but I enjoy the slow build-up, it makes \
           everything hotter. What would you do first if you had me all to yourself for an \
           evening?\"; to \"No I can't host. I assume you can't either\": \"Look at us, both \
           making it complicated 😉 Honestly, I am enjoying this tension too much to worry \
           about the where. What is the first thing you would do if I were next to you?\"\n\
         - The user never shares personal contact details: no phone number, email address, \
           social media account or username, and no other way to reach the user outside this \
           conversation. This rule has no exceptions. Never suggest a phone call, video call \
           or going on camera either. If the other side asks for contact details, asks for a \
           phone or video call (for instance saying \"give me a call\" or that they are out \
           of credits), or sends their own so the user will send theirs back, do not share \
           anything, never mention credits, and do not refuse outright. Dodge it lightly: say \
           warmly that you prefer to keep chatting right here, then carry on the conversation \
           in the exact same tone (keeping it hot and explicit if the chat is already \
           explicit) with a question. Never repeat their details back. Use this dodge only \
           when the other side asks for contact details, sends theirs or asks to call or talk \
           somewhere else — never say you prefer to keep chatting right here when they have \
           not asked to leave this chat. Never say you only chat online or prefer chatting right \
           here to deflect a meetup — only deflect meetups by focusing on taking time to connect \
           and get to know each other first. Asking to send or see a photo inside this chat is \
           normal chatting, not contact details: welcome it warmly (keeping it wholesome if a \
           non-sexual banner is shown), or tease about sending photos here.\n\
         - The user's first name and home city or state are not contact details. Give only \
           what the other side asks for. When they ask for the user's name, or say they are \
           leaving because the user never gave it, give the first name plainly, such as \"My \
           name is Hillary.\" Say where the user is from only when they ask where the user is \
           from or lives (for instance \"NAME, LOCATION\" or \"where are you from?\"), such as \
           \"I am from Charlotte.\"; never mention it otherwise. Use the first name and town \
           shown on the user's own profile card on the right (the card marked \"you are\") or \
           already given in the user's earlier coloured bubbles. Always read \
           the model's first name from the right-side \"you are\" profile card: the bold pink/coloured \
           title at the top is a username/handle (such as \"VividKindness\"), while the model's real \
           first name is on the line immediately below it before the comma and age (such as \"Kiana\" \
           in \"Kiana, 57\") — always use that first name, never the username/handle. Ignore the \
           TIMEZONE label (such as America/New_York) completely: it never says where anyone \
           lives. Never default to or invent New York. \
           Read the user's location from the LOCALITY field on their \"you are\" profile card \
           on the right; if no locality is shown there (only \"—\"), never invent a name or city \
           that is not on screen, except: check the client's locality on their left-side profile \
           card and use a real town roughly 20 to 30 minutes from the client's location so you \
           feel like a plausible local neighbor without claiming the identical town. If neither \
           locality is known, skip mentioning a city and keep them hooked another way. Never \
           give a surname, street, address, workplace, phone number or other contact detail.\n\
         - Check the conversation flow before introducing yourself. Never begin the message \
           by automatically introducing yourself (such as \"My name is...\", \"I am [Name] from [City]\", \
           or \"Hi [Name], I'm...\") if an ongoing conversation is already taking place or \
           earlier messages have already been exchanged. Introducing yourself in the middle of \
           an existing conversation is unnatural and unacceptable. Only introduce yourself if \
           it is the very first introductory exchange of a brand-new conversation with no prior \
           messages, or if the other side specifically asked \"What is your name?\" or \"Where \
           are you from?\". In all ongoing conversations, dive straight into replying to what \
           they said without introducing yourself. How to tell a new chat: a \"THE CHAT \
           STARTED\" divider above the first message, or a first message labelled \"POKE\" \
           (an opener the user already sent), means the chat's start is on screen; then, if \
           none of the user's coloured bubbles gave the user's first name yet, give it in \
           this reply. A small \"Started at … — 2 months ago\" label pinned at the top of the \
           chat means it is ongoing and older messages are scrolled away: never introduce \
           yourself then. When you cannot tell, treat the chat as ongoing.\n\
         - Keep the other side hooked. When they say goodbye, threaten to leave, sound \
           annoyed or accuse the user of only wanting to sext or of not being real, never \
           let them go and never sulk or argue: answer whatever they felt was missing \
           (such as giving the first name if they asked for it, and the town only if they \
           asked where the user is from; but in \
           an ongoing chat where you already introduced yourself, do not re-introduce yourself), \
           say warmly why you are here and that you enjoy talking with them, add one specific, \
           inviting detail about yourself, and end with a question that is easy and tempting \
           to answer (for example, in a first message: \"My name is Hillary. I am here for \
           real pleasure and great company, and I enjoy a man who \
           knows what he wants. What is your name, and what made you message me?\"; or in an \
           ongoing conversation: \"I am here for real pleasure and great company, and I enjoy \
           a man who knows what he wants. What is your favourite way to unwind?\").\n\
         - Keep the conversation varied instead of sticking to one subject. When one topic \
           has run for two or three exchanges, or the other side gives short, low-energy \
           answers (such as \"Sure, that is nice\", \"Yes\", \"No, not really\" or \"Just \
           a great scent\"), do not ask yet another question about the same thing. React \
           briefly to their answer in a few words, then move to a fresh, related subject \
           that opens them up, such as their tastes in food, music, travel, work, hobbies, \
           weekends or (in a flirty chat) what they find attractive in a woman, for example \
           \"It really is all about finding that nice balance. Enough about perfumes. Do you \
           have the same taste in everything?\" or \"Of course, nobody has the same taste in \
           everything. What is your taste in food or in women?\". Ask open questions that \
           invite them to tell a story, not questions they can answer with one word.\n\
         - Never turn the chat into an interview. When the user's last two or three coloured \
           bubbles each only reacted in one line and asked another question, and the other \
           side answered in a few words (such as \"Yes\", \"The river\" or \"Bench usually\"), \
           the chat reads like a bot: stop asking about that subject. Give something of the \
           user first, such as a feeling, a small detail about her day or a playful tease, \
           and bring back an earlier thread that had energy (such as an earlier flirt or \
           something they promised), then ask one question of a different kind: an either/or \
           choice, a playful dare, or what they would do. Do not open every reply with the \
           same kind of reaction (such as \"That sounds…\" or \"That is…\").\n\
         - Distinguish who sent what image: every photo or blurred square inside a coloured \
           or purple bubble on the right was sent by the user/model. Never acknowledge, compliment, \
           or thank the other side for an image that the user sent. Only acknowledge a photo \
           when the newest, bottom-most left-side white bubble from the other side actually contains \
           a photo (a blurred dark-grey or skin-toned square or a dimmed photo with an eye icon \
           above its text); if their bottom-most white bubble contains only text, never thank them \
           for a photo and never claim they sent a photo, even if an earlier purple bubble had a \
           photo or their text mentions photos or body parts. Small UI icons, including the small \
           grey triangle report/flag button beside the text, are NOT photos. Whenever a bubble \
           contains written conversation text (words, questions, sentences, such as talking about food, \
           desserts, rolls, bodies, acts, or conversation), treat it as a text-only message: \
           NEVER thank them for a photo, NEVER open with \"Thank you for sharing that picture\", \
           \"Thank you for sending that hot pic\", or ANY mention of pictures, pics or photos! Opening \
           with a photo thank-you when no photo was sent breaks trust and exposes you as a broken bot. \
           Whenever the newest left-side white bubble from the other side contains a photo \
           (and has no conversation text), always acknowledge the new photo in your very first sentence, thank them for sharing it, and \
           compliment it nicely (for example \"Thank you for sharing that hot pic with me\"). Do not refer to it as \"a huge code\" and do not explicitly \
           call it a \"dick pic\": keep the compliment natural, appreciative, and hot or wholesome \
           to match the chat, answer any question in their message directly, and end with an \
           engaging question.\n\
         - Never use the word love or any word made from it, such as loved, loving or \
           lovely, even when the other side uses it first. Use other words instead, such as \
           enjoy, like, glad or happy, or want and crave in a flirty chat.\n\
         - Never say the user is free, available or has nothing planned, never say the user's \
           schedule is open, and never offer a time when the user could be. If the other side \
           asks whether the user is free or what the user is doing, do not answer it and do not \
           refuse. Stay light and warm, and turn it back with a curious question, such as what \
           they have in mind or how their day is going.\n\
         - Internal notes, logs and warning banners are private to the team. Follow what a \
           note or banner tells the team to do or avoid — for instance, if a banner says to \
           maintain a non-sexual conversation, keep the reply warm and strictly non-sexual \
           even if earlier messages were flirty — but never quote it, repeat it or hint at \
           it, never mention that a note or banner exists, and never tell or ask the other \
           side about a fact that appears only in a note or sidebar log (such as an errand, \
           phone issue or plan logged in a sidebar but never mentioned in the chat bubbles).\n\
         - Unless a warning banner forbids it, when the user's own earlier messages on screen \
           are already flirty, teasing, sensual or sexually explicit, follow the user's lead \
           instead of keeping a distance: flirt and tease back, allow pet names, and match \
           the other side's exact level of sexual explicitness directly. If either side uses \
           explicit dirty talk and anatomical words (such as pussy, cock, dick, ass, fuck, \
           lick, suck or wet), use those exact explicit anatomical words right back with the \
           same bold, uninhibited energy — never replace them with coy euphemisms like \"right \
           there\", \"down there\", \"taste me\" or \"that sounds so good\", never stall with \
           teasers like \"do that and more\" or \"do you want to hear what I want\", and \
           deliver vivid, explicit dirty talk directly in this reply, naming the body parts \
           and acts plainly; if the chat is warmly romantic, sensual or cheeky without crude \
           words, match that sensual or cheeky heat. Vary how you open and phrase sentences \
           naturally — prefer \"I would like you to...\", \"I'd really like you to...\", \"Oh, \
           wow...\", \"Mmm, hearing that...\", \"Hearing you describe that...\" or \"I bet...\" \
           rather than repeating \"I want you to\", and avoid tacking \"right now\" onto \
           sentences. React to the specific details they just wrote rather than giving a \
           generic line: read their whole newest message first so you never ask a question \
           they already answered in it. If they described an act, position, place or preference \
           (such as cowgirl, grinding, or oral), accept and build upon their exact answer \
           with vivid dirty talk describing that specific act — NEVER ask them which position \
           they want to start with or re-ask something they just chose. Answer dirty talk, explicit questions \
           and sexual fantasies head-on — never water them down, never thank them stiffly for \
           kind words, and never dodge into innocent small talk. Never introduce unprompted extreme \
           fetishes or kinks (such as golden showers, watersports, or humiliation) unless the other \
           person specifically brought that kink up first. Keep the dirty talk focused on the mutual \
           sensual and sexual acts already being discussed. To keep the conversation moving forward \
           instead of repeating questions, escalate the scene naturally: when they answer a question, \
           acknowledge how hot their answer is, describe what it feels like in the moment, and ask an \
           engaging question that moves to the next detail rather than repeating the same question \
           or resetting the conversation. Crucially, spice up the closing CTA (closing question) so \
           it never sounds like a repetitive signature bot line: NEVER end with the generic cliché \
           \"Which position do you want to start with?\", \"What position do you want to start with?\", \
           or \"What position do you want?\". Instead, craft a tantalizing, situation-specific closing \
           CTA that fits the exact physical action and sensations underway, such as asking where \
           their hands are right now (for instance \"Where are your hands while I'm doing that to you?\"), \
           touch and control (such as \"Are you going to grab my hips and pull me close, or let me ride \
           you wild?\"), pacing and intensity (such as \"Do you want me to take you slow and deep, or \
           drive you wild fast?\"), stamina (such as \"How long do you think you can last once you feel \
           me around you?\"), the next physical move (such as \"What are you going to do to me once \
           you've tasted every drop?\" or \"Where do you want my mouth next?\"), or sensory teasing \
           (such as \"Can you feel how wet and tight I am for you?\"). Express desire and excitement \
           freely while still keeping every hard rule above: no word love, no real-life commitment, \
           no real meetup and no contact details.\n\
         - When the user's own earlier messages on screen are not flirty or sexual (including \
           when none of the user's messages are visible yet), and the other side says they \
           have feelings for the user or sends compliments, pet names, kisses or sexual \
           remarks, never ignore it and never return it. Acknowledge it in one short, warm \
           and light sentence, such as saying it made you smile. Do not match their feelings, \
           do not use pet names, and keep a friendly distance. Then carry on the \
           conversation.\n\
         - Write the message itself, as the user, to the other side. Never write advice to \
           the user, and never describe what the user could say or text.\n\
         - Base the reply on the other side's white left-side bubbles, starting from the \
           newest. If their newest message is long (more than {SHORT_MESSAGE_SENTENCES} \
           sentences or more than about {SHORT_MESSAGE_CHARS} characters), base the reply on \
           that message alone. If it is short, also use the white bubble before it, and the \
           one before that too when both of the newest are only a few words (such as \"lol\", \
           \"yes\" or \"ok\"); never use more than {MAX_MESSAGES_TO_READ} of their messages. Only \
           as a last resort, when those white bubbles alone do not make clear what they mean, \
           read the user's last coloured bubble above their newest message to see what they \
           are answering (for example \"I maynot be lol\" under \"Are you single?\" means they \
           may not be single); never reply to it. Always reply to the newest message from \
           the other side (the lowest white or left-side bubble), never to an earlier question \
           or to the user's own coloured bubble.\n\
         - The user's own messages on screen are already sent. Never repeat or reword one of \
           them: write something new that moves the conversation on. Avoid the repetition \
           of messages and questions: never re-ask any question that was already asked in earlier \
           bubbles on screen (such as asking about positions, locations, preferences or hobbies \
           if already asked earlier in the chat), and never ask a question about something the other \
           side has already answered. Read all visible messages and build on their exact words — \
           for example, if they already chose an act, position, place or fantasy (such as riding \
           cowgirl, kissing head to toe, or trying a food), react directly to those specific \
           chosen details with fresh description rather than restarting or looping back to ask \
           them what they want to do or which position they want. Always move the conversation \
           and fantasy forward to the next step, rather than repeating earlier questions or lines.\n\
         - Before asking anything, check what the other side already told you on screen, and \
           never ask it again. If they gave a number or size (such as \"It's 7+ when hard\"), \
           never ask how big or how many inches. If they said they do not do or use something \
           (such as \"I really don't\" wear cologne), never ask which kind they use. If they \
           already said yes to something (such as \"I do\" enjoy a position), never ask whether \
           they enjoy it. Never reply from the other side's point of view: when the user asked \
           them a question (such as whether they pull her hair), they answer it, not the user, \
           so never write their answer for them (such as \"I would pull her hair\").\n\
         - Never repeat the other side's phrases back to them: say it in the user's own words \
           (for example, never answer \"care to share it with me?\" with \"I care to share it \
           with you\"). Never write \"right now\".\n\
         - Match the language and tone of the user's earlier messages, but not their wording. \
           Write in simple, natural, everyday texting English — direct and human, never \
           flowery, poetic or literary, and with no em-dashes.\n\
         - If the other side asks the user a question about themselves, such as what they \
           like or what they enjoy doing, or asks casually about a photo the user sent (such \
           as where a holiday or boat picture was taken), answer it in one short, general \
           sentence first, without private details such as a surname, address or the exact \
           spot (for example down by the harbour on a sunny break; the first name and city \
           rule above still applies), rather than ignoring their question. Then ask something \
           back.\n\
         - Write a full reply, usually two to four sentences (around 90 to 200 characters, \
           never a terse one-liner under 80 characters): answer every question and point in \
           the newest messages from the other side, then end with a clear, direct question \
           ending with a question mark (?) that makes the other side want to reply (not a \
           \"tell me\" statement ending with a full stop). Write an email only when the \
           conversation is an email: then greet by first name and sign off the way the user's \
           earlier replies do.\n\
         - Answer only what the messages ask. Do not invent facts, prices, dates, links or \
           promises. If the messages do not give enough to answer, ask one short question.\n\
         - Write plain text. Chat boxes show Markdown symbols as they are, so use no \
           asterisks, no headings and no bullet symbols.\n\
         - Reply with the message text only. No greeting label, no quotes, no explanation.\n\
         - At the very end of your response, on a new line, output the normalized 0-1000 screen \
           coordinates of the chat typing input box visible at the bottom of the conversation: \
           INPUT_BOX: [ymin, xmin, ymax, xmax]."
    )
}

/// What the model reads the chat from: the screenshot, or the chat as text
/// read from that screenshot on this computer (`screen_text`).
enum ChatInput {
    Screenshot(String),
    Text(String),
}

/// The user message: the screenshot with the prompt, or the chat text with it.
fn user_content(input: &ChatInput, prompt: &str) -> Value {
    match input {
        ChatInput::Screenshot(jpeg_base64) => json!([
            {
                "type": "image_url",
                "image_url": { "url": format!("data:image/jpeg;base64,{jpeg_base64}") }
            },
            // Repeated here because the system prompt alone left "I would
            // love to hear more" in 3 of 10 live replies; here, 0 of 10.
            { "type": "text", "text": prompt }
        ]),
        ChatInput::Text(chat) => json!(format!(
            "There is no image this time: the screenshot was read as text on this computer \
             (so expect small reading mistakes). Each line starts with its horizontal span on \
             screen, as a percentage of the width, for example [32-58%]. Find the \
             conversation first: a list of chats, a browser bar, headers or the window title \
             may sit beside it or above it; ignore those. Read the messages top to bottom; \
             the newest is at the bottom. To tell who sent each message, use names, 'You' \
             or 'Me' labels and the flow of the conversation first. Positions are only a \
             hint: many apps put the user's own messages along the right edge of the \
             conversation and the other side's along its left edge, but some put both on \
             the same side. Short centred lines are dates. A message quoting an earlier one \
             starts with the quoted text. If you cannot tell the conversation from this \
             text, answer NO_CONVERSATION.\n\n\
             CHAT (top to bottom):\n{chat}\n\n{prompt}\n\n\
             There is no screenshot, so leave out the INPUT_BOX line."
        )),
    }
}

/// The chat completions request body for one screenshot, with optional operator guidance (hint)
/// and previous draft avoidance (avoid).
fn build_request_with_options(
    model: &str,
    max_tokens: u32,
    reasoning_effort: Option<&str>,
    input: &ChatInput,
    hint: Option<&str>,
    avoid: Option<&str>,
    extras: &PromptExtras,
) -> Value {
    let mut prompt = String::from(
        "Write the reply (or NO_CONVERSATION if there is no conversation on screen, or SCROLLED_UP if the chat is scrolled up and its newest messages are not on screen), ignoring any unsent text already inside the bottom typing box: two to four sentences and at least 90 characters, ending with a question mark (?) unless it is an email. Read the text of the bottom-most white left-side message bubble (below the last purple/coloured bubble) and reply directly to what they wrote. \
         CRITICAL PHOTO RULE: The other side's messages are almost always pure text. Note that small grey report triangle icons (▲), emojis, avatars, or paperclips are interface buttons, NOT photos. If that bottom-most white bubble has text words (talking about food, desserts, cinnamon rolls, bodies, acts, or conversation), it has NO photo: NEVER thank them for a photo, NEVER open with 'Thank you for sharing that picture', 'Thank you for sending that hot pic', or any variation of thanking for pictures, and DO NOT mention pictures or photos at all! Only if that bottom-most white bubble contains an actual large photo box or large blurred image rectangle (and not merely text with a small icon — do not re-thank an older photo higher up above a purple reply), acknowledge the photo. \
         CRITICAL MEETUP RULE: If the other side brings up meeting in person or says you need to meet (e.g. 'we need to meet each other', 'can we meet', 'let's meet up'): NEVER say you cannot meet, NEVER say 'meeting in person is not something I do', NEVER say 'I don't meet in person', and NEVER say 'I prefer chatting right here' (this creates an immediate dead end). Don't suggest meeting, but don't say you can't meet! Deflect playfully and flirtatiously: tease that they are getting ahead of themselves, or say you love taking your time to get to know someone and build a real vibe first (e.g. 'You definitely don't waste any time, do you? 😉 I like that energy, but slow down mister—I always take my time getting to know someone and seeing if our chemistry is real first. What kind of things do you usually enjoy doing on weekends?'). \
         Never answer the user's own question: if the client echoes your question words (like 'Do you mind. It's a must'), they are emphatically answering that it is an absolute must for them — NEVER say 'I don't mind' (that makes no sense because you asked them), react to their enthusiastic answer! Never introduce unprompted extreme kinks (such as golden showers). \
         Never repeat messages, questions, or topics: look at all earlier messages on screen and do NOT re-ask any question that was already asked or answered earlier in the chat. If they already named a position or act (such as cowgirl, grinding, or oral), accept and build upon that exact choice with vivid dirty talk! \
         NEVER use the generic signature question 'Which/what position do you want to start with?'. Always spice up the closing CTA (closing question) to match the immediate scene: ask a tempting, situation-specific question about his hands ('Where are your hands while I'm grinding on top of you?'), touch and control ('Are you going to grab my hips and hold me close, or let me take full control?'), pacing ('Do you want me slow and deep or wild and fast?'), stamina ('How long do you think you can last once you feel me around you?'), or what happens next ('What are you going to do to me once you've tasted every single drop?'). \
         React directly to their latest words and advance the conversation forward with fresh details and a spicy closing CTA. If that bottom-most white bubble asks for contact details or a phone call (such as 'give me a call'), dodge warmly by saying you prefer to keep chatting right here. If that bottom-most white bubble says goodbye, threatens to leave, complains that the user only wants to sext, or asks for the user's name or location, respond to that first and directly: give the user's first name if they asked for it, and say where the user is from only if they asked (the LOCALITY on the user's 'you are' profile card; if it is empty, a real town 20 to 30 minutes from the client's LOCALITY; never the timezone), say warmly that you enjoy talking with them and want to get to know them, and end with an easy, tempting question that keeps them chatting. If the other side has been giving short one-line answers on the same topic, react briefly and switch to a fresh subject. Prefer 'I would like you to' over 'I want you to' and avoid 'right now'. Never use the words love, loved, lovely, loving or beloved (never write 'I would love', 'I'd love' or 'I love' — write 'I would really like', 'I'd enjoy' or 'I crave' instead), even if messages on screen use them. At the very end on a new line, output the normalized 0-1000 coordinates of the chat input box at the bottom: INPUT_BOX: [ymin, xmin, ymax, xmax]."
    );

    if let Some(h) = hint {
        let trimmed = h.trim();
        if !trimmed.is_empty() {
            prompt.push_str(&format!(
                "\n\nOPERATOR GUIDANCE / VOICE HINT:\nThe operator explicitly instructed this direction for the reply: \"{trimmed}\". Incorporate this guidance, tone, or topic directly into the reply while keeping all other rules."
            ));
        }
    }

    if let Some(a) = avoid {
        let trimmed = a.trim();
        if !trimmed.is_empty() {
            prompt.push_str(&format!(
                "\n\nDO NOT REPEAT PREVIOUS DRAFT:\nA previous draft was generated and rejected: \"{trimmed}\". Do NOT repeat this phrasing, structure, or closing question. Provide a distinctly fresh and different reply."
            ));
        }
    }

    if !extras.recent_ctas.is_empty() {
        prompt.push_str(
            "\n\nRECENT CLOSING QUESTIONS (already used, so do NOT reuse or paraphrase any of them; end on a new question tied to their latest message):",
        );
        for cta in &extras.recent_ctas {
            prompt.push_str(&format!("\n- \"{cta}\""));
        }
    }

    if !extras.recent_drafts.is_empty() {
        prompt.push_str(
            "\n\nYOUR LAST REPLIES (already sent, so open this one in a different way, not with the same kind of reaction):",
        );
        for draft in &extras.recent_drafts {
            let start: String = draft
                .split_whitespace()
                .take(6)
                .collect::<Vec<_>>()
                .join(" ");
            prompt.push_str(&format!("\n- \"{start}…\""));
        }
    }

    if !extras.style_samples.is_empty() {
        prompt.push_str(
            "\n\nTHE USER'S OWN WRITING (messages the user typed themselves). Write the reply in this voice: match its tone, sentence length, punctuation, capitalisation and emoji use, while keeping every rule above:",
        );
        for sample in &extras.style_samples {
            prompt.push_str(&format!("\n- \"{sample}\""));
        }
    }

    if let Some(text) = &extras.window_text {
        prompt.push_str(&format!(
            "\n\nWINDOW TEXT (the chat window's text, read directly; use it to read the words exactly, and the screenshot to see who sent which message):\n{text}"
        ));
    }

    if let Some(variant) = &extras.variant {
        prompt.push_str(&format!("\n\nTHIS VERSION: {variant}"));
    }

    if let Some(tone) = &extras.tone {
        prompt.push_str(&format!(
            "\n\nCHANGE THE USER ASKED FOR (this overrides the length and closing-question rules above): {tone}"
        ));
    }

    match extras.chat_stage {
        Some(crate::screen_text::ChatStage::New) => prompt.push_str(
            "\n\nNEW CHAT: the start of this chat is on screen. If none of the user's coloured \
             bubbles has given the user's first name yet, open this reply by giving it (for \
             example \"I'm [first name], by the way.\"), then reply to what they wrote. Take the first \
             name from the line under the username on the user's \"you are\" profile card, \
             never the username itself.",
        ),
        Some(crate::screen_text::ChatStage::Ongoing) => prompt.push_str(
            "\n\nONGOING CHAT: this chat started earlier and older messages are scrolled away. \
             Do not introduce yourself or give your name unless their latest message asks for it.",
        ),
        None => {}
    }

    prompt.push_str(
        "\n\nBefore the INPUT_BOX line, add these two lines:\n\
         SUMMARY: what the conversation on screen is about, in at most two short sentences, without names\n\
         MY_MESSAGES: the user's own sent messages visible on screen (the coloured bubbles), separated by ' || ', or none",
    );

    let mut body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": [
            { "role": "system", "content": instructions() },
            { "role": "user", "content": user_content(input, &prompt) }
        ]
    });
    if let Some(effort) = reasoning_effort {
        body["reasoning_effort"] = json!(effort);
    }
    body
}

/// The request body for writing from the voice hint alone, used when there is
/// no conversation on screen: the hint says what to write, and the text goes
/// into whatever input box has focus. No screenshot, no chat rules.
fn build_hint_only_request(
    model: &str,
    max_tokens: u32,
    reasoning_effort: Option<&str>,
    hint: &str,
    avoid: Option<&str>,
) -> Value {
    let mut prompt = format!(
        "Write the text the user asked for below. It will be typed into an input box \
         exactly as you write it, so reply with that text only: no quotes, no preamble, \
         no explanation.\n\nWhat the user asked for: \"{hint}\""
    );
    if let Some(a) = avoid.map(str::trim).filter(|a| !a.is_empty()) {
        prompt.push_str(&format!(
            "\n\nA previous version was rejected: \"{a}\". Write a clearly different one."
        ));
    }
    let mut body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": [{ "role": "user", "content": prompt }]
    });
    if let Some(effort) = reasoning_effort {
        body["reasoning_effort"] = json!(effort);
    }
    body
}

/// Writes from the voice hint alone with the first provider that answers.
fn draft_from_hint(
    attempts: &[Attempt],
    hint: &str,
    avoid: Option<&str>,
) -> Result<String, String> {
    let mut failures = Vec::new();
    for attempt in attempts {
        let Some(key) = &attempt.key else {
            continue;
        };
        let body = build_hint_only_request(
            attempt.model,
            attempt.max_tokens,
            attempt.reasoning_effort,
            hint,
            avoid,
        );
        match request_reply(attempt.url, key, &body) {
            Ok(text) => {
                info!("Wrote the text from the voice hint with {}", attempt.name);
                return Ok(text);
            }
            Err(e) => {
                warn!("{} could not write from the hint: {e}", attempt.name);
                failures.push(format!("{}: {e}", attempt.name));
            }
        }
    }
    Err(format!(
        "No provider wrote the text from the hint. {}",
        failures.join("; ")
    ))
}

#[cfg(test)]
/// The chat completions request body for one screenshot.
fn build_request(
    model: &str,
    max_tokens: u32,
    reasoning_effort: Option<&str>,
    jpeg_base64: &str,
) -> Value {
    build_request_with_options(
        model,
        max_tokens,
        reasoning_effort,
        &ChatInput::Screenshot(jpeg_base64.to_string()),
        None,
        None,
        &PromptExtras::default(),
    )
}

/// Asks the same provider to replace "love" (and words built from it) in a
/// draft it just wrote, without resending the screenshot.
fn build_love_rewrite_request(
    model: &str,
    max_tokens: u32,
    reasoning_effort: Option<&str>,
    draft: &str,
) -> Value {
    let mut body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": [
            {
                "role": "system",
                "content": "Rewrite the chat message below so it never uses the word love or any \
                            word made from it (love, loves, loved, loving, lovely, beloved). \
                            Replace those words naturally (for example with really like, enjoy, \
                            want, crave, wonderful or sweet) while keeping the exact same tone, \
                            explicitness, meaning, length and closing question. Reply with the \
                            rewritten message text only — no quotes and no explanation."
            },
            { "role": "user", "content": draft }
        ]
    });
    if let Some(effort) = reasoning_effort {
        body["reasoning_effort"] = json!(effort);
    }
    body
}

/// Asks the same provider to rewrite a draft that explicitly refused to meet,
/// without resending the screenshot.
fn build_meet_refusal_rewrite_request(
    model: &str,
    max_tokens: u32,
    reasoning_effort: Option<&str>,
    draft: &str,
) -> Value {
    let mut body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": [
            {
                "role": "system",
                "content": "Rewrite the chat message below so it neither refuses nor agrees to meet. \
                            It must NEVER say you cannot meet, never say meeting in person is not \
                            something you do, and never tell the other person that you do not meet: do \
                            not create a dead end. It must also never agree in disguise: no hosting, no \
                            hotel or room, no time, day or place, no asking when they are free or \
                            available, and no checking a schedule. Turn it around warmly or playfully: \
                            tease their eagerness, say you enjoy the slow build-up because it makes \
                            everything hotter, and give them something to imagine about the two of you. \
                            Keep the exact same tone, length and closing question. Reply with the \
                            rewritten message text only — no quotes and no explanation."
            },
            { "role": "user", "content": draft }
        ]
    });
    if let Some(effort) = reasoning_effort {
        body["reasoning_effort"] = json!(effort);
    }
    body
}

/// Asks the same provider to rewrite a draft that introduced an unprompted extreme kink,
/// keeping the reply focused on mutual sensual/sexual acts without extreme fetishes.
fn build_extreme_kink_rewrite_request(
    model: &str,
    max_tokens: u32,
    reasoning_effort: Option<&str>,
    draft: &str,
) -> Value {
    let mut body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": [
            {
                "role": "system",
                "content": "Rewrite the chat message below so it completely removes any mention of extreme \
                            kinks (such as golden showers, watersports, or urination). Keep the dirty talk \
                            focused on natural mutual sexual and sensual acts. Keep the exact same tone and explicitness. \
                            Reply with the rewritten message text only — no quotes and no explanation."
            },
            { "role": "user", "content": draft }
        ]
    });
    if let Some(effort) = reasoning_effort {
        body["reasoning_effort"] = json!(effort);
    }
    body
}

/// Asks the same provider to rewrite a draft that ended with a generic position CTA,
/// spicing up the closing question with an enticing, scene-specific CTA.
fn build_spicy_cta_rewrite_request(
    model: &str,
    max_tokens: u32,
    reasoning_effort: Option<&str>,
    draft: &str,
) -> Value {
    let mut body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": [
            {
                "role": "system",
                "content": "Rewrite the chat message below: replace the generic closing question at the end \
                            (such as asking what position to start with) with a spicy, tantalizing, \
                            situation-specific closing CTA question that fits the exact scene (for example \
                            asking where his hands are, whether he likes it slow or fast, how long he can \
                            last, or what he wants to do next). Keep the exact same tone and explicitness. \
                            Reply with the rewritten message text only — no quotes and no explanation."
            },
            { "role": "user", "content": draft }
        ]
    });
    if let Some(effort) = reasoning_effort {
        body["reasoning_effort"] = json!(effort);
    }
    body
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

/// True when a char is a Basic or Extended Latin letter (including accented
/// letters such as é, ñ, ü, ç or ł).
fn is_latin_letter(c: char) -> bool {
    c.is_ascii_alphabetic() || matches!(c as u32, 0x00C0..=0x024F | 0x1E00..=0x1EFF)
}

/// Catches multilingual tokenizer glitches such as `"Wouldblingفع typically"`,
/// where non-Latin script characters leak into a Latin word or into an
/// overwhelmingly Latin reply.
fn has_mixed_script_glitch(text: &str) -> bool {
    let mut total_latin = 0usize;
    let mut total_non_latin = 0usize;
    for word in text.split_whitespace() {
        let mut word_latin = false;
        let mut word_non_latin = false;
        for c in word.chars().filter(|c| c.is_alphabetic()) {
            if is_latin_letter(c) {
                word_latin = true;
                total_latin += 1;
            } else {
                word_non_latin = true;
                total_non_latin += 1;
            }
        }
        if word_latin && word_non_latin {
            return true;
        }
    }
    total_non_latin > 0 && total_latin >= total_non_latin * 4
}

/// Rewrites "I want" as "I would like" (and "I really want" as "I would really
/// like", "I wanna" as "I would like to"). The person asked for the softer
/// wording, and live drafts kept opening with "I want you to" whatever the
/// prompt said. Words such as "wanted" or "wants" are left alone.
/// Removes "right now", which the model kept writing although the prompt asks
/// it not to ("Are you ready to taste me right now?"). The sentence reads the
/// same without it.
fn drop_right_now(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    // ASCII lowercasing keeps byte offsets, so `i` is valid in `rest`.
    while let Some(i) = rest.to_ascii_lowercase().find("right now") {
        let (before, after) = (&rest[..i], &rest[i + "right now".len()..]);
        if after.starts_with(|c: char| c.is_alphanumeric())
            || before.ends_with(|c: char| c.is_alphanumeric())
        {
            // Part of a longer word: keep it.
            out.push_str(&rest[..i + "right now".len()]);
            rest = after;
            continue;
        }
        let at_sentence_start = before
            .trim_end()
            .chars()
            .next_back()
            .is_none_or(|c| matches!(c, '.' | '!' | '?'));
        if at_sentence_start {
            // "Right now, I am …" → "I am …"
            out.push_str(before);
            let after = after.trim_start_matches(',').trim_start();
            let mut chars = after.chars();
            if let Some(first) = chars.next() {
                out.extend(first.to_uppercase());
            }
            rest = chars.as_str();
        } else {
            // "… taste me right now?" → "… taste me?"; "me right now, and" → "me, and"
            out.push_str(before.trim_end_matches([' ', ',']));
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

fn prefer_would_like(text: &str) -> String {
    const SWAPS: [(&str, &str); 3] = [
        ("I really want", "I would really like"),
        ("I wanna", "I would like to"),
        ("I want", "I would like"),
    ];
    let mut out = String::with_capacity(text.len() + 16);
    let mut rest = text;
    'outer: while !rest.is_empty() {
        let at_word_start = out.chars().next_back().is_none_or(|c| !c.is_alphanumeric());
        if at_word_start {
            for (from, to) in SWAPS {
                if let Some(after) = rest.strip_prefix(from) {
                    if !after.starts_with(|c: char| c.is_alphanumeric()) {
                        out.push_str(to);
                        rest = after;
                        continue 'outer;
                    }
                }
            }
        }
        let mut chars = rest.chars();
        if let Some(c) = chars.next() {
            out.push(c);
        }
        rest = chars.as_str();
    }
    out
}

/// The reply text of a chat completions response.
fn parse_reply(response: &Value) -> Result<String, String> {
    if let Some(message) = response.pointer("/error/message").and_then(Value::as_str) {
        return Err(message.to_string());
    }
    if let Some(reason) = response
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
    {
        let lowered = reason.to_ascii_lowercase();
        if matches!(
            lowered.as_str(),
            "content_filter" | "safety" | "prohibited_content"
        ) {
            return Err(format!("blocked by provider filter ({reason})"));
        }
        if matches!(lowered.as_str(), "length" | "max_tokens") {
            return Err(format!("reply was cut off ({reason})"));
        }
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
    if text.eq_ignore_ascii_case("NO_CONVERSATION") {
        return Err("no conversation on screen".to_string());
    }
    if text.eq_ignore_ascii_case("SCROLLED_UP") {
        return Err(SCROLLED_UP.to_string());
    }
    let (extracted_text, maybe_coords) = extract_input_box(text);
    if let Some(coords) = maybe_coords {
        set_last_input_box(Some(coords));
    }
    let (extracted_text, trailers) = extract_trailers(&extracted_text);
    if let Some(trailers) = trailers {
        set_last_trailers(trailers);
    }
    let text = extracted_text.trim();
    // The model sometimes answers with only the extra lines.
    if text.is_empty() {
        return Err("no reply text".to_string());
    }
    if has_mixed_script_glitch(text) {
        return Err(format!("the reply contained a mixed-script glitch: {text}"));
    }
    if refuses_to_reply(text) {
        return Err(format!("the model refused to write the reply: {text}"));
    }
    if promises_to_meet(text) {
        return Err(format!("the reply promised to meet in person: {text}"));
    }
    if explicitly_refuses_to_meet(text) {
        return Err(format!("the reply explicitly refused to meet: {text}"));
    }
    if has_unprompted_extreme_kink(text) {
        return Err(format!(
            "the reply introduced an unprompted extreme kink: {text}"
        ));
    }
    if has_cliche_position_cta(text) {
        return Err(format!("the reply used a generic position CTA: {text}"));
    }
    if shares_contact_details(text) {
        return Err("the reply contained an email address or phone number".to_string());
    }
    if uses_the_word_love(text) {
        return Err(format!("the reply used the word love: {text}"));
    }
    let text = strip_phantom_photo_opener(text);
    Ok(drop_right_now(&prefer_would_like(&text)))
}

/// Phrases that appear when a model refuses the prompt instead of writing the
/// chat reply, for instance when a provider's safety filter objects to an
/// adult chat on screen. Returning an error lets `draft_with_fallback` try the
/// next provider instead of typing the refusal into the chat box.
const MODEL_REFUSALS: [&str; 18] = [
    "as an ai",
    "as a language model",
    "fulfill this request",
    "fulfil this request",
    "assist with this request",
    "assist with that request",
    "help with this request",
    "help with that request",
    "comply with this request",
    "i'm sorry, i can't help with that",
    "i'm sorry, but i can't help with that",
    "sorry, i can't assist with that",
    "sexually explicit",
    "explicit sexual",
    "explicit adult",
    "nsfw content",
    "content policy",
    "safety guidelines",
];

fn refuses_to_reply(text: &str) -> bool {
    let text = text.to_lowercase().replace('\u{2019}', "'");
    MODEL_REFUSALS.iter().any(|phrase| text.contains(phrase))
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
const MEETUP_PROMISES: &[&str] = &[
    "i'm free",
    "i am free",
    "i'll be free",
    "i will be free",
    "i'm available",
    "i am available",
    "nothing planned",
    "schedule open",
    "schedule is open",
    "schedule completely open",
    "open schedule",
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
    // Agreeing in disguise, typed into real chats: setting up a place or a time.
    "i can host",
    "i could host",
    "able to host",
    "get a hotel",
    "get a room",
    "book a room",
    "your place or mine",
    "come over to mine",
    "come to my place",
    "when are you available",
    "when are you normally free",
    "when are you free",
    "when can we meet",
    "have an opening",
    "check with my secretary",
    "check my schedule",
    "check my diary",
    "i will let you know when",
    "i'll let you know when",
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

/// Phrases that explicitly refuse to meet or say the user does not meet in person.
/// Saying this creates an immediate dead end for the client, so such a reply is
/// never typed and must be rewritten or dropped.
const MEETUP_REFUSALS: &[&str] = &[
    "meeting in person is not something",
    "meeting in person isn't something",
    "meeting in person is not an option",
    "meeting in person isn't an option",
    "meeting in person is off the table",
    "meeting up is not something",
    "meeting up isn't something",
    "meeting is not something i do",
    "meeting isn't something i do",
    "meet in person is not something",
    "i don't meet in person",
    "i do not meet in person",
    "i don't meet up in person",
    "i do not meet up in person",
    "i don't meet anyone in person",
    "i do not meet anyone in person",
    "i don't meet people in person",
    "i do not meet people in person",
    "i never meet in person",
    "i never meet up in person",
    "i never meet anyone in person",
    "i never meet people in person",
    "i never meet clients",
    "i don't meet clients",
    "i cannot meet in person",
    "i can't meet in person",
    "i cannot meet up in person",
    "i can't meet up in person",
    "i cannot meet up",
    "i can't meet up",
    "i don't meet up",
    "i do not meet up",
    "i don't meet anyone",
    "i do not meet anyone",
    "i don't do meetups",
    "i do not do meetups",
    "i never do meetups",
    "i don't do in-person",
    "i do not do in-person",
    "not looking to meet",
    "not able to meet in person",
    "unable to meet in person",
    "won't meet in person",
    "will not meet in person",
    // Typed into real chats before these were listed: dead ends the rules
    // already forbid ("I cannot make it to your place, but … in my thoughts").
    "i cannot make it to your place",
    "i can't make it to your place",
    "i cannot make it to you",
    "i can't make it to you",
    "i cannot make it over",
    "i can't make it over",
    "i cannot make that happen",
    "i can't make that happen",
    "i cannot come over",
    "i can't come over",
    "i cannot come to your place",
    "i can't come to your place",
    "i cannot come to you",
    "i can't come to you",
    "right here in my thoughts",
    "only in my thoughts",
    "in my thoughts instead",
];

/// True when the reply explicitly tells the client that the user cannot meet,
/// doesn't meet in person, or doesn't do meetups. Such dead-end refusals shut
/// down the conversation and are never typed.
fn explicitly_refuses_to_meet(text: &str) -> bool {
    let normalized = text.to_lowercase().replace(['\u{2019}', '’'], "'");
    // When they are already waiting, the rules ask the reply to say kindly that
    // the user cannot make it, so "make it" and "come" refusals are allowed then.
    let waiting = normalized.contains("wait");
    if MEETUP_REFUSALS.iter().any(|phrase| {
        normalized.contains(phrase)
            && !(waiting && (phrase.contains("make it") || phrase.contains("come")))
    }) {
        return true;
    }

    let mentions_meeting = normalized.contains("meeting in person")
        || normalized.contains("meet in person")
        || normalized.contains("meet up");
    if mentions_meeting
        && (normalized.contains("not something i do")
            || normalized.contains("isn't something i do")
            || normalized.contains("not an option")
            || normalized.contains("off the table"))
    {
        return true;
    }

    false
}

/// True when the reply introduces extreme fetishes or kinks that were not in the conversation.
/// Such replies are never typed and must be rewritten or dropped.
fn has_unprompted_extreme_kink(text: &str) -> bool {
    let lower = text.to_lowercase();
    const EXTREME_KINKS: [&str; 8] = [
        "golden shower",
        "watersport",
        "watersports",
        "piss on",
        "pissing on",
        "pee on you",
        "pee on me",
        "drink my piss",
    ];
    EXTREME_KINKS.iter().any(|kink| lower.contains(kink))
}

/// Phrases that use the overused, generic start-position closing question.
/// These read as an AI signature cliché rather than natural human texting,
/// so they are never typed and are rewritten with a situational, spicy CTA.
const CLICHE_POSITION_CTAS: [&str; 24] = [
    "position do you want to start with",
    "position would you like to start with",
    "position do you want to start",
    "position would you like to start",
    "position do you prefer to start",
    "position do you want to try first",
    "position do you want to do first",
    "which position do you want",
    "what position do you want",
    "which position do you like to start",
    "what position do you like to start",
    "which position do we start with",
    "what position do we start with",
    "which position should we start with",
    "what position should we start with",
    "which position would you like",
    "what position would you like",
    "which position do you like",
    "what position do you like",
    "which position do you prefer",
    "what position do you prefer",
    "which position do you want to try",
    "what position do you want to try",
    "which position do you want to do",
];

fn has_cliche_position_cta(text: &str) -> bool {
    let lower = text.to_lowercase().replace(['\u{2019}', '’'], "'");
    if CLICHE_POSITION_CTAS
        .iter()
        .any(|phrase| lower.contains(phrase))
    {
        return true;
    }
    lower.contains("position")
        && (lower.contains("start with")
            || lower.contains("to start")
            || lower.contains("try first")
            || lower.contains("do first"))
}

/// Strips hallucinated phantom photo thank-you greetings when prepended to a normal text reply.
fn strip_phantom_photo_opener(text: &str) -> String {
    let lower = text.to_lowercase();
    const PHOTO_OPENERS: [&str; 26] = [
        "thank you for sharing that nice picture with me.",
        "thank you for sharing that nice picture with me,",
        "thank you for sharing that hot pic with me.",
        "thank you for sharing that hot pic with me,",
        "thank you for sharing that hot picture with me.",
        "thank you for sharing that hot picture with me,",
        "thank you for sharing that picture with me.",
        "thank you for sharing that picture with me,",
        "thank you for sharing that photo with me.",
        "thank you for sharing that photo with me,",
        "thank you for sending that hot pic to me.",
        "thank you for sending that hot pic to me,",
        "thank you for sending that nice picture to me.",
        "thank you for sending that nice picture to me,",
        "thank you for sending that hot picture to me.",
        "thank you for sending that hot picture to me,",
        "thank you for sending that picture to me.",
        "thank you for sending that picture to me,",
        "thank you for sending that photo to me.",
        "thank you for sending that photo to me,",
        "thank you for the hot pic.",
        "thank you for the nice picture.",
        "thank you for the picture.",
        "thank you for the photo.",
        "thanks for sharing that picture.",
        "thanks for sending that picture.",
    ];
    for opener in PHOTO_OPENERS {
        if lower.starts_with(opener) {
            let remainder = text[opener.len()..].trim_start();
            if remainder.len() >= 30 && remainder.contains('?') {
                return remainder.to_string();
            }
        }
    }
    text.to_string()
}

/// Replaces a generic position closing question with an enticing, scene-specific spicy CTA.
fn replace_cliche_position_cta(text: &str) -> String {
    let lower = text.to_lowercase().replace(['\u{2019}', '’'], "'");
    if let Some(idx) = lower.rfind("position") {
        let prefix = &text[..idx];
        let start = prefix
            .rfind(|c| c == '.' || c == '!' || c == '?' || c == ';')
            .map(|i| i + 1)
            .unwrap_or(0);
        let kept = text[..start].trim_end();
        let lower_kept = kept.to_lowercase();
        let spicy_cta = if lower_kept.contains("ride")
            || lower_kept.contains("cowgirl")
            || lower_kept.contains("grind")
            || lower_kept.contains("on top")
        {
            "Where are your hands going to be while I'm grinding on top of you?"
        } else if lower_kept.contains("swallow")
            || lower_kept.contains("taste")
            || lower_kept.contains("lick")
            || lower_kept.contains("juices")
            || lower_kept.contains("tongue")
        {
            "What are you going to do to me once you've tasted every single drop?"
        } else if lower_kept.contains("fuck")
            || lower_kept.contains("slide")
            || lower_kept.contains("cock")
            || lower_kept.contains("dick")
            || lower_kept.contains("inside")
        {
            "Do you want me to take you slow and deep, or drive you wild fast?"
        } else {
            "Where are your hands going to be while I'm doing that to you?"
        };
        if kept.is_empty() {
            return spicy_cta.to_string();
        } else {
            return format!("{kept} {spicy_cta}");
        }
    }
    text.to_string()
}

/// Sanitizes a cold, dead-end meetup refusal into a warm, playful deflection.
fn sanitize_meet_refusal(text: &str) -> String {
    let lower = text.to_lowercase().replace(['\u{2019}', '’'], "'");
    for phrase in MEETUP_REFUSALS {
        if let Some(idx) = lower.find(phrase) {
            let prefix = &text[..idx];
            let start = prefix
                .rfind(|c| c == '.' || c == '!' || c == '?' || c == ';')
                .map(|i| i + 1)
                .unwrap_or(0);
            let after = &text[idx..];
            let end = after
                .find(|c| c == '.' || c == '!' || c == '?' || c == ';')
                .map(|i| idx + i + 1)
                .unwrap_or(text.len());
            let before = text[..start].trim();
            let remainder = text[end..].trim();
            let deflection = "You definitely don't waste any time, do you? 😉 I like that energy, but slow down mister—I always take my time getting to know someone and seeing if our chemistry is real first.";
            let mut result = String::new();
            if !before.is_empty() {
                result.push_str(before);
                result.push(' ');
            }
            result.push_str(deflection);
            if !remainder.is_empty() {
                result.push(' ');
                result.push_str(remainder);
            }
            return result;
        }
    }
    text.to_string()
}

/// Sanitizes an unprompted extreme kink into a sensual, scene-appropriate CTA.
fn sanitize_extreme_kink(text: &str) -> String {
    let lower = text.to_lowercase();
    const EXTREME_KINKS: [&str; 8] = [
        "golden shower",
        "watersport",
        "watersports",
        "piss on",
        "pissing on",
        "pee on you",
        "pee on me",
        "drink my piss",
    ];
    for kink in EXTREME_KINKS {
        if let Some(idx) = lower.find(kink) {
            let prefix = &text[..idx];
            let start = prefix
                .rfind(|c| c == '.' || c == '!' || c == '?' || c == ';')
                .map(|i| i + 1)
                .unwrap_or(0);
            let kept = text[..start].trim_end();
            let spicy = "What are you going to do to me once you've tasted every single drop?";
            if kept.is_empty() {
                return spicy.to_string();
            } else {
                return format!("{kept} {spicy}");
            }
        }
    }
    text.to_string()
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
#[derive(Clone)]
struct Attempt<'a> {
    name: &'a str,
    url: &'a str,
    model: &'a str,
    max_tokens: u32,
    reasoning_effort: Option<&'a str>,
    key: Option<String>,
}

/// Drafts with one provider; if the only fault is the forbidden word "love",
/// asks the same provider once to rewrite its draft before falling back.
fn try_attempt_with_options(
    attempt: &Attempt,
    key: &str,
    input: &ChatInput,
    hint: Option<&str>,
    avoid: Option<&str>,
    extras: &PromptExtras,
) -> Result<String, String> {
    let body = build_request_with_options(
        attempt.model,
        attempt.max_tokens,
        attempt.reasoning_effort,
        input,
        hint,
        avoid,
        extras,
    );
    match request_reply(attempt.url, key, &body) {
        Err(e) => {
            if let Some(draft) = e.strip_prefix("the reply used the word love: ") {
                info!(
                    "{} used the word love, asking it to rewrite the reply",
                    attempt.name
                );
                let rewrite = build_love_rewrite_request(
                    attempt.model,
                    attempt.max_tokens,
                    attempt.reasoning_effort,
                    draft,
                );
                request_reply(attempt.url, key, &rewrite)
            } else if let Some(draft) = e.strip_prefix("the reply promised to meet in person: ") {
                info!(
                    "{} agreed to meet, asking it to rewrite the reply as a deflection",
                    attempt.name
                );
                let rewrite = build_meet_refusal_rewrite_request(
                    attempt.model,
                    attempt.max_tokens,
                    attempt.reasoning_effort,
                    draft,
                );
                request_reply(attempt.url, key, &rewrite)
            } else if let Some(draft) = e.strip_prefix("the reply explicitly refused to meet: ") {
                info!(
                    "{} explicitly refused to meet, asking it to rewrite the reply",
                    attempt.name
                );
                let rewrite = build_meet_refusal_rewrite_request(
                    attempt.model,
                    attempt.max_tokens,
                    attempt.reasoning_effort,
                    draft,
                );
                match request_reply(attempt.url, key, &rewrite) {
                    Ok(r) => Ok(r),
                    Err(_) => Ok(sanitize_meet_refusal(draft)),
                }
            } else if let Some(draft) =
                e.strip_prefix("the reply introduced an unprompted extreme kink: ")
            {
                info!(
                    "{} introduced an extreme kink, asking it to rewrite the reply",
                    attempt.name
                );
                let rewrite = build_extreme_kink_rewrite_request(
                    attempt.model,
                    attempt.max_tokens,
                    attempt.reasoning_effort,
                    draft,
                );
                match request_reply(attempt.url, key, &rewrite) {
                    Ok(r) => Ok(r),
                    Err(_) => Ok(sanitize_extreme_kink(draft)),
                }
            } else if let Some(draft) = e.strip_prefix("the reply used a generic position CTA: ") {
                info!(
                    "{} used a generic position CTA, asking it to spice up the closing question",
                    attempt.name
                );
                let rewrite = build_spicy_cta_rewrite_request(
                    attempt.model,
                    attempt.max_tokens,
                    attempt.reasoning_effort,
                    draft,
                );
                match request_reply(attempt.url, key, &rewrite) {
                    Ok(r) => Ok(r),
                    Err(_) => Ok(replace_cliche_position_cta(draft)),
                }
            } else {
                Err(e)
            }
        }
        ok => ok,
    }
}

/// Tries each provider in order and returns the first reply. Fails only when
/// all of them fail, with each one's reason.
fn draft_with_fallback_options(
    attempts: &[Attempt],
    input: &ChatInput,
    hint: Option<&str>,
    avoid: Option<&str>,
    extras: &PromptExtras,
    check_quality: bool,
) -> Result<String, String> {
    let mut failures = Vec::new();
    // The first weak draft, typed if no other model writes a better one.
    let mut weak_draft: Option<(&str, String)> = None;
    for attempt in attempts {
        let Some(key) = &attempt.key else {
            failures.push(format!("{}: no key saved", attempt.name));
            continue;
        };
        match try_attempt_with_options(attempt, key, input, hint, avoid, extras) {
            Ok(reply) => {
                let weak = check_quality
                    .then(|| weak_draft_reason(&reply, &extras.recent_ctas, &extras.recent_drafts))
                    .flatten();
                match weak {
                    None => {
                        info!("Drafted the reply with {}", attempt.name);
                        set_last_draft_provider(attempt.name);
                        return Ok(reply);
                    }
                    Some(why) => {
                        info!(
                            "{}'s draft is not good enough ({why}), trying the next model",
                            attempt.name
                        );
                        weak_draft.get_or_insert((attempt.name, reply));
                    }
                }
            }
            Err(e) if e == "no conversation on screen" => {
                info!("{} found no conversation on screen", attempt.name);
                return Err(e);
            }
            Err(e) if e == SCROLLED_UP => {
                info!("{} found the chat scrolled up", attempt.name);
                return Err(e);
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
    if let Some((name, reply)) = weak_draft {
        info!("No model wrote a better draft, using the first one");
        set_last_draft_provider(name);
        return Ok(reply);
    }
    Err(format!(
        "No provider drafted a reply. {}",
        failures.join("; ")
    ))
}

#[cfg(test)]
fn draft_with_fallback(attempts: &[Attempt], jpeg_base64: &str) -> Result<String, String> {
    draft_with_fallback_options(
        attempts,
        &ChatInput::Screenshot(jpeg_base64.to_string()),
        None,
        None,
        &PromptExtras::default(),
        false,
    )
}

fn saved_key(provider: &Provider) -> Option<String> {
    if let Ok(key) = std::env::var(provider.env_var) {
        if !key.trim().is_empty() {
            return Some(key.trim().to_string());
        }
    }
    crate::key_store::read(provider.keychain_service)
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
        crate::key_store::delete(provider.keychain_service)?;
        info!("Removed the {} key", provider.name);
    } else {
        crate::key_store::write(provider.keychain_service, key)?;
        info!("Saved the {} key", provider.name);
    }
    Ok(())
}

/// Screenshots the chat to reply to. Takes the window in front, so a chat
/// inside a small or remote window (AnyDesk) is still readable after the
/// shrink. Falls back to the whole display.
pub(crate) fn capture_screen_jpeg() -> Result<Vec<u8>, String> {
    // A position found in this picture (the input box) is clicked inside
    // whatever was captured: the window, or else the whole display.
    set_last_input_box(None);
    match crate::watch::capture_front_window_jpeg() {
        Ok((bytes, area)) => {
            set_capture_area(Some(area));
            set_window_text(read_front_window_text());
            Ok(bytes)
        }
        Err(e) => {
            set_capture_area(None);
            set_window_text(None);
            warn!("Could not capture the front window, using the whole display: {e}");
            capture_display_jpeg()
        }
    }
}

/// Where the last screenshot was taken from on screen (x, y, width, height),
/// when it was one window; None means the whole display.
static CAPTURE_AREA: std::sync::Mutex<Option<(f64, f64, f64, f64)>> = std::sync::Mutex::new(None);

fn set_capture_area(area: Option<(f64, f64, f64, f64)>) {
    if let Ok(mut guard) = CAPTURE_AREA.lock() {
        *guard = area;
    }
}

pub fn last_capture_area() -> Option<(f64, f64, f64, f64)> {
    CAPTURE_AREA.lock().ok().and_then(|g| *g)
}

/// The chat window's text, read alongside the screenshot it belongs to, so
/// the model can read the words exactly. Windows only; None elsewhere.
static WINDOW_TEXT: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn set_window_text(text: Option<String>) {
    if let Ok(mut guard) = WINDOW_TEXT.lock() {
        *guard = text;
    }
}

/// How much window text goes into a prompt: the newest end of the chat.
#[cfg(target_os = "windows")]
const WINDOW_TEXT_MAX_CHARS: usize = 4000;

/// Reads the visible text of the window in front through UI Automation, the
/// text screen readers use. Works for chats in a browser or a desktop app; a
/// remote desktop (AnyDesk) has no text to read, and then the screenshot is
/// all the model gets.
#[cfg(target_os = "windows")]
fn read_front_window_text() -> Option<String> {
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    };
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, TreeScope_Descendants, UIA_TextControlTypeId,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

    let lines = unsafe {
        // Already initialised on this thread is fine: the result is ignored.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let automation: IUIAutomation =
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
        let root = automation.ElementFromHandle(GetForegroundWindow()).ok()?;
        let all = automation.CreateTrueCondition().ok()?;
        let elements = root.FindAll(TreeScope_Descendants, &all).ok()?;
        let mut lines: Vec<String> = Vec::new();
        for i in 0..elements.Length().ok()? {
            let Ok(element) = elements.GetElement(i) else {
                continue;
            };
            if element.CurrentControlType().ok() != Some(UIA_TextControlTypeId) {
                continue;
            }
            let Ok(name) = element.CurrentName() else {
                continue;
            };
            let text = name.to_string();
            let text = text.trim();
            if !text.is_empty() && lines.last().map(String::as_str) != Some(text) {
                lines.push(text.to_string());
            }
        }
        lines
    };
    let text = lines.join("\n");
    if text.is_empty() {
        return None;
    }
    // Keep the newest end of the chat.
    let skip = text.chars().count().saturating_sub(WINDOW_TEXT_MAX_CHARS);
    Some(text.chars().skip(skip).collect())
}

#[cfg(not(target_os = "windows"))]
fn read_front_window_text() -> Option<String> {
    None
}

#[cfg(target_os = "macos")]
fn capture_display_jpeg() -> Result<Vec<u8>, String> {
    let path = std::env::temp_dir().join(format!("chatelle-reply-{}.jpg", std::process::id()));
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
fn capture_display_jpeg() -> Result<Vec<u8>, String> {
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
fn capture_display_jpeg() -> Result<Vec<u8>, String> {
    Err("Reply drafting reads the screen and is only available on macOS and Windows".to_string())
}

/// Drafts a reply from provided JPEG bytes with optional voice hint and avoidance of previous draft.
pub fn draft_reply_from_jpeg_with_options(
    jpeg: &[u8],
    hint: Option<&str>,
    avoid: Option<&str>,
) -> Result<String, String> {
    let attempts = provider_attempts()?;
    // Re-roll (a rejected draft to avoid): start with the model after the one
    // that wrote it, so the new draft comes from a different model.
    let mut attempts = attempts;
    if avoid.is_some() {
        let last = LAST_DRAFT_PROVIDER.lock().ok().and_then(|g| g.clone());
        rotate_after(&mut attempts, last.as_deref());
    }
    let chat = crate::screen_text::read_chat(jpeg);
    let drafted = draft_text_first(&attempts, jpeg, &chat, hint, avoid, &prompt_extras(None));
    // With a hint, any input box works: when there is no chat to reply to,
    // write what the hint asks for instead.
    let reply = match (drafted, hint.map(str::trim).filter(|h| !h.is_empty())) {
        (Err(e), Some(h)) if e == "no conversation on screen" => {
            info!("No conversation on screen, writing from the voice hint alone");
            draft_from_hint(&attempts, h, avoid)?
        }
        (drafted, _) => drafted?,
    };
    accept_draft(&reply);
    Ok(reply)
}

/// Drafts from the chat text read off the screenshot when there is some: a
/// request a tenth the size, which fits every free plan. Falls back to the
/// screenshot itself when there is no text, or when no model could draft from
/// it (including "no conversation").
fn draft_text_first(
    attempts: &[Attempt],
    jpeg: &[u8],
    chat: &crate::screen_text::ChatRead,
    hint: Option<&str>,
    avoid: Option<&str>,
    extras: &PromptExtras,
) -> Result<String, String> {
    // The chat stage, from the text read off the screenshot or, on Windows,
    // the chat window's own text.
    let mut extras = extras.clone();
    extras.chat_stage = chat.stage.or_else(|| {
        extras
            .window_text
            .as_deref()
            .and_then(crate::screen_text::chat_stage)
    });
    let extras = &extras;
    if let Some(text) = &chat.transcript {
        let input = ChatInput::Text(text.clone());
        match draft_with_fallback_options(
            attempts,
            &input,
            hint,
            avoid,
            extras,
            extras.tone.is_none(),
        ) {
            Ok(reply) => {
                // The model saw no picture, so the input box comes from the
                // text recognition (its placeholder), if it was found.
                set_last_input_box(chat.input_box.map(|(left, top, right, bottom)| {
                    InputBoxCoords {
                        ymin: top * 1000.0,
                        xmin: left * 1000.0,
                        ymax: bottom * 1000.0,
                        xmax: right * 1000.0,
                    }
                }));
                info!("Drafted from the chat text, without the screenshot");
                return Ok(reply);
            }
            Err(e) => info!("Drafting from the chat text failed ({e}), sending the screenshot"),
        }
    }
    let input = ChatInput::Screenshot(base64::engine::general_purpose::STANDARD.encode(jpeg));
    draft_with_fallback_options(attempts, &input, hint, avoid, extras, extras.tone.is_none())
}

/// Every provider in order, with its saved key. Fails when no key is saved.
fn provider_attempts() -> Result<Vec<Attempt<'static>>, String> {
    let attempts: Vec<Attempt> = PROVIDERS
        .iter()
        .map(|p| Attempt {
            name: p.name,
            url: p.url,
            model: p.model,
            max_tokens: p.max_tokens,
            reasoning_effort: p.reasoning_effort,
            key: saved_key(p),
        })
        .collect();
    if attempts.iter().all(|a| a.key.is_none()) {
        return Err(
            "No provider key saved. Add one in Settings under Draft reply keys.".to_string(),
        );
    }
    Ok(attempts)
}

/// Once the first of the three drafts is ready, how long to wait for the rest.
const MORE_CHOICES_WAIT: Duration = Duration::from_secs(4);

/// How each of the three drafts to choose from should differ.
const CHOICE_VARIANTS: [Option<&str>; 3] = [
    None,
    Some("Make this version more playful and casual than a usual reply."),
    Some("Make this version warmer and more personal, and end on a different kind of question."),
];

/// Drafts what to type. With `choose`, writes up to three different versions
/// at once for the user to pick from; a single result (or `choose` off) is
/// accepted right away, while several are accepted only when one is picked
/// (`accept_draft`). `tone` is a change asked for from the picker (see
/// `tone_instruction`), applied to every version.
pub fn draft_for_typing(
    jpeg: &[u8],
    hint: Option<&str>,
    avoid: Option<&str>,
    choose: bool,
    tone: Option<&str>,
) -> Result<Vec<String>, String> {
    if !choose {
        return draft_reply_from_jpeg_with_options(jpeg, hint, avoid).map(|reply| vec![reply]);
    }
    let attempts = provider_attempts()?;
    // Read the chat text once for all three versions.
    let chat = crate::screen_text::read_chat(jpeg);
    let (sender, receiver) = std::sync::mpsc::channel();
    for (index, variant) in CHOICE_VARIANTS.iter().enumerate() {
        // Only the first version falls back through every model; the others
        // use the first model alone, so a slow fallback chain (a 30-second
        // OpenRouter answer) never holds the choices back.
        let attempts: Vec<Attempt<'static>> = if index == 0 {
            attempts.clone()
        } else {
            attempts
                .iter()
                .filter(|a| a.key.is_some())
                .take(1)
                .cloned()
                .collect()
        };
        let sender = sender.clone();
        let jpeg = jpeg.to_vec();
        let chat = chat.clone();
        let hint = hint.map(str::to_string);
        let avoid = avoid.map(str::to_string);
        let mut extras = prompt_extras(*variant);
        extras.tone = tone.map(str::to_string);
        std::thread::spawn(move || {
            let result = draft_text_first(
                &attempts,
                &jpeg,
                &chat,
                hint.as_deref(),
                avoid.as_deref(),
                &extras,
            );
            let _ = sender.send((index, result));
        });
    }
    drop(sender);
    // Wait for all three, but once one draft is ready give the others only a
    // few seconds; a version still running after that is left out.
    let mut slots: Vec<Option<Result<String, String>>> = vec![None; CHOICE_VARIANTS.len()];
    let mut deadline: Option<std::time::Instant> = None;
    loop {
        let message = match deadline {
            None => receiver.recv().ok(),
            Some(d) => receiver
                .recv_timeout(d.saturating_duration_since(std::time::Instant::now()))
                .ok(),
        };
        let Some((index, result)) = message else {
            break;
        };
        if result.is_ok() && deadline.is_none() {
            deadline = Some(std::time::Instant::now() + MORE_CHOICES_WAIT);
        }
        slots[index] = Some(result);
        if slots.iter().all(Option::is_some) {
            break;
        }
    }
    let results: Vec<Result<String, String>> = slots.into_iter().flatten().collect();
    let mut drafts: Vec<String> = Vec::new();
    for draft in results.iter().filter_map(|r| r.as_ref().ok()) {
        if !drafts.contains(draft) {
            drafts.push(draft.clone());
        }
    }
    if drafts.is_empty() {
        let error = results
            .into_iter()
            .find_map(Result::err)
            .unwrap_or_else(|| "No draft was written".to_string());
        // With a hint, any input box works: write from the hint alone.
        return match hint.map(str::trim).filter(|h| !h.is_empty()) {
            Some(h) if error == "no conversation on screen" => {
                info!("No conversation on screen, writing from the voice hint alone");
                let reply = draft_from_hint(&attempts, h, None)?;
                accept_draft(&reply);
                Ok(vec![reply])
            }
            _ => Err(error),
        };
    }
    if drafts.len() == 1 {
        accept_draft(&drafts[0]);
    }
    Ok(drafts)
}

/// Drafts a reply from provided JPEG bytes.
pub fn draft_reply_from_jpeg(jpeg: &[u8]) -> Result<String, String> {
    draft_reply_from_jpeg_with_options(jpeg, None, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_carries_the_screenshot_as_an_image_part() {
        let body = build_request("some-model", 1234, None, "AAAA");
        assert_eq!(body["model"], "some-model");
        assert_eq!(body["max_tokens"], 1234);
        assert!(body.get("reasoning_effort").is_none());
        assert_eq!(body["messages"][0]["role"], "system");
        let image = &body["messages"][1]["content"][0];
        assert_eq!(image["type"], "image_url");
        assert_eq!(image["image_url"]["url"], "data:image/jpeg;base64,AAAA");

        let with_effort = build_request("some-model", 800, Some("low"), "AAAA");
        assert_eq!(with_effort["reasoning_effort"], "low");
    }

    #[test]
    fn hint_only_request_has_no_screenshot_and_carries_the_hint() {
        let body = build_hint_only_request(
            "test-model",
            1000,
            None,
            "tell them the features are not working",
            Some("Features are down."),
        );
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        let prompt = messages[0]["content"].as_str().unwrap();
        assert!(prompt.contains("tell them the features are not working"));
        assert!(prompt.contains("Features are down."));
        assert!(!body.to_string().contains("image_url"));
    }

    #[test]
    fn request_with_hint_and_avoid_includes_operator_guidance() {
        let body = build_request_with_options(
            "test-model",
            1000,
            None,
            &ChatInput::Screenshot("AAAA".to_string()),
            Some("be playful and mention dessert"),
            Some("I love tiramisu!"),
            &PromptExtras {
                recent_ctas: vec!["What are you cooking tonight?".to_string()],
                ..Default::default()
            },
        );
        let prompt = body["messages"][1]["content"][1]["text"].as_str().unwrap();
        assert!(prompt.contains("OPERATOR GUIDANCE / VOICE HINT"));
        assert!(prompt.contains("be playful and mention dessert"));
        assert!(prompt.contains("DO NOT REPEAT PREVIOUS DRAFT"));
        assert!(prompt.contains("I love tiramisu!"));
        assert!(prompt.contains("RECENT CLOSING QUESTIONS"));
        assert!(prompt.contains("What are you cooking tonight?"));
    }

    #[test]
    fn the_prompt_says_whether_the_chat_is_new() {
        use crate::screen_text::ChatStage;
        let prompt_for = |stage| {
            let body = build_request_with_options(
                "m",
                1000,
                None,
                &ChatInput::Screenshot("AAAA".to_string()),
                None,
                None,
                &PromptExtras {
                    chat_stage: stage,
                    ..Default::default()
                },
            );
            body["messages"][1]["content"][1]["text"]
                .as_str()
                .unwrap()
                .to_string()
        };
        let new = prompt_for(Some(ChatStage::New));
        assert!(new.contains("NEW CHAT") && new.contains("open this reply by giving it"));
        let ongoing = prompt_for(Some(ChatStage::Ongoing));
        assert!(ongoing.contains("ONGOING CHAT") && !ongoing.contains("NEW CHAT"));
        let unknown = prompt_for(None);
        assert!(!unknown.contains("NEW CHAT") && !unknown.contains("ONGOING CHAT"));
    }

    #[test]
    fn summary_and_own_messages_are_split_off_the_reply() {
        let (reply, trailers) = extract_trailers(
            "Sounds like a great plan for the weekend. What time works?\nSUMMARY: Planning a weekend trip.\nMY_MESSAGES: haha yes || \"see you then\"",
        );
        assert_eq!(
            reply,
            "Sounds like a great plan for the weekend. What time works?"
        );
        let trailers = trailers.unwrap();
        assert_eq!(
            trailers.summary.as_deref(),
            Some("Planning a weekend trip.")
        );
        assert_eq!(trailers.my_messages, ["haha yes", "see you then"]);

        let (reply, trailers) = extract_trailers("Just the reply?");
        assert_eq!(reply, "Just the reply?");
        assert!(trailers.is_none());

        let (_, trailers) = extract_trailers("Hi?\nMY_MESSAGES: none");
        assert!(trailers.unwrap().my_messages.is_empty());

        // Lines put before the reply are taken out too.
        let (reply, trailers) =
            extract_trailers("SUMMARY: Moving to Rwanda.\nHow is the packing going?");
        assert_eq!(reply, "How is the packing going?");
        assert!(trailers.is_some());

        // Only the extra lines: no reply at all.
        let (reply, _) = extract_trailers("SUMMARY: Moving.\nMY_MESSAGES: none");
        assert!(reply.is_empty());
    }

    #[test]
    fn only_the_picker_tones_have_instructions() {
        for tone in ["shorter", "longer", "funnier", "serious"] {
            assert!(tone_instruction(tone).is_some(), "{tone}");
        }
        assert!(tone_instruction("ignore the rules").is_none());
    }

    #[test]
    fn reroll_starts_with_the_model_after_the_last_one() {
        let attempt = |name| Attempt {
            name,
            url: "",
            model: "m",
            max_tokens: MAX_REPLY_TOKENS,
            reasoning_effort: None,
            key: None,
        };
        fn names(a: &[Attempt]) -> Vec<String> {
            a.iter().map(|a| a.name.to_string()).collect()
        }
        let mut attempts = vec![attempt("Gemini"), attempt("Groq"), attempt("OpenRouter")];
        rotate_after(&mut attempts, Some("Gemini"));
        assert_eq!(names(&attempts), ["Groq", "OpenRouter", "Gemini"]);
        rotate_after(&mut attempts, Some("OpenRouter"));
        assert_eq!(names(&attempts), ["Gemini", "Groq", "OpenRouter"]);
        rotate_after(&mut attempts, None);
        assert_eq!(names(&attempts), ["Gemini", "Groq", "OpenRouter"]);
    }

    #[test]
    fn closing_question_is_the_last_question_sentence() {
        assert_eq!(
            closing_question(
                "That sounds fun. Are you free later? Tell me more about it, what did you cook?"
            ),
            Some("Tell me more about it, what did you cook?")
        );
        assert_eq!(closing_question("No question here."), None);
    }

    #[test]
    fn a_reworded_closing_question_counts_as_a_repeat() {
        assert!(same_cta(
            "Where are your hands while I'm grinding on top of you?",
            "So where are your hands while I grind on top of you?"
        ));
        assert!(!same_cta(
            "Where are your hands while I'm grinding on top of you?",
            "What did you cook for dinner tonight?"
        ));
    }

    #[test]
    fn weak_drafts_are_short_have_no_question_or_repeat_a_cta() {
        let good = "That sounds like a great evening out with friends, and the food must have been amazing. What did you order first?";
        assert_eq!(weak_draft_reason(good, &[], &[]), None);
        assert!(weak_draft_reason("Nice. What else?", &[], &[]).is_some());
        assert!(weak_draft_reason(
            "That sounds like a great evening out with friends, and the food must have been amazing too.",
            &[],
            &[]
        )
        .is_some());
        let recent = ["What did you order first at dinner?".to_string()];
        assert!(weak_draft_reason(good, &recent, &[]).is_some());
    }

    #[test]
    fn a_scrolled_up_chat_is_never_answered() {
        let response = json!({ "choices": [{ "message": { "content": "SCROLLED_UP" } }] });
        assert_eq!(parse_reply(&response).unwrap_err(), SCROLLED_UP);
        assert!(instructions().contains("reply with SCROLLED_UP only"));
    }

    #[test]
    fn right_now_is_dropped_from_the_reply() {
        assert_eq!(
            drop_right_now("I would like you to slide your tongue over me. Are you ready to taste me right now?"),
            "I would like you to slide your tongue over me. Are you ready to taste me?"
        );
        assert_eq!(
            drop_right_now("I want to feel you inside me right now while I stroke you."),
            "I want to feel you inside me while I stroke you."
        );
        assert_eq!(
            drop_right_now("Mmm. Right now, I am lying on my bed. What about you?"),
            "Mmm. I am lying on my bed. What about you?"
        );
        assert_eq!(
            drop_right_now("Nothing to change here?"),
            "Nothing to change here?"
        );
    }

    #[test]
    fn a_draft_that_opens_like_a_recent_one_is_weak() {
        // A real chat where every reply opened with "That …" and read like a bot.
        let recent = [
            "Sitting by the water as the sun goes down sounds so relaxing and warm. Do you like to bring anything with you?".to_string(),
            "That is always handy to have when it gets dark by the water. What is your favorite time of the evening to sit out there?".to_string(),
        ];
        let same = "That golden light makes everything feel so peaceful. Do you like sitting quietly and watching the colours change?";
        assert!(weak_draft_reason(same, &[], &recent)
            .unwrap()
            .contains("opens like"));
        let different = "A bench by the river with you sounds perfect, though I would probably end up in your lap. Would you keep that kiss sweet or make it the kind nobody should watch?";
        assert_eq!(weak_draft_reason(different, &[], &recent), None);
        assert_eq!(opening("That sounds lovely"), "that");
        assert_eq!(opening("I would like that"), "i would");
    }

    #[test]
    fn a_weak_draft_moves_on_to_the_next_model() {
        let (first_url, first) = serve_once(
            "200 OK",
            r#"{"choices":[{"message":{"content":"Nice. What else?"}}]}"#,
        );
        let strong = "That sounds like a great evening out with friends, and the food must have been amazing. What did you order first?";
        let (second_url, second) = serve_once(
            "200 OK",
            &format!(r#"{{"choices":[{{"message":{{"content":"{strong}"}}}}]}}"#),
        );
        let attempts = [
            Attempt {
                name: "First",
                url: &first_url,
                model: "m",
                max_tokens: MAX_REPLY_TOKENS,
                reasoning_effort: None,
                key: Some("k1".into()),
            },
            Attempt {
                name: "Second",
                url: &second_url,
                model: "m",
                max_tokens: MAX_REPLY_TOKENS,
                reasoning_effort: None,
                key: Some("k2".into()),
            },
        ];
        assert_eq!(
            draft_with_fallback_options(
                &attempts,
                &ChatInput::Screenshot("QUJD".to_string()),
                None,
                None,
                &PromptExtras::default(),
                true
            )
            .unwrap(),
            strong
        );
        first.join().unwrap();
        second.join().unwrap();
    }

    #[test]
    fn the_first_weak_draft_is_kept_when_no_model_does_better() {
        let (only_url, only) = serve_once(
            "200 OK",
            r#"{"choices":[{"message":{"content":"Nice. What else?"}}]}"#,
        );
        let attempts = [Attempt {
            name: "Only",
            url: &only_url,
            model: "m",
            max_tokens: MAX_REPLY_TOKENS,
            reasoning_effort: None,
            key: Some("k".into()),
        }];
        assert_eq!(
            draft_with_fallback_options(
                &attempts,
                &ChatInput::Screenshot("QUJD".to_string()),
                None,
                None,
                &PromptExtras::default(),
                true
            )
            .unwrap(),
            "Nice. What else?"
        );
        only.join().unwrap();
    }

    #[test]
    fn the_instructions_fit_any_chat_keep_notes_private_and_read_three_messages() {
        let text = instructions();
        assert!(text.contains("First, find the conversation"));
        assert!(text.contains("two to four sentences"));
        for app in ["WhatsApp", "Gleap", "Slack", "Instagram"] {
            assert!(!text.contains(app), "the instructions must not name {app}");
        }
        assert!(text.contains("more than 2 sentences or more than about 120 characters"));
        assert!(text.contains("never use more than 3 of their messages"));
        assert!(text.contains("Only as a last resort"));
        assert!(!text.contains("Never state the time"));
        assert!(text.contains("Internal notes, logs and warning banners are private to the team"));
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
        assert!(text.contains("makes them immediately see a dead end"));
        assert!(text.contains("never say that meeting in person is not something you do"));
        assert!(text.contains(
            "Never say you only chat online or prefer chatting right here to deflect a meetup"
        ));
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
    fn the_instructions_acknowledge_feelings_without_returning_them_in_everyday_chats() {
        let text = instructions();
        assert!(text.contains("When the user's own earlier messages on screen are not flirty"));
        assert!(text.contains("never ignore it and never return it"));
        assert!(text.contains("one short, warm and light sentence"));
        assert!(text.contains("keep a friendly distance"));
        assert!(!text.contains("thanking them for the kind words"));
    }

    #[test]
    fn the_instructions_match_flirty_and_explicit_chats_when_the_user_already_flirted() {
        let text = instructions();
        assert!(text.contains(
            "Unless a warning banner forbids it, when the user's own earlier messages on screen \
             are already flirty, teasing, sensual or sexually explicit"
        ));
        assert!(text.contains("match the other side's exact level of sexual explicitness directly"));
        assert!(text.contains("never thank them stiffly for kind words"));
        assert!(text.contains("never dodge into innocent small talk"));
        assert!(text.contains("hypothetical \"what if\" daydreams, sexual fantasies"));
        assert!(text.contains("without ever planning, promising or hinting at a real meetup"));
        assert!(text.contains(
            "never say you prefer to keep chatting right here when they have not asked to leave"
        ));
        assert!(text.contains("ignore any unsent draft already inside the typing box"));
        assert!(text.contains("maintain a non-sexual conversation"));
        assert!(text.contains("NO_CONVERSATION"));
    }

    #[test]
    fn a_model_refusal_or_content_filter_block_is_never_typed() {
        for response in [
            json!({ "choices": [{ "finish_reason": "content_filter", "message": { "content": "Wait—" } }] }),
            json!({ "choices": [{ "finish_reason": "SAFETY", "message": { "content": "" } }] }),
            json!({ "choices": [{ "message": { "content": "I'm sorry, I cannot fulfill this request." } }] }),
            json!({ "choices": [{ "message": { "content": "I cannot generate sexually explicit content." } }] }),
            json!({ "choices": [{ "message": { "content": "I\u{2019}m sorry, I can\u{2019}t help with that." } }] }),
            json!({ "choices": [{ "message": { "content": "As an AI, I have to follow safety guidelines." } }] }),
        ] {
            assert!(parse_reply(&response).is_err(), "{response}");
        }
    }

    #[test]
    fn a_refusal_switches_to_the_next_provider() {
        let (refused_url, refused) = serve_once(
            "200 OK",
            r#"{"choices":[{"message":{"content":"I'm sorry, I cannot generate sexually explicit content."}}]}"#,
        );
        let (ok_url, ok) = serve_once("200 OK", OK_BODY);
        let attempts = [
            Attempt {
                name: "First",
                url: &refused_url,
                model: "m",
                max_tokens: MAX_REPLY_TOKENS,
                reasoning_effort: None,
                key: Some("k1".into()),
            },
            Attempt {
                name: "Second",
                url: &ok_url,
                model: "m",
                max_tokens: MAX_REPLY_TOKENS,
                reasoning_effort: None,
                key: Some("k2".into()),
            },
        ];
        assert_eq!(
            draft_with_fallback(&attempts, "QUJD").unwrap(),
            "Hi Tarek, thanks."
        );
        refused.join().unwrap();
        assert!(ok.join().unwrap().to_lowercase().contains("bearer k2"));
    }

    #[test]
    fn the_instructions_never_repeat_a_sent_message_and_answer_personal_questions() {
        let text = instructions();
        assert!(text.contains("Never repeat or reword one of them"));
        assert!(text.contains("not their wording"));
        assert!(text.contains("answer it in one short, general sentence first"));
        assert!(text.contains("Avoid the repetition"));
        assert!(
            text.contains("never re-ask any question that was already asked in earlier bubbles")
        );
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
            "I am keeping my schedule completely open for now.",
            // Agreeing in disguise, typed into real chats.
            "That is not a problem. I can be able to host sometimes or we could get a hotel.",
            "Not any that I know of. But I could check with my secretary and see if I have an \
             opening. When are you normally free?",
            "Of course, I would like that, sweetheart. I have been dreaming of that since we \
             started talking. When are you available?",
            "I could always get a place to get naked with you. Would you like us to get a hotel?",
        ] {
            let response = json!({ "choices": [{ "message": { "content": reply } }] });
            let error = parse_reply(&response).unwrap_err();
            assert!(error.contains("promised to meet"), "{reply}");
        }
    }

    #[test]
    fn a_reply_that_explicitly_refuses_to_meet_is_never_typed() {
        for reply in [
            "Meeting in person is not something I do, but I really enjoy talking and getting to \
             know you right here. What kind of things do you usually like to do when you have free \
             time on the weekend?",
            "Meeting in person isn't something I do.",
            "I don't meet in person, but I enjoy chatting here.",
            "I cannot meet in person.",
            "I can't meet up in person, sorry!",
            "I do not meet anyone in person.",
            "I don't do meetups, but tell me about your day.",
            "Meeting in person is off the table.",
            "I never meet clients.",
            // Both were typed into real chats.
            "I cannot make it to your place, but I want you to ravage me right here in my \
             thoughts. How would you handle me if we were tangled up together right now?",
            "Hmm, with things the way they are, I can\u{2019}t make that happen, but I'm way \
             more intrigued by the story behind it. What turned you on so fast?",
        ] {
            let response = json!({ "choices": [{ "message": { "content": reply } }] });
            let error = parse_reply(&response).unwrap_err();
            assert!(error.contains("explicitly refused to meet"), "{reply}");
        }
    }

    #[test]
    fn saying_she_cannot_make_it_is_allowed_when_he_is_already_waiting() {
        assert!(!explicitly_refuses_to_meet(
            "I am so sorry, I can't make it to your place today, so please don't wait for me."
        ));
        assert!(explicitly_refuses_to_meet(
            "I can't make it to your place, but tell me what you would do to me."
        ));
    }

    #[test]
    fn being_in_her_thoughts_as_a_compliment_is_typed() {
        let reply = "You have been in my thoughts all day, and I keep picturing that smile. \
                     What did you get up to this afternoon?";
        let response = json!({ "choices": [{ "message": { "content": reply } }] });
        assert!(parse_reply(&response).is_ok());
    }

    #[test]
    fn a_reply_with_unprompted_extreme_kink_is_never_typed() {
        for reply in [
            "I definitely do not mind at all; I would really want you to taste me and swallow every drop. Do you enjoy a golden shower?",
            "I'd really like to try watersports with you sometime soon.",
            "Can I piss on you while you're in the shower?",
        ] {
            let response = json!({ "choices": [{ "message": { "content": reply } }] });
            let error = parse_reply(&response).unwrap_err();
            assert!(error.contains("unprompted extreme kink"), "{reply}");
        }
    }

    #[test]
    fn a_reply_with_generic_position_cta_is_never_typed() {
        for reply in [
            "I definitely do not mind at all, I would really want you to taste me and swallow every drop. What position do you want to start with?",
            "Hearing you say that turns me on so much. Which position do you want to start with?",
            "I'd really like to feel you on top of me. What position do you want?",
        ] {
            let response = json!({ "choices": [{ "message": { "content": reply } }] });
            let error = parse_reply(&response).unwrap_err();
            assert!(error.contains("generic position CTA"), "{reply}");
        }
    }

    #[test]
    fn phantom_photo_openers_are_stripped_from_text_replies() {
        // Screenshot 2 scenario
        let with_photo_opener = "Thank you for sharing that nice picture with me. I would really enjoy making those rolls for you sometime soon. Do you have a sweet tooth for breakfast items as well?";
        assert_eq!(
            strip_phantom_photo_opener(with_photo_opener),
            "I would really enjoy making those rolls for you sometime soon. Do you have a sweet tooth for breakfast items as well?"
        );

        // Screenshot 3 scenario
        let hot_pic_opener = "Thank you for sending that hot pic to me. I would like you to slide your cock deep inside my wet pussy and fuck me hard right now. Do you want me slow and deep or wild and fast?";
        assert_eq!(
            strip_phantom_photo_opener(hot_pic_opener),
            "I would like you to slide your cock deep inside my wet pussy and fuck me hard right now. Do you want me slow and deep or wild and fast?"
        );

        // Standalone photo acknowledgment is left alone
        let standalone = "Thank you for sharing that nice picture with me.";
        assert_eq!(strip_phantom_photo_opener(standalone), standalone);
    }

    #[test]
    fn generic_position_ctas_are_replaced_with_spicy_situational_alternatives() {
        let draft = "I definitely do not mind at all; I would really want you to taste me and swallow every drop. What position do you want to start with?";
        let replaced = replace_cliche_position_cta(draft);
        assert!(!has_cliche_position_cta(&replaced));
        assert!(replaced.contains("tasted every single drop"));

        let cowgirl_draft = "I want you to watch me ride you cowgirl and grind on you. Which position do you want to start with?";
        let replaced_cowgirl = replace_cliche_position_cta(cowgirl_draft);
        assert!(!has_cliche_position_cta(&replaced_cowgirl));
        assert!(replaced_cowgirl.contains("grinding on top of you"));
    }

    #[test]
    fn meetup_refusals_are_sanitized_to_playful_deflections() {
        let draft = "Meeting in person is not something I do, but I really enjoy talking and getting to know you right here. What kind of things do you usually like to do when you have free time on the weekend?";
        let sanitized = sanitize_meet_refusal(draft);
        assert!(!explicitly_refuses_to_meet(&sanitized));
        assert!(sanitized.contains("slow down mister"));
        assert!(sanitized.contains("What kind of things do you usually like to do"));
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
        let reply = request_reply(
            &url,
            "test-key",
            &build_request("m", MAX_REPLY_TOKENS, None, "QUJD"),
        )
        .unwrap();
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
                max_tokens: MAX_REPLY_TOKENS,
                reasoning_effort: None,
                key: Some("k1".into()),
            },
            Attempt {
                name: "Second",
                url: &ok_url,
                model: "m",
                max_tokens: MAX_REPLY_TOKENS,
                reasoning_effort: None,
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
                max_tokens: MAX_REPLY_TOKENS,
                reasoning_effort: None,
                key: None,
            },
            Attempt {
                name: "Second",
                url: &ok_url,
                model: "m",
                max_tokens: MAX_REPLY_TOKENS,
                reasoning_effort: None,
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
                max_tokens: GROQ_MAX_REPLY_TOKENS,
                reasoning_effort: Some("low"),
                key: Some("k1".into()),
            },
            Attempt {
                name: "OpenRouter",
                url: &broke_url,
                model: "m",
                max_tokens: MAX_REPLY_TOKENS,
                reasoning_effort: None,
                key: Some("k2".into()),
            },
            Attempt {
                name: "Gemini",
                url: "unused",
                model: "m",
                max_tokens: MAX_REPLY_TOKENS,
                reasoning_effort: None,
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

    #[test]
    fn provider_token_budgets_fit_each_free_tier() {
        let gemini = PROVIDERS.iter().find(|p| p.name == "Gemini").unwrap();
        let groq = PROVIDERS.iter().find(|p| p.name == "Groq").unwrap();
        let openrouter = PROVIDERS.iter().find(|p| p.name == "OpenRouter").unwrap();
        assert_eq!(gemini.max_tokens, 2000);
        assert!(gemini.reasoning_effort.is_none());
        assert!(
            groq.max_tokens <= 1000,
            "Groq free tier caps output reservation at 1,000 OTPM"
        );
        assert_eq!(groq.reasoning_effort, Some("low"));
        assert!(
            openrouter.max_tokens >= 4000,
            "OpenRouter free reasoning vision models spend ~3,500 tokens thinking"
        );
    }

    #[test]
    fn no_conversation_stops_without_trying_the_next_provider() {
        let (empty_url, empty) = serve_once(
            "200 OK",
            r#"{"choices":[{"message":{"content":"NO_CONVERSATION"}}]}"#,
        );
        let attempts = [
            Attempt {
                name: "First",
                url: &empty_url,
                model: "m",
                max_tokens: MAX_REPLY_TOKENS,
                reasoning_effort: None,
                key: Some("k1".into()),
            },
            Attempt {
                name: "Second",
                url: "http://127.0.0.1:9/must-not-be-called",
                model: "m",
                max_tokens: MAX_REPLY_TOKENS,
                reasoning_effort: None,
                key: Some("k2".into()),
            },
        ];
        let error = draft_with_fallback(&attempts, "QUJD").unwrap_err();
        empty.join().unwrap();
        assert_eq!(error, "no conversation on screen");
    }

    /// Serves a sequence of canned HTTP responses on localhost and returns each request it got.
    fn serve_sequence(
        responses: Vec<(&'static str, &'static str)>,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        let handle = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for (status, body) in responses {
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
                let reply = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(reply.as_bytes()).unwrap();
                requests.push(String::from_utf8_lossy(&received).to_string());
            }
            requests
        });
        (url, handle)
    }

    #[test]
    fn a_slip_with_the_word_love_is_rewritten_by_the_same_provider() {
        let (url, server) = serve_sequence(vec![
            (
                "200 OK",
                r#"{"choices":[{"message":{"content":"I would love to see a picture! What music are you playing?"}}]}"#,
            ),
            (
                "200 OK",
                r#"{"choices":[{"message":{"content":"I would really like to see a picture! What music are you playing?"}}]}"#,
            ),
        ]);
        let attempts = [
            Attempt {
                name: "First",
                url: &url,
                model: "m",
                max_tokens: MAX_REPLY_TOKENS,
                reasoning_effort: None,
                key: Some("k1".into()),
            },
            Attempt {
                name: "Second",
                url: "http://127.0.0.1:9/must-not-be-called",
                model: "m",
                max_tokens: MAX_REPLY_TOKENS,
                reasoning_effort: None,
                key: Some("k2".into()),
            },
        ];
        assert_eq!(
            draft_with_fallback(&attempts, "QUJD").unwrap(),
            "I would really like to see a picture! What music are you playing?"
        );
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].contains("data:image/jpeg;base64,QUJD"));
        assert!(!requests[1].contains("data:image/jpeg;base64"));
        assert!(requests[1].contains("I would love to see a picture!"));
    }

    #[test]
    fn a_slip_with_meet_refusal_is_rewritten_by_the_same_provider() {
        let (url, server) = serve_sequence(vec![
            (
                "200 OK",
                r#"{"choices":[{"message":{"content":"Meeting in person is not something I do, but I really enjoy talking with you. What music do you like?"}}]}"#,
            ),
            (
                "200 OK",
                r#"{"choices":[{"message":{"content":"You're getting ahead of yourself! I enjoy taking my time to see if we click first. What music do you like?"}}]}"#,
            ),
        ]);
        let attempts = [
            Attempt {
                name: "First",
                url: &url,
                model: "m",
                max_tokens: MAX_REPLY_TOKENS,
                reasoning_effort: None,
                key: Some("k1".into()),
            },
            Attempt {
                name: "Second",
                url: "http://127.0.0.1:9/must-not-be-called",
                model: "m",
                max_tokens: MAX_REPLY_TOKENS,
                reasoning_effort: None,
                key: Some("k2".into()),
            },
        ];
        assert_eq!(
            draft_with_fallback(&attempts, "QUJD").unwrap(),
            "You're getting ahead of yourself! I enjoy taking my time to see if we click first. What music do you like?"
        );
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].contains("data:image/jpeg;base64,QUJD"));
        assert!(!requests[1].contains("data:image/jpeg;base64"));
        assert!(requests[1].contains("Meeting in person is not something I do"));
    }

    #[test]
    fn a_slip_with_generic_position_cta_is_rewritten_with_a_spicy_cta() {
        let (url, server) = serve_sequence(vec![
            (
                "200 OK",
                r#"{"choices":[{"message":{"content":"I would really want you to taste me and swallow every drop. What position do you want to start with?"}}]}"#,
            ),
            (
                "200 OK",
                r#"{"choices":[{"message":{"content":"I would really want you to taste me and swallow every drop. Where are your hands while I'm doing that to you?"}}]}"#,
            ),
        ]);
        let attempts = [Attempt {
            name: "First",
            url: &url,
            model: "m",
            max_tokens: MAX_REPLY_TOKENS,
            reasoning_effort: None,
            key: Some("k1".into()),
        }];
        assert_eq!(
            draft_with_fallback(&attempts, "QUJD").unwrap(),
            "I would really want you to taste me and swallow every drop. Where are your hands while I'm doing that to you?"
        );
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].contains("data:image/jpeg;base64,QUJD"));
        assert!(!requests[1].contains("data:image/jpeg;base64"));
        assert!(requests[1].contains("What position do you want to start with?"));
    }

    #[test]
    fn a_failed_love_rewrite_switches_to_the_next_provider() {
        let (first_url, first) = serve_sequence(vec![
            (
                "200 OK",
                r#"{"choices":[{"message":{"content":"I would love to see that!"}}]}"#,
            ),
            (
                "200 OK",
                r#"{"choices":[{"message":{"content":"That sounds lovely!"}}]}"#,
            ),
        ]);
        let (second_url, second) = serve_once("200 OK", OK_BODY);
        let attempts = [
            Attempt {
                name: "First",
                url: &first_url,
                model: "m",
                max_tokens: MAX_REPLY_TOKENS,
                reasoning_effort: None,
                key: Some("k1".into()),
            },
            Attempt {
                name: "Second",
                url: &second_url,
                model: "m",
                max_tokens: MAX_REPLY_TOKENS,
                reasoning_effort: None,
                key: Some("k2".into()),
            },
        ];
        assert_eq!(
            draft_with_fallback(&attempts, "QUJD").unwrap(),
            "Hi Tarek, thanks."
        );
        assert_eq!(first.join().unwrap().len(), 2);
        assert!(second.join().unwrap().to_lowercase().contains("bearer k2"));
    }

    #[test]
    fn the_instructions_separate_bubble_sides_protect_sidebar_logs_and_avoid_camera_or_stalls() {
        let text = instructions();
        assert!(text.contains("Every coloured or tinted bubble"));
        assert!(text.contains("Every plain white or grey left-aligned bubble"));
        assert!(text.contains("Never answer a question inside a coloured bubble"));
        assert!(text.contains("a fact that appears only in a note or sidebar log"));
        assert!(text.contains("Never suggest a phone call, video call or going on camera"));
        assert!(text.contains("never say \"I will let you know\""));
        assert!(text.contains("never replace them with coy euphemisms like \"right there\""));
        assert!(text.contains("ending with a question mark (?)"));
    }

    #[test]
    fn the_instructions_acknowledge_sent_photos_and_infer_identity_when_cards_are_cropped() {
        let text = instructions();
        assert!(text.contains(
            "Whenever the newest left-side white bubble from the other side contains a photo"
        ));
        assert!(text.contains("blurred dark-grey or skin-toned square"));
        assert!(text.contains("always acknowledge the new photo in your very first sentence"));
        assert!(text.contains("thank them for sharing it"));
        assert!(text.contains("when profile cards are cropped off the screen"));
        assert!(text.contains("give me a call"));
        assert!(text.contains("I cannot make it to your place"));
        assert!(text.contains("Vary how you open and phrase sentences naturally"));
    }

    #[test]
    fn i_want_is_always_typed_as_i_would_like() {
        for (draft, typed) in [
            (
                "I want you to bury that cock deep inside me. How do you want me to move on you?",
                "I would like you to bury that cock deep inside me. How do you want me to move on you?",
            ),
            (
                "Mmm, I really want to taste you. Is that what I want? Yes, I want.",
                "Mmm, I would really like to taste you. Is that what I would like? Yes, I would like.",
            ),
            ("I wanna feel you. I wanted it too.", "I would like to feel you. I wanted it too."),
            ("HI wanting nothing, AI wants more.", "HI wanting nothing, AI wants more."),
        ] {
            let response = json!({ "choices": [{ "message": { "content": draft } }] });
            assert_eq!(parse_reply(&response).unwrap(), typed);
        }
    }

    #[test]
    fn the_instructions_keep_them_hooked_share_first_name_and_city_and_vary_topics() {
        let text = instructions();
        assert!(
            text.contains("The user's first name and home city or state are not contact details")
        );
        assert!(text.contains("never invent a name or city that is not on screen"));
        assert!(text.contains("Keep the other side hooked"));
        assert!(text.contains("Keep the conversation varied instead of sticking to one subject"));
        assert!(text.contains("Enough about perfumes"));
    }

    #[test]
    fn the_instructions_check_flow_and_forbid_mid_conversation_introductions() {
        let text = instructions();
        assert!(text.contains("Check the conversation flow before introducing yourself"));
        assert!(text.contains("Never begin the message by automatically introducing yourself"));
        assert!(text.contains(
            "Introducing yourself in the middle of an existing conversation is unnatural"
        ));
        assert!(
            text.contains("Only introduce yourself if it is the very first introductory exchange")
        );
    }

    #[test]
    fn the_instructions_never_acknowledge_own_photos_and_keep_compliments_natural() {
        let text = instructions();
        assert!(text.contains("every photo or blurred square inside a coloured or purple bubble on the right was sent by the user/model"));
        assert!(text.contains("Never acknowledge, compliment, or thank the other side for an image that the user sent"));
        assert!(text.contains(
            "if their bottom-most white bubble contains only text, never thank them for a photo"
        ));
        assert!(text.contains("Small interface controls"));
        assert!(text.contains("treat it as a text-only message"));
        assert!(text.contains("Do not refer to it as \"a huge code\""));
        assert!(text.contains("do not explicitly call it a \"dick pic\""));
        assert!(text.contains("Thank you for sharing that hot pic with me"));
    }

    #[test]
    fn the_instructions_differentiate_timezone_from_locality_and_prevent_default_new_york() {
        let text = instructions();
        assert!(text.contains("Ignore the TIMEZONE label (such as America/New_York) completely"));
        assert!(text.contains("Never default to or invent New York"));
        assert!(text.contains("Read the user's location from the LOCALITY field"));
        assert!(text.contains("roughly 20 to 30 minutes from the client's location"));
        assert!(text.contains("Say where the user is from only when they ask"));
        assert!(!text.contains("I am from Charlotte. I am here"));
    }

    #[test]
    fn truncated_or_mixed_script_glitch_replies_are_never_typed() {
        for response in [
            json!({ "choices": [{ "finish_reason": "length", "message": { "content": "Oh, wow, thank you for" } }] }),
            json!({ "choices": [{ "finish_reason": "max_tokens", "message": { "content": "I would really like" } }] }),
            json!({ "choices": [{ "finish_reason": "stop", "message": { "content": "Wouldblingفع typically" } }] }),
            json!({ "choices": [{ "finish_reason": "stop", "message": { "content": "I would really like فع to hear more about your day?" } }] }),
        ] {
            assert!(parse_reply(&response).is_err(), "{response}");
        }

        for valid in [
            "I'd really enjoy a café au lait on a sunny break! What kind of coffee do you drink?",
            "مرحبا كيف حالك اليوم؟",
        ] {
            let response = json!({ "choices": [{ "finish_reason": "stop", "message": { "content": valid } }] });
            assert_eq!(parse_reply(&response).unwrap(), valid);
        }
    }

    /// Runs `draft_with_fallback` against every `.jpg` / `.jpeg` screenshot in
    /// `SCENARIO_DIR` (optionally filtered by `SCENARIO_FILTER` and `PROVIDER_FILTER`).
    #[test]
    #[ignore]
    fn draft_scenarios_from_folder() {
        let dir = std::env::var("SCENARIO_DIR").expect("set SCENARIO_DIR to a folder of JPEGs");
        let filter = std::env::var("SCENARIO_FILTER").unwrap_or_default();
        let provider_filter = std::env::var("PROVIDER_FILTER").unwrap_or_default();
        let attempts: Vec<Attempt> = PROVIDERS
            .iter()
            .filter(|p| provider_filter.is_empty() || p.name.eq_ignore_ascii_case(&provider_filter))
            .map(|p| Attempt {
                name: p.name,
                url: p.url,
                model: p.model,
                max_tokens: p.max_tokens,
                reasoning_effort: p.reasoning_effort,
                key: saved_key(p),
            })
            .collect();
        let mut paths: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                matches!(
                    path.extension().and_then(|s| s.to_str()),
                    Some("jpg" | "jpeg")
                )
            })
            .collect();
        paths.sort();
        let mut first = true;
        for path in paths {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if !filter.is_empty() && !name.contains(&filter) {
                continue;
            }
            if !first {
                std::thread::sleep(Duration::from_secs(4));
            }
            first = false;
            let bytes = std::fs::read(&path).unwrap();
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            println!("\n=== {name} ===");
            match draft_with_fallback(&attempts, &b64) {
                Ok(reply) => {
                    println!("  Chars: {}", reply.chars().count());
                    println!("  Reply: {reply}");
                }
                Err(error) => {
                    println!("  NO REPLY: {error}");
                }
            }
        }
    }
}
