//! The user's tone, kept in `chat_memory.json` in the app data folder: messages
//! the user wrote themselves (not ones Chatelle typed), so drafts match how the
//! user writes. Nothing about the other side is stored, and the screenshot is
//! never kept.
//!
//! The samples come back from the model with each draft (see
//! `reply::Trailers`).

use log::warn;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// How many of the user's own messages to keep as tone samples.
const STYLE_SAMPLES_KEPT: usize = 30;
/// How many chats, and closing questions per chat, to remember. A chat is
/// keyed by the ID the chat site shows under it, so a question asked an hour
/// ago, now scrolled out of view, is still never asked again.
const CHATS_KEPT: usize = 300;
const QUESTIONS_PER_CHAT: usize = 40;
/// Longer messages are cut, so one long message cannot crowd out the rest.
const SAMPLE_MAX_CHARS: usize = 300;

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
struct Memory {
    style_samples: Vec<String>,
    /// Per chat, oldest chat first: the closing questions Chatelle already
    /// typed there.
    #[serde(default)]
    chats: Vec<ChatHistory>,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
struct ChatHistory {
    id: String,
    questions: Vec<String>,
}

static PATH: OnceLock<PathBuf> = OnceLock::new();
static MEMORY: Mutex<Memory> = Mutex::new(Memory {
    style_samples: Vec::new(),
    chats: Vec::new(),
});

/// Loads the memory file from `dir`. Called once at startup; a missing or
/// unreadable file starts an empty memory.
pub fn init(dir: PathBuf) {
    let path = dir.join("chat_memory.json");
    if let Ok(text) = std::fs::read_to_string(&path) {
        match serde_json::from_str::<Memory>(&text) {
            Ok(memory) => *MEMORY.lock().unwrap_or_else(|e| e.into_inner()) = memory,
            Err(e) => warn!("Could not read {}: {e}", path.display()),
        }
    }
    let _ = PATH.set(path);
}

/// The user's newest own messages, oldest first.
pub fn style_samples(count: usize) -> Vec<String> {
    let memory = MEMORY.lock().unwrap_or_else(|e| e.into_inner());
    let skip = memory.style_samples.len().saturating_sub(count);
    memory.style_samples[skip..].to_vec()
}

/// Keeps those of the user's visible messages that Chatelle did not write.
/// `written_by_chatelle` checks a message against recent drafts.
pub fn remember_own_messages(my_messages: &[String], written_by_chatelle: impl Fn(&str) -> bool) {
    let mut added = false;
    {
        let mut memory = MEMORY.lock().unwrap_or_else(|e| e.into_inner());
        for message in my_messages {
            let sample: String = message.trim().chars().take(SAMPLE_MAX_CHARS).collect();
            if sample.chars().count() < 3
                || written_by_chatelle(&sample)
                || memory.style_samples.contains(&sample)
            {
                continue;
            }
            memory.style_samples.push(sample);
            added = true;
        }
        let excess = memory
            .style_samples
            .len()
            .saturating_sub(STYLE_SAMPLES_KEPT);
        memory.style_samples.drain(..excess);
    }
    if added {
        save();
    }
}

/// The closing questions already typed in the chat with this ID, oldest first.
pub fn chat_questions(chat_id: &str) -> Vec<String> {
    let memory = MEMORY.lock().unwrap_or_else(|e| e.into_inner());
    memory
        .chats
        .iter()
        .find(|c| c.id == chat_id)
        .map(|c| c.questions.clone())
        .unwrap_or_default()
}

/// Remembers a closing question typed in the chat with this ID.
pub fn remember_chat_question(chat_id: &str, question: &str) {
    let question = question.trim();
    if chat_id.is_empty() || question.is_empty() {
        return;
    }
    {
        let mut memory = MEMORY.lock().unwrap_or_else(|e| e.into_inner());
        // The chat moves to the end as the most recent one.
        let mut chat = match memory.chats.iter().position(|c| c.id == chat_id) {
            Some(i) => memory.chats.remove(i),
            None => ChatHistory {
                id: chat_id.to_string(),
                questions: Vec::new(),
            },
        };
        if !chat.questions.iter().any(|q| q == question) {
            chat.questions.push(question.to_string());
        }
        let excess = chat.questions.len().saturating_sub(QUESTIONS_PER_CHAT);
        chat.questions.drain(..excess);
        memory.chats.push(chat);
        let excess = memory.chats.len().saturating_sub(CHATS_KEPT);
        memory.chats.drain(..excess);
    }
    save();
}

fn save() {
    let Some(path) = PATH.get() else {
        return;
    };
    let text = {
        let memory = MEMORY.lock().unwrap_or_else(|e| e.into_inner());
        serde_json::to_string_pretty(&*memory)
    };
    match text {
        Ok(text) => {
            if let Err(e) = std::fs::write(path, text) {
                warn!("Could not save {}: {e}", path.display());
            }
        }
        Err(e) => warn!("Could not encode the tone samples: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_questions_per_chat() {
        remember_chat_question("chatA0000000000000001", "What did you cook tonight?");
        remember_chat_question("chatA0000000000000001", "What did you cook tonight?");
        remember_chat_question("chatA0000000000000001", "Where is your favourite walk?");
        remember_chat_question("chatB0000000000000002", "How was the match?");
        assert_eq!(
            chat_questions("chatA0000000000000001"),
            vec![
                "What did you cook tonight?".to_string(),
                "Where is your favourite walk?".to_string()
            ]
        );
        assert_eq!(chat_questions("chatB0000000000000002").len(), 1);
        assert!(chat_questions("unknown").is_empty());
    }

    #[test]
    fn keeps_only_the_users_own_messages() {
        remember_own_messages(
            &[
                "haha same, I'm so tired today".to_string(),
                "Typed by Chatelle earlier".to_string(),
                "ok".to_string(),
            ],
            |m| m.starts_with("Typed by Chatelle"),
        );
        let samples = style_samples(10);
        assert!(samples.contains(&"haha same, I'm so tired today".to_string()));
        assert!(!samples.iter().any(|s| s.starts_with("Typed by")));
        assert!(!samples.contains(&"ok".to_string()));
    }
}
