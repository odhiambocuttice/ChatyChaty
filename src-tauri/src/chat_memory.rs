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
/// Longer messages are cut, so one long message cannot crowd out the rest.
const SAMPLE_MAX_CHARS: usize = 300;

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
struct Memory {
    style_samples: Vec<String>,
}

static PATH: OnceLock<PathBuf> = OnceLock::new();
static MEMORY: Mutex<Memory> = Mutex::new(Memory {
    style_samples: Vec::new(),
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
