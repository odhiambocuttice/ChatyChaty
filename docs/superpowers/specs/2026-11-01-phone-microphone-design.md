# Phone microphone over Wi-Fi — design

Date: 2026-11-01
Status: approved by the user, 2026-11-01

## Why

The user wants better audio. A phone held close to the mouth sounds clearer than the laptop microphone, so transcripts are more accurate. The user's setup is an Android phone and a Windows laptop on the same Wi-Fi. It is for the user's own use; it does not need to go upstream.

## What the user sees

1. In Chatelle's **Microphone** setting, the list has one more choice: **Phone (Wi-Fi)**.
2. When it is selected, a QR code and a short web address appear under the list.
3. The user scans the QR code with the phone. Chrome opens a page from Chatelle. The first time, Chrome shows a security warning (the certificate is self-signed). The user taps "Advanced → Proceed" once. Chrome then asks for microphone permission once.
4. The page shows one large **Record** button and a status line ("Connected", "Recording…", "Transcribing…").
5. Tapping the button starts recording in Chatelle, as if the keyboard shortcut were pressed. Tapping again stops it. Chatelle then transcribes and types the text at the cursor on the laptop, as usual.
6. The overlay on the laptop shows the recording and the audio levels, as usual.

## Decisions the user made

- The phone side is a **web page** opened from a QR code. There is no Android app.
- Recording starts and stops with a **button on the phone page**.
- The phone appears as an item **in the Microphone list**, not as a separate toggle.

## Goals

- Phone audio goes through the same pipeline as a normal microphone: VAD, levels, live decoding, transcription, typing.
- Setup is one QR scan, plus a one-time certificate warning and a one-time microphone permission.
- Works with Chrome on Android and Chatelle on Windows. macOS should also work but is not the target.

## Non-goals

- No Android or iOS app.
- No connection over the internet or through a relay server. Same local network only.
- No more than one phone at a time.
- No changes to transcription, typing or the model.

## Anti-goals (a failure even if it works)

- Anyone else on the network can open the page and record or trigger typing on the laptop. The page must require a secret.
- Chatelle listens on the network when the phone microphone is not selected.
- The normal microphone path gets slower or less reliable.

## How it works

### Where the audio enters

Today `AudioRecordingManager` (`src-tauri/src/managers/audio.rs`) opens a `handy_recorder::Recorder<CaptureSink>`. The recorder delivers 16 kHz mono chunks to `CaptureSink` (`src-tauri/src/audio_toolkit/audio/sink.rs`) through the `handy_recorder::Sink` trait. `CaptureSink` does VAD, levels and the live feed.

For the phone, the network code feeds the same `CaptureSink` through the same `Sink::process_chunk` call, instead of a `Recorder`. Everything after `CaptureSink` stays the same. The builder decides the exact shape (for example, an enum with a `Device(Recorder)` and a `Phone(...)` variant inside `OpenMicrophone`). The builder must check how to build a `handy_recorder::AudioChunk` from outside the recorder in the installed `handy-recorder 0.1.0`.

### Selecting the phone

`set_selected_microphone` (`src-tauri/src/commands/audio.rs`) already treats the name `"default"` as a special value. The phone uses another reserved name (for example `"phone"`). `resolve_microphone_device` must not report it as an unavailable device and must not reset it to the default.

`get_available_microphones` adds the phone entry to the list, so `MicrophoneSelector.tsx` shows it without a new control. The QR code and address appear under the selector only while the phone is selected.

### The network server

- It runs **only while "Phone (Wi-Fi)" is selected**. It stops when the user picks another microphone or quits.
- **HTTPS and a secure WebSocket on one port.** Phone browsers allow microphone use only on secure pages, so plain HTTP will not work.
- **Certificate:** self-signed, created on first use and saved in the app data folder. Its subject names the current IP address; create a new one when the IP address changes. The Chrome warning can come back now and then (new IP address, or Chrome forgets the choice). The user accepted this; no certificate is installed on the phone.
- **Secret:** a random token, created once and saved in the app data folder. It is in the QR code's address. The server refuses any page load or WebSocket that does not carry it. A "New code" button replaces the token (and drops the connected phone).
- **Address:** the laptop's local network IP address, plus the port. If there are several network adapters, use the one with a private address on an active Wi-Fi or Ethernet adapter. The builder decides how to detect this.
- **Port:** fixed default (builder's choice). Once a port works, save it and reuse it, because Chrome ties the microphone permission to the port. Fall back to another free port only if the saved one is busy.
- **One phone at a time:** a new connection replaces the old one.
- **Windows Firewall** asks for permission the first time Chatelle listens. Chatelle's UI says so near the QR code.

### The phone page

- Static HTML and JavaScript embedded in the Chatelle binary. No build step and no framework.
- Uses `getUserMedia` with echo cancellation and noise suppression on, and an `AudioWorklet` to resample to 16 kHz mono.
- Sends audio as binary WebSocket messages while connected. Format is the builder's choice (16-bit integers halve the traffic).
- Sends small JSON control messages: `toggle`. Receives state messages: `idle`, `recording`, `transcribing`, `error`.
- Asks for a Screen Wake Lock so the screen stays on. Chrome stops the microphone when the screen turns off.
- Strings on the phone page come from the same English translation file, passed in by the server. Other languages fall back to English.

### The record button

The `toggle` message calls `send_transcription_input(app, "transcribe", "phone")` in `src-tauri/src/signal_handle.rs`. This is the same path as the CLI and the Unix signal, so the coordinator logic is not duplicated. The laptop shortcut keeps working too (my call: it costs nothing and helps when the phone is out of reach).

### Failures

- **Phone disconnects while recording (user's decision):** keep the audio captured so far and stay in the recording state. Chatelle never stops or types on its own. The user stops it with the laptop shortcut, or taps Record on the phone after it reconnects. Audio from a phone that reconnects during the same recording is added to that recording (builder's call how). The overlay or phone page shows that the phone is disconnected.
- **Recording starts with no phone connected:** fail the start with the existing `recording-error` event and a clear message ("Phone not connected — scan the QR code in Settings").
- **Server cannot start** (no network, port problems): show the error under the QR code. Do not fall back to another microphone silently.

## New dependencies

The builder picks the smallest set. Expected: a TLS library, a WebSocket library, a certificate generator, and a QR code generator. Prefer crates that are already in `Cargo.lock` (check `tokio`, `rustls` and others already pulled in by `ureq`/`hf-hub`). Each new crate is noted in the PR.

## Left to the builder

- The exact type that holds either a device recorder or the phone stream.
- Port number, audio wire format, chunk size.
- QR rendering: generate it in Rust (SVG string) or in the frontend.
- How to choose the IP address.

## How to verify

1. `bun run lint`, `bun run format:check`, `cargo clippy`, `cargo test`.
2. Unit tests: the token check refuses bad tokens; selecting `"phone"` is never reset to default by device resolution.
3. Manual test on Windows with an Android phone: select Phone (Wi-Fi), scan, accept warning and permission, record a sentence with the phone button, confirm it is typed at the cursor. Then turn off Wi-Fi on the phone mid-recording: confirm nothing is typed, then press the laptop shortcut and confirm the partial text is typed. Then select a normal microphone and confirm the port is closed.
