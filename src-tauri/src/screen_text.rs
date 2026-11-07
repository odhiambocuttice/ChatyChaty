//! Reads a chat screenshot as text on this computer, so a draft request can
//! send the chat as a few hundred words instead of an image: smaller (it fits
//! Groq's free limit), cheaper, and only the chat text leaves the computer.
//!
//! Each line is labelled by where it sits: chat apps put the user's own
//! messages on the right and the other side's on the left. The screenshot is
//! still sent when this fails or reads too little (see `reply`).

/// One line of recognised text, in coordinates normalised to the image (0 to
/// 1), with `top` measured from the top edge.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub text: String,
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub height: f64,
}

/// Fewer lines than this is not a chat worth reading as text.
const MIN_LINES: usize = 3;

/// Recognises the text in a JPEG with the Vision framework. Runs on this
/// computer; nothing is sent anywhere.
#[cfg(target_os = "macos")]
pub fn recognize(jpeg: &[u8]) -> Result<Vec<Line>, String> {
    use objc2::AllocAnyThread;
    use objc2_foundation::{NSArray, NSData, NSDictionary};
    use objc2_vision::{
        VNImageRequestHandler, VNRecognizeTextRequest, VNRequest, VNRequestTextRecognitionLevel,
    };

    let request = VNRecognizeTextRequest::new();
    request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
    // Chats mix languages and slang; "correcting" them changes what was said.
    request.setUsesLanguageCorrection(false);

    let data = NSData::with_bytes(jpeg);
    let handler = VNImageRequestHandler::initWithData_options(
        VNImageRequestHandler::alloc(),
        &data,
        &NSDictionary::new(),
    );
    let as_request: &VNRequest = &request;
    handler
        .performRequests_error(&NSArray::from_slice(&[as_request]))
        .map_err(|e| format!("text recognition failed: {e}"))?;

    let Some(observations) = request.results() else {
        return Ok(Vec::new());
    };
    let mut lines = Vec::new();
    for observation in observations.iter() {
        let Some(best) = observation.topCandidates(1).firstObject() else {
            continue;
        };
        // Vision sometimes returns "&", "<" and ">" as HTML entities.
        let text = best
            .string()
            .to_string()
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&");
        if text.trim().is_empty() {
            continue;
        }
        // Vision's origin is the lower-left corner.
        let rect = unsafe { observation.boundingBox() };
        lines.push(Line {
            text: text.trim().to_string(),
            left: rect.origin.x,
            right: rect.origin.x + rect.size.width,
            top: 1.0 - (rect.origin.y + rect.size.height),
            height: rect.size.height,
        });
    }
    Ok(lines)
}

#[cfg(not(target_os = "macos"))]
pub fn recognize(_jpeg: &[u8]) -> Result<Vec<Line>, String> {
    Err("text recognition is only built for macOS".to_string())
}

/// The chat as lines tagged with their horizontal span on screen, as a
/// percentage of the width, top to bottom. Lines of one bubble (same left
/// edge, no gap) are joined into one message.
///
/// Fixed Left/Right labels failed on WhatsApp Web, where the chat list fills
/// the left of the window: the list read as the other side, and the other
/// side's messages, which start at the conversation's left edge further in,
/// read as the user's. The positions let the model find the conversation and
/// its two sides itself. None when too little text was read to be worth
/// sending instead of the screenshot.
pub fn transcript(lines: &[Line]) -> Option<String> {
    if lines.len() < MIN_LINES {
        return None;
    }
    let mut sorted: Vec<&Line> = lines.iter().collect();
    sorted.sort_by(|a, b| a.top.total_cmp(&b.top));

    struct Message {
        text: String,
        left: f64,
        right: f64,
        bottom: f64,
    }
    let mut messages: Vec<Message> = Vec::new();
    for line in sorted {
        // A bubble's next line starts at the same left edge with no gap. Other
        // columns (a chat list) interleave, so look back over a few messages.
        let same_bubble = messages
            .iter_mut()
            .rev()
            .take(6)
            .find(|m| (m.left - line.left).abs() < 0.03 && line.top - m.bottom < line.height);
        match same_bubble {
            Some(m) => {
                m.text.push(' ');
                m.text.push_str(&line.text);
                m.right = m.right.max(line.right);
                m.bottom = line.top + line.height;
            }
            None => messages.push(Message {
                text: line.text.clone(),
                left: line.left,
                right: line.right,
                bottom: line.top + line.height,
            }),
        }
    }
    let percent = |x: f64| (x * 100.0).round().clamp(0.0, 100.0) as u32;
    let text = messages
        .iter()
        .map(|m| format!("[{}-{}%] {}", percent(m.left), percent(m.right), m.text))
        .collect::<Vec<_>>()
        .join("\n");
    Some(text)
}

/// Reads the screenshot and returns its transcript, or None when there is no
/// usable text (or no text recognition on this platform).
/// What reading a screenshot found: the chat as text, and where the message
/// box is (left, top, right, bottom, normalised to the picture).
#[derive(Debug, Clone, Default)]
pub struct ChatRead {
    pub transcript: Option<String>,
    pub input_box: Option<(f64, f64, f64, f64)>,
    pub stage: Option<ChatStage>,
}

/// Whether the chat on screen is new or already going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatStage {
    /// The chat's start is on screen: its "THE CHAT STARTED" divider, or the
    /// "POKE" label on the opening message.
    New,
    /// Older messages are scrolled away: the chat site pins a "Started at …
    /// — 2 months ago" label at the top of the chat.
    Ongoing,
}

/// The chat site's chat ID, shown beside a small flag under the chat (such as
/// "7a69a548cb4441e49235"): one 20-character word of letters and digits with
/// at least one of each. The lowest one on screen wins. It keys the memory of
/// what was already asked in this chat, including what has scrolled away.
pub fn chat_id(text: &str) -> Option<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .rfind(|w| {
            w.len() == 20
                && w.chars().any(|c| c.is_ascii_digit())
                && w.chars().any(|c| c.is_ascii_alphabetic())
        })
        .map(str::to_string)
}

/// Reads the chat site's markers in the text on screen. The divider wins over
/// the pinned label, because the divider shows only when the chat's start is
/// on screen. None when no marker was read; the model then judges itself.
pub fn chat_stage(text: &str) -> Option<ChatStage> {
    let lines: Vec<String> = text.lines().map(|l| l.trim().to_lowercase()).collect();
    if lines.iter().any(|l| l.contains("the chat started")) {
        Some(ChatStage::New)
    } else if lines.iter().any(|l| l.contains("started at")) {
        Some(ChatStage::Ongoing)
    } else if lines.iter().any(|l| l == "poke") {
        Some(ChatStage::New)
    } else {
        None
    }
}

/// Placeholders chat apps show in an empty message box.
const INPUT_PLACEHOLDERS: [&str; 7] = [
    "type a message",
    "write a message",
    "type your message",
    "send a message",
    "message...",
    "message…",
    "message",
];

/// The empty message box, found by its placeholder: the lowest line that
/// reads like one. None when the box already has text in it.
pub fn input_box(lines: &[Line]) -> Option<(f64, f64, f64, f64)> {
    lines
        .iter()
        .filter(|line| {
            let text = line.text.trim().to_lowercase();
            INPUT_PLACEHOLDERS
                .iter()
                .any(|p| text == *p || (p.len() > 7 && text.starts_with(p)))
        })
        .max_by(|a, b| a.top.total_cmp(&b.top))
        .map(|line| (line.left, line.top, line.right, line.top + line.height))
}

/// Reads the screenshot: its transcript and message box. Empty when there is
/// no usable text (or no text recognition on this platform).
pub fn read_chat(jpeg: &[u8]) -> ChatRead {
    match recognize(jpeg) {
        Ok(lines) => {
            let transcript = transcript(&lines);
            let text = lines
                .iter()
                .map(|l| l.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            // The pinned "Started at" label is usually cut in half at the top
            // and not read, while the full-width divider of a new chat is.
            // So a chat read without the divider is taken as ongoing.
            let stage = chat_stage(&text).or(transcript.is_some().then_some(ChatStage::Ongoing));
            ChatRead {
                transcript,
                input_box: input_box(&lines),
                stage,
            }
        }
        Err(e) => {
            log::debug!("Reading the screenshot as text failed: {e}");
            ChatRead::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, left: f64, right: f64, top: f64) -> Line {
        Line {
            text: text.to_string(),
            left,
            right,
            top,
            height: 0.03,
        }
    }

    #[test]
    fn tags_positions_and_joins_lines_of_one_bubble() {
        let lines = [
            line("Makes sense on the saving on the rent", 0.6, 0.95, 0.50),
            line("Good luck in Rwanda", 0.6, 0.8, 0.53),
            // A chat list row beside the conversation does not join it.
            line("Dad Yesterday", 0.02, 0.25, 0.51),
            line("I will bro", 0.32, 0.45, 0.70),
        ];
        assert_eq!(
            transcript(&lines).unwrap(),
            "[60-95%] Makes sense on the saving on the rent Good luck in Rwanda\n[2-25%] Dad Yesterday\n[32-45%] I will bro"
        );
    }

    /// Reads a real screenshot: `CHATELLE_OCR_IMAGE=/path/to.jpg cargo test
    /// read_a_real_screenshot -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn read_a_real_screenshot() {
        let path = std::env::var("CHATELLE_OCR_IMAGE").expect("set CHATELLE_OCR_IMAGE");
        let jpeg = std::fs::read(path).unwrap();
        let chat = read_chat(&jpeg);
        println!("{}", chat.transcript.expect("no text read"));
        println!("input box: {:?}", chat.input_box);
        println!("stage: {:?}", chat.stage);
    }

    #[test]
    fn finds_the_chat_id_under_the_chat() {
        let text = "Balls deep.\n11:56 | 12:56 Sun, Nov 1, 2026 — a few seconds ago\noGtJe3dTc0zL5Cwt49oa\nType your reply here...";
        assert_eq!(chat_id(text).as_deref(), Some("oGtJe3dTc0zL5Cwt49oa"));
        assert_eq!(
            chat_id("7a69a548cb4441e49235").as_deref(),
            Some("7a69a548cb4441e49235")
        );
        // Ordinary words, times and dates are never an ID.
        assert_eq!(
            chat_id("Sitting by the water as the sun goes down 12:49 Sun, Nov 1, 2026"),
            None
        );
        assert_eq!(chat_id("aaaaaaaaaaaaaaaaaaaa 12345678901234567890"), None);
    }

    #[test]
    fn reads_the_chat_stage_from_the_sites_markers() {
        // The new chat in a real screenshot: the divider, then the opener.
        assert_eq!(
            chat_stage("THE CHAT STARTED\n00:10 — 21 minutes ago\nPOKE\nDAMN! I should be in bed."),
            Some(ChatStage::New)
        );
        assert_eq!(
            chat_stage("POKE\nAre you single?\nI maynot be lol"),
            Some(ChatStage::New)
        );
        // An ongoing chat: the label pinned at the top.
        assert_eq!(
            chat_stage("Started at 21:02 18-Jun-2026 — a month ago\nNo i can't unfortunately."),
            Some(ChatStage::Ongoing)
        );
        assert_eq!(chat_stage("Hey darling\nWhat have you been doing?"), None);
    }

    #[test]
    fn finds_the_message_box_by_its_placeholder() {
        let lines = [
            line("Dennis Bett", 0.4, 0.5, 0.05),
            line("Type a message", 0.41, 0.49, 0.95),
            line("I will bro", 0.39, 0.43, 0.80),
        ];
        let (left, top, ..) = input_box(&lines).unwrap();
        assert_eq!((left, top), (0.41, 0.95));
        assert!(input_box(&lines[..1]).is_none());
    }

    #[test]
    fn too_little_text_is_not_a_transcript() {
        assert!(transcript(&[line("Hi", 0.1, 0.2, 0.1)]).is_none());
    }
}
