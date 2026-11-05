# Feature ideas

A list of things Chatelle could do next. These are ideas, not commitments. Each one names the code it would touch and its rough size.

**Size:** S = a day or less, M = a few days, L = a week or more.

**Status:** 💡 idea · 🚧 in progress · ✅ done · ❌ dropped

> Upstream [cjpais/Handy](https://github.com/cjpais/Handy) is under a feature freeze. That only matters if a feature is sent back upstream; this fork can add anything.

## Suggested order

1. Speak a hint before drafting (#2)
2. Preview before typing (#3)
3. Undo the last dictation (#8)
4. Tone presets (#1)

## Reply drafting

Code: [`src-tauri/src/reply.rs`](../src-tauri/src/reply.rs)

| #   | Feature                      | What it does                                                                                                                            | Size | Status |
| --- | ---------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- | ---- | ------ |
| 1   | Tone presets                 | Friendly, formal, short, decline politely. Each preset is its own shortcut or a quick picker, instead of editing the prompt.            | S    | 💡     |
| 2   | Speak a hint before drafting | Hold the reply shortcut and say what you want ("say yes but suggest Friday"). The transcript goes into the prompt as the user's intent. | M    | 💡     |
| 3   | Preview before typing        | Show the draft in the overlay. Enter types it, Esc throws it away. Stops a bad draft being typed into the chat box.                     | M    | 💡     |
| 4   | Regenerate shortcut          | Delete the draft that was just typed and type a new one.                                                                                | S    | 💡     |
| 5   | Local reply model            | An optional Ollama or LM Studio endpoint, added as another OpenAI-compatible entry in `PROVIDERS`. Private, and no free-tier limits.    | S    | 💡     |

### Notes

- **#2** reuses the speech model that is already loaded, so it needs no new model. The work is a new shortcut mode in [`actions.rs`](../src-tauri/src/actions.rs) that records, transcribes, and passes the text to the reply prompt instead of typing it.
- **#3** needs a draft state in the overlay ([`overlay.rs`](../src-tauri/src/overlay.rs), [`src/overlay/`](../src/overlay/)) and temporary Enter/Esc shortcuts while the preview is shown.
- **#4** needs the length of the last typed text to be remembered, so it can be deleted with Backspace. #8 needs the same thing.

## Dictation

Code: [`src-tauri/src/actions.rs`](../src-tauri/src/actions.rs), [`src-tauri/src/input.rs`](../src-tauri/src/input.rs), [`src-tauri/src/clipboard.rs`](../src-tauri/src/clipboard.rs)

| #   | Feature                        | What it does                                                                                  | Size | Status |
| --- | ------------------------------ | --------------------------------------------------------------------------------------------- | ---- | ------ |
| 6   | Voice commands                 | "new line", "period", "delete that", "scratch that" while dictating.                          | M    | 💡     |
| 7   | Custom vocabulary/replacements | Fix names and jargon the model keeps getting wrong, such as "chat elle" → "Chatelle".         | S    | 💡     |
| 8   | Undo the last dictation        | One shortcut that backspaces exactly what was just typed.                                     | S    | 💡     |
| 9   | Per-app typing speed           | Change the typing speed based on the app in front, for web apps that drop keys at high speed. | M    | 💡     |

### Notes

- **#6** and **#7** run on the transcript text before it is typed. One small text-processing step could handle both.
- **#9** needs to know which app is in front (macOS `NSWorkspace`, Windows `GetForegroundWindow`), plus a per-app list in settings.

## Phone microphone

Code: [`src-tauri/src/phone/`](../src-tauri/src/phone/), [`src/components/settings/PhoneMicrophonePanel.tsx`](../src/components/settings/PhoneMicrophonePanel.tsx)

| #   | Feature                          | What it does                                                               | Size | Status |
| --- | -------------------------------- | -------------------------------------------------------------------------- | ---- | ------ |
| 10  | Trigger from the phone           | A big start/stop button on the phone page, so the phone works as a remote. | S    | 💡     |
| 11  | Show the transcript on the phone | Send the result back over the WebSocket so it can be checked on the phone. | S    | 💡     |

## Quality of life

| #   | Feature                        | What it does                                                                                                             | Size | Status |
| --- | ------------------------------ | ------------------------------------------------------------------------------------------------------------------------ | ---- | ------ |
| 12  | Recent transcripts in the tray | A short in-memory list (nothing saved to disk) so you can re-type something. Code: [`tray.rs`](../src-tauri/src/tray.rs) | M    | 💡     |
| 13  | Provider status in the tray    | Show which reply provider was used last and whether its free limit has run out.                                          | S    | 💡     |

## Remote reply

| #   | Feature                    | What it does                                                                                                                                                                                           | Size | Status |
| --- | -------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---- | ------ |
| 14  | Remote reply over Telegram | Watch an open chat while you're away. New messages and a draft reply go to your Telegram, and tapping Send makes the laptop type and send it. Design: [REMOTE_REPLY_DESIGN.md](REMOTE_REPLY_DESIGN.md) | L    | 🚧     |

## Adding an idea

Add a row to the right table with the next number, a one-line description, a size and 💡. Put longer thoughts under that section's **Notes**.
