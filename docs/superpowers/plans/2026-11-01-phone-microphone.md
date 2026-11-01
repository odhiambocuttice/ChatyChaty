# Phone Microphone over Wi-Fi Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let Chatelle use an Android phone's microphone over the local Wi-Fi, chosen as "Phone (Wi-Fi)" in the Microphone list, with a Record button on the phone page.

**Architecture:** A new `src-tauri/src/phone/` module runs an HTTPS + WebSocket server only while the phone microphone is selected. The phone page streams 16 kHz mono PCM; a `PhoneCapture` buffer cuts it into VAD-sized `AudioChunk`s and feeds the same `CaptureSink` the device recorder feeds. `AudioRecordingManager` holds either a device `Recorder` or a `PhoneCapture`, so everything after `CaptureSink` (VAD, levels, live decode, transcription, typing) is unchanged. The Record button calls the existing `send_transcription_input`.

**Tech Stack:** Rust (tokio, rustls 0.23 + tokio-rustls 0.26 already in `Cargo.lock`; new: `tokio-tungstenite`, `rcgen`, `qrcode`), plain HTML/JS phone page (`getUserMedia`, `AudioWorklet`, Wake Lock), React + i18next settings UI.

**Spec:** `docs/superpowers/specs/2026-11-01-phone-microphone-design.md`

## Global Constraints

- User-facing name is **Chatelle**, never "Handy" (code identifiers may keep `handy`).
- Work on a new branch `feat/phone-microphone` off `main`. **Commit only after the user says to** (user's global rule); the commit steps below mark the points where a commit belongs.
- Reserved microphone name: `PHONE_MICROPHONE = "phone-wifi"` (stored in `settings.selected_microphone`).
- Server runs only while `selected_microphone == Some("phone-wifi")`; it is stopped on any other selection and on quit.
- Every page load and WebSocket must carry the token (`?t=<token>`); otherwise respond `403` and close. Compare tokens in constant time.
- Wire format: binary WebSocket message = 16-bit signed little-endian PCM, 16 000 Hz, mono. Text messages are JSON.
- Phone disconnect never stops, cancels or types on its own. A reconnecting phone feeds the same recording.
- Save the first working port in settings and reuse it; pick another free port only if it is busy.
- Certificate is self-signed for the current LAN IPv4 and regenerated when the IP changes. No CA install on the phone.
- Crate versions in this plan are unverified starting points. Add each with `cargo add` and pick the newest release that builds against the lockfile's `rustls 0.23` / `ring 0.17`. Use the rustls crypto provider the lockfile already enables; do not add `aws-lc-rs` if it is not already there.
- New UI strings go in `src/i18n/locales/en/translation.json` only; other locales fall back.
- Before each commit: `bun run lint`, `bun run format:check`, `cd src-tauri && cargo clippy --all-targets && cargo test`.

## Review Focus

1. **Laptop has several network adapters (VPN, Hyper-V, WSL):** the QR must use the Wi-Fi/Ethernet private address the phone can reach, not a VPN or virtual one. → `local_ipv4_prefers_private` test in Task 3; manual check in Task 8.
2. **Phone screen turns off / Chrome goes to background mid-recording:** audio stops, recording stays open, pressing the laptop shortcut types what was heard. → `stop_after_disconnect_keeps_audio` test in Task 1.
3. **Second phone or a second tab opens the page:** the new connection replaces the old one and the old one gets closed; audio is never mixed from two sources. → `new_connection_replaces_old` test in Task 4.
4. **Phone not connected when the user presses the laptop shortcut:** the start fails with the "Phone not connected" message, and no reopen loop runs. → `start_without_phone_fails` test in Task 1, error mapping in Task 6.
5. **Overlay disabled in settings:** the phone page still shows Recording/Transcribing, because the state hook is placed before any overlay-setting early return. → step in Task 5.

---

### Task 1: `PhoneCapture` — cut streamed audio into chunks for a sink

**Files:**
- Create: `src-tauri/src/phone/mod.rs` (module root; for now only `pub mod capture;`)
- Create: `src-tauri/src/phone/capture.rs`
- Modify: `src-tauri/src/lib.rs` (add `mod phone;`)

**Interfaces:**
- Produces:
  - `pub const PHONE_SAMPLE_RATE: u32 = 16_000;`
  - `pub enum PhoneStartError { NotConnected, AlreadyRecording }` (derive `Debug`, `thiserror`-free `Display`: "Phone not connected — scan the QR code in Settings" / "Already recording")
  - `pub struct PhoneCapture<S: Sink>` — `Clone` (inner `Arc<Mutex<..>>`)
  - `pub fn new(frames_per_chunk: usize) -> Self`
  - `pub fn set_frames_per_chunk(&self, frames: usize)` — ignored while recording (the VAD frame size can change with the VAD backend).
  - `pub fn set_connected(&self, connected: bool)`
  - `pub fn is_connected(&self) -> bool`
  - `pub fn start(&self, sink: S) -> Result<(), (PhoneStartError, S)>`
  - `pub fn push_samples(&self, samples: &[f32])` — appends to a buffer; while recording, sends every full `frames_per_chunk` block as an `AudioChunk { sample_rate: 16000, channels: 1, valid_frames: frames_per_chunk }`; while not recording, drops input.
  - `pub fn stop(&self) -> Option<S>` — sends the remainder as one zero-padded chunk with `valid_frames = remainder`, clears the buffer, returns the sink.
  - `pub fn decode_pcm16le(bytes: &[u8]) -> Vec<f32>` — `i16 / 32768.0`; an odd trailing byte is ignored.

- [ ] **Step 1: Write the failing tests** in `capture.rs` `#[cfg(test)] mod tests`, using `handy_recorder::CollectingSink`:

```rust
#[test] fn start_without_phone_fails() {
    let c = PhoneCapture::new(512);
    let (err, _sink) = c.start(CollectingSink::new()).unwrap_err();
    assert!(matches!(err, PhoneStartError::NotConnected));
}
#[test] fn chunks_and_flushes_remainder() {
    let c = PhoneCapture::new(512); c.set_connected(true);
    c.start(CollectingSink::new()).unwrap();
    c.push_samples(&vec![0.25; 700]);
    assert_eq!(c.stop().unwrap().into_samples(), vec![0.25; 700]);
}
#[test] fn second_start_is_rejected() { /* start twice → AlreadyRecording */ }
#[test] fn audio_before_start_is_dropped() {
    let c = PhoneCapture::new(4); c.set_connected(true);
    c.push_samples(&[1.0; 10]);
    c.start(CollectingSink::new()).unwrap();
    c.push_samples(&[0.5; 4]);
    assert_eq!(c.stop().unwrap().into_samples(), vec![0.5; 4]);
}
#[test] fn stop_after_disconnect_keeps_audio() {
    let c = PhoneCapture::new(4); c.set_connected(true);
    c.start(CollectingSink::new()).unwrap();
    c.push_samples(&[0.5; 6]); c.set_connected(false);
    c.set_connected(true); c.push_samples(&[0.5; 2]); // reconnect, same recording
    assert_eq!(c.stop().unwrap().into_samples(), vec![0.5; 8]);
}
#[test] fn stop_when_idle_returns_none() { assert!(PhoneCapture::<CollectingSink>::new(4).stop().is_none()); }
#[test] fn decodes_pcm16le() {
    assert_eq!(decode_pcm16le(&[0x00, 0x40, 0x00, 0xC0, 0x01]), vec![0.5, -0.5]);
}
```

- [ ] **Step 2: Run** `cd src-tauri && cargo test phone::capture` — expect compile failure (types missing).
- [ ] **Step 3: Implement** the interfaces above. Disconnect only flips the flag; it never touches the sink.
- [ ] **Step 4: Run** `cargo test phone::capture` — expect 7 passed.
- [ ] **Step 5: Commit point** — `feat: add phone audio chunker for the phone microphone`.

---

### Task 2: Protocol and phone page

**Files:**
- Create: `src-tauri/src/phone/protocol.rs`
- Create: `src-tauri/src/phone/page.html`
- Modify: `src-tauri/src/phone/mod.rs` (`pub mod protocol;`)
- Modify: `src/i18n/locales/en/translation.json` (add `phonePage` block, below)

**Interfaces:**
- Produces:
  - `pub enum PhoneState { Idle, Recording, Transcribing }` — `Serialize` as lowercase strings.
  - `pub enum ClientMessage { Toggle }` — from `{"type":"toggle"}`; `pub fn parse_client_message(text: &str) -> Option<ClientMessage>`.
  - `pub fn state_message(state: PhoneState) -> String` → `{"type":"state","state":"recording"}`.
  - `pub struct RequestHead { pub path: String, pub token: Option<String>, pub websocket_key: Option<String> }`; `pub fn parse_request_head(head: &str) -> Option<RequestHead>` — GET only; reads `t` from the query; `websocket_key` set only when `Upgrade: websocket` (case-insensitive) and `Sec-WebSocket-Key` are present.
  - `pub fn token_matches(expected: &str, given: &str) -> bool` — constant time over equal lengths, false on length mismatch.
  - `pub fn render_page(token: &str) -> String` — `include_str!("page.html")` with `__TOKEN__` and `__STRINGS__` (JSON of the `phonePage` object, read from `include_str!("../../../src/i18n/locales/en/translation.json")`) replaced.

- [ ] **Step 1: Write failing tests**: `parses_page_request` (`"GET /?t=abc HTTP/1.1\r\nHost: x\r\n\r\n"` → path `/`, token `Some("abc")`, key `None`); `parses_websocket_request` (path `/ws`, key `Some("dGhlIHNhbXBsZSBub25jZQ==")`); `rejects_post`; `token_mismatch_and_length` (`token_matches("abcd","abce") == false`, `("abcd","abc") == false`, `("abcd","abcd") == true`); `toggle_parses` / `unknown_type_is_none`; `state_message_shape` (exact JSON above); `page_has_token_and_strings` (`render_page("tok")` contains `"tok"` and `"Record"` and no `__TOKEN__`).
- [ ] **Step 2: Run** `cargo test phone::protocol` — fails.
- [ ] **Step 3: Implement** `protocol.rs`.
- [ ] **Step 4: Write `page.html`** (no framework, no external URLs):
  - One large Record button, a status line. Strings from `__STRINGS__`.
  - On load: open `wss://<location.host>/ws?t=__TOKEN__`; reconnect every 2 s on close, status "Reconnecting…".
  - First tap: `getUserMedia({audio:{echoCancellation:true,noiseSuppression:true,channelCount:1}})`, create `AudioContext`, load an inline `AudioWorklet` (Blob URL) that resamples to 16 kHz by linear interpolation and posts `Int16Array` blocks of ~20 ms; main thread sends them as binary while the socket is open. Request `navigator.wakeLock.request("screen")` and re-request on `visibilitychange`.
  - Each tap sends `{"type":"toggle"}`. Incoming `state` messages set the button label and status.
- [ ] **Step 5: Add strings** to `translation.json` at top level:

```json
"phonePage": {
  "record": "Record", "stop": "Stop",
  "connected": "Connected", "reconnecting": "Reconnecting…",
  "recording": "Recording…", "transcribing": "Transcribing…",
  "micDenied": "Chrome blocked the microphone. Allow it in the site settings and reload."
}
```

- [ ] **Step 6: Run** `cargo test phone::protocol` — pass. **Commit point** — `feat: add phone page and its message protocol`.

---

### Task 3: Address, certificate and token

**Files:**
- Create: `src-tauri/src/phone/identity.rs`
- Modify: `src-tauri/Cargo.toml` — add `rcgen = "0.13"` (default features use `ring`, already in the lockfile), `rand = "0.9"` (already in the lockfile).
- Modify: `src-tauri/src/phone/mod.rs` (`pub mod identity;`)

**Interfaces:**
- Produces:
  - `pub fn local_ipv4() -> Option<Ipv4Addr>` — bind a `std::net::UdpSocket` to `0.0.0.0:0`, `connect("192.168.0.1:9")` (no packet is sent), read `local_addr()`; return it only if `is_private()`. Wrap the choice in `pub fn pick_private(candidate: Option<Ipv4Addr>) -> Option<Ipv4Addr>` for testing.
  - `pub struct TlsIdentity { pub cert_chain: Vec<rustls::pki_types::CertificateDer<'static>>, pub key: rustls::pki_types::PrivateKeyDer<'static> }`
  - `pub fn load_or_create_identity(dir: &Path, ip: Ipv4Addr) -> anyhow::Result<TlsIdentity>` — files `phone-cert.pem`, `phone-key.pem`, `phone-ip.txt` in `dir`; reuse when `phone-ip.txt == ip`, else regenerate with SAN `IpAddress(ip)` and overwrite all three.
  - `pub fn new_token() -> String` — 32 lowercase hex chars from `rand::random::<[u8; 16]>()`.

- [ ] **Step 1: Write failing tests**: `local_ipv4_prefers_private` (`pick_private(Some(10.0.0.5))` → same; `Some(8.8.8.8)` → `None`; `None` → `None`); `reuses_cert_for_same_ip` (tempdir, two calls with `192.168.1.10` → identical cert bytes); `regenerates_cert_when_ip_changes` (second call with `192.168.1.11` → different bytes, `phone-ip.txt` updated); `token_is_32_hex` and two tokens differ.
- [ ] **Step 2: Run** `cargo test phone::identity` — fails.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** — pass. **Commit point** — `feat: add LAN address and self-signed certificate for the phone microphone`.

---

### Task 4: The HTTPS + WebSocket server

**Files:**
- Create: `src-tauri/src/phone/server.rs`
- Modify: `src-tauri/Cargo.toml` — main `tokio` gets `features = ["net", "io-util", "sync", "time", "macros"]`; add `tokio-tungstenite = "0.26"` (no TLS features; we wrap the stream ourselves); `tokio-rustls = "0.26"` direct dependency.
- Modify: `src-tauri/src/phone/mod.rs` (`pub mod server;`)

**Interfaces:**
- Consumes: `PhoneCapture<S>` (Task 1), `protocol::*` (Task 2), `TlsIdentity` (Task 3).
- Produces:
  - `pub struct ServerConfig<S: Sink> { pub port: u16, pub token: String, pub identity: TlsIdentity, pub capture: PhoneCapture<S>, pub on_toggle: Arc<dyn Fn() + Send + Sync>, pub state: tokio::sync::watch::Receiver<PhoneState>, pub on_connection_change: Arc<dyn Fn(bool) + Send + Sync> }`
  - `pub struct RunningServer { pub port: u16, /* shutdown handle */ }` with `pub fn stop(self)` (aborts accept loop and the live connection; sets capture disconnected).
  - `pub async fn start_server<S: Sink>(config: ServerConfig<S>) -> anyhow::Result<RunningServer>` — binds `0.0.0.0:port`; if busy and `port != 0`, retries once with port `0`; returns the real port.

Behaviour:
- Accept TLS, read the request head up to `\r\n\r\n` (max 8 KiB), `parse_request_head`, check `token_matches`; on failure write `HTTP/1.1 403 Forbidden` and close.
- `/` → `200` `text/html; charset=utf-8`, `Cache-Control: no-store`, body `render_page(token)`.
- `/ws` → write `101 Switching Protocols` with `Sec-WebSocket-Accept: tungstenite::handshake::derive_accept_key(key)`, then `WebSocketStream::from_raw_socket(stream, Role::Server, None)`.
- One phone at a time: a new `/ws` connection closes the previous one (keep a connection id; the older task exits when its id is stale).
- On connect: `capture.set_connected(true)`, `on_connection_change(true)`, send the current `state_message`. On each `state` change: send it. Binary → `capture.push_samples(&decode_pcm16le(..))`. Text `Toggle` → `on_toggle()`. Send a ping every 5 s; if nothing arrives for 15 s, close. On close of the *current* connection: `set_connected(false)`, `on_connection_change(false)`.

- [ ] **Step 1: Write failing tests** (`#[tokio::test]`, rustls client with a test-only verifier that accepts any certificate, identity from `load_or_create_identity(tempdir, 127.0.0.1)`, `port: 0`):
  - `bad_token_gets_403` — `GET /?t=wrong` → status line starts with `HTTP/1.1 403`.
  - `page_served_with_token` — `GET /?t=<token>` → `200` and body contains `Record`.
  - `audio_reaches_capture` — WebSocket with token, capture started with a `CollectingSink`, send binary `[0x00,0x40]` × 4, wait until `capture.is_connected()`, short sleep, `stop()` → 4 samples of `0.5` (chunk size 4).
  - `toggle_calls_callback` — send `{"type":"toggle"}` → an `AtomicUsize` counter becomes 1.
  - `new_connection_replaces_old` — open two sockets; the first receives a close frame; `is_connected()` stays true.
- [ ] **Step 2: Run** `cargo test phone::server` — fails.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** — 5 passed. **Commit point** — `feat: add HTTPS and WebSocket server for the phone microphone`.

---

### Task 5: `PhoneMicrophone` state, settings, commands, state hook

**Files:**
- Modify: `src-tauri/src/phone/mod.rs`
- Modify: `src-tauri/src/settings.rs` — add `#[serde(default)] pub phone_microphone_token: Option<String>` and `#[serde(default)] pub phone_microphone_port: Option<u16>` (defaults `None`).
- Modify: `src-tauri/src/commands/audio.rs` — `get_available_microphones` appends `AudioDevice { index: "phone-wifi", name: "phone-wifi", is_default: false }` after the system devices; `set_selected_microphone` calls `phone.sync(&app)` after saving.
- Create: `src-tauri/src/commands/phone.rs`; modify `src-tauri/src/commands/mod.rs`.
- Modify: `src-tauri/src/lib.rs` — `app_handle.manage(Arc<PhoneMicrophone>)` before `AudioRecordingManager::new`; call `sync` once after setup; register the two commands; add `qrcode = { version = "0.14", default-features = false, features = ["svg"] }` to `Cargo.toml`.
- Modify: `src-tauri/src/overlay.rs` — first line of `show_recording_overlay` and `show_streaming_overlay`: `phone::notify_state(app_handle, PhoneState::Recording)`; `show_transcribing_overlay`: `Transcribing`; `hide_recording_overlay`: `Idle`. Place them before any early return.

**Interfaces:**
- Produces (in `phone/mod.rs`):
  - `pub const PHONE_MICROPHONE: &str = "phone-wifi";`
  - `pub struct PhoneMicrophone` with `pub fn new() -> Self` (holds one shared `PhoneCapture<CaptureSink>`, created with chunk size 512; Task 6 sets the real size)
  - `pub fn capture(&self) -> PhoneCapture<CaptureSink>` (a clone of the shared instance)
  - `pub async fn sync(&self, app: &AppHandle)` — reads settings; if phone selected and not running: get IP (`local_ipv4`), token (create + save if missing), identity in `portable::app_data_dir(app)`, `start_server` with port `phone_microphone_port.unwrap_or(47615)`, save the real port; `on_toggle` = `signal_handle::send_transcription_input(&app, "transcribe", "phone")`; if not selected and running: stop. Errors are stored in `status().error`, never panic. Emits `phone-microphone-changed` after each change.
  - `pub fn status(&self) -> PhoneStatus` — `#[derive(Serialize, Type)] pub struct PhoneStatus { pub running: bool, pub url: Option<String>, pub qr_svg: Option<String>, pub connected: bool, pub error: Option<String> }`; `url = https://<ip>:<port>/?t=<token>`.
  - `pub async fn new_code(&self, app: &AppHandle)` — new token, save, restart server (drops the phone).
  - `pub fn notify_state(app: &AppHandle, state: PhoneState)` — no-op when the state is not managed.
- Commands (`commands/phone.rs`): `get_phone_microphone_status(app) -> Result<PhoneStatus, String>`, `new_phone_microphone_code(app) -> Result<PhoneStatus, String>`.

- [ ] **Step 1: Write failing test** in `settings.rs` tests: old settings JSON without the two keys deserializes with both `None`.
- [ ] **Step 2: Implement** the above. The commands `.await` `sync`/`new_code`; at startup `lib.rs` calls `tauri::async_runtime::spawn(async move { phone.sync(&app).await })`.
- [ ] **Step 3: Run** `cargo test` and `cargo clippy --all-targets` — clean. Run `bun run tauri dev` once so `src/bindings.ts` regenerates with the new commands and `PhoneStatus`.
- [ ] **Step 4: Commit point** — `feat: run the phone microphone server while it is selected`.

---

### Task 6: Record from the phone in `AudioRecordingManager`

**Files:**
- Modify: `src-tauri/src/managers/audio.rs`

**Interfaces:**
- Consumes: `PhoneMicrophone::capture()`, `PHONE_MICROPHONE`, `PhoneStartError` (Tasks 1, 5).
- Changes:
  - `enum MicSource { Device(Recorder<CaptureSink>), Phone(PhoneCapture<CaptureSink>) }`; `OpenMicrophone { source: MicSource, failed: Arc<AtomicBool> }`.
  - `DesiredMicrophone::Phone`; `desired_microphone` returns it when `selected_microphone == PHONE_MICROPHONE` (before the clamshell check is fine — the phone wins). `start_microphone_stream` opens `MicSource::Phone` without device resolution, so the phone is never reported unavailable and never reset to Default. Set the capture's chunk size from `vad_frame_samples()`.
  - `try_start_recording`: `Phone` → `capture.start(sink)`; `Err((NotConnected, sink))` returns the VAD and `Err(anyhow!(PhoneStartError::NotConnected))` immediately (no reopen loop).
  - `collect_recording`: `Phone` → `capture.stop()` → `sink.finish()` (same VAD return and `last_input_silent` handling); `None` → empty audio.
  - `close_microphone`: `Phone` → `stop()` and drop (do not stop the server; `PhoneMicrophone::sync` owns it).
  - `needs_reopen` stays false for the phone; a phone disconnect never sets `failed`.
  - `recording_error_type` returns `"phone_not_connected"` when the error downcasts to `PhoneStartError::NotConnected`.

- [ ] **Step 1: Write failing tests** in the existing `audio.rs` test module: `phone_name_maps_to_phone` — a pure helper `fn desired_from(selected: Option<&str>) -> DesiredMicrophone` (extract it from `desired_microphone`, minus clamshell) returns `Phone` for `"phone-wifi"`; `phone_error_type` — `recording_error_type(&anyhow!(PhoneStartError::NotConnected)) == "phone_not_connected"`.
- [ ] **Step 2: Run** `cargo test managers::audio` — fails.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test` (all) and `cargo clippy --all-targets` — pass. Check the frontend's `recording-error` handler shows a message for the new `error_type` (add an i18n key `errors.phoneNotConnected`: "Phone not connected — scan the QR code in Settings" if the handler maps types to keys).
- [ ] **Step 5: Commit point** — `feat: record from the phone microphone`.

---

### Task 7: Settings UI

**Files:**
- Modify: `src/components/settings/MicrophoneSelector.tsx`
- Create: `src/components/settings/PhoneMicrophonePanel.tsx`
- Modify: `src/i18n/locales/en/translation.json`

**Interfaces:**
- Consumes: `commands.getPhoneMicrophoneStatus()`, `commands.newPhoneMicrophoneCode()`, event `phone-microphone-changed`, type `PhoneStatus` (Task 5 bindings).

- [ ] **Step 1:** In `MicrophoneDropdown`, label the option whose `value === "phone-wifi"` with `t("settings.sound.microphone.phone")`; show that label when it is selected.
- [ ] **Step 2:** `PhoneMicrophonePanel` renders only when `getSetting("selected_microphone") === "phone-wifi"`: QR (`qr_svg` via `dangerouslySetInnerHTML`; it is generated by Chatelle, never from input), the URL as selectable text, "Phone connected" / "Waiting for the phone…", `error` if set, the firewall note, and a "New code" button. Reload status on mount and on `phone-microphone-changed`. Render it under the dropdown in `MicrophoneSelector`.
- [ ] **Step 3: Add strings** under `settings.sound.microphone`:

```json
"phone": "Phone (Wi-Fi)",
"phoneScan": "Scan this code with your phone's camera and open the page in Chrome.",
"phoneConnected": "Phone connected",
"phoneWaiting": "Waiting for the phone…",
"phoneFirewall": "The first time, Windows may ask to allow Chatelle on private networks. Allow it.",
"phoneNewCode": "New code",
"phoneCertNote": "Chrome warns that the connection is not private. Tap Advanced, then Proceed. This is expected."
```

- [ ] **Step 4: Run** `bun run lint` and `bun run build` — pass.
- [ ] **Step 5: Commit point** — `feat: show the phone QR code in microphone settings`.

---

### Task 8: End-to-end check on Windows with an Android phone (manual)

- [ ] Build: `bun run tauri dev` on the Windows laptop. Both devices on the same Wi-Fi.
- [ ] Select **Phone (Wi-Fi)**. Allow the firewall prompt. QR and URL appear; the URL's IP is the Wi-Fi adapter's address (check with `ipconfig`), not a VPN/Hyper-V one.
- [ ] Scan, accept the certificate warning, allow the microphone. Status shows "Phone connected" in Chatelle.
- [ ] Focus Notepad. Tap Record, say a sentence, tap Stop. The phone shows Recording → Transcribing → idle; the sentence is typed in Notepad.
- [ ] Press the laptop shortcut instead; recording also works.
- [ ] Turn off the phone's Wi-Fi mid-recording. Nothing is typed. Press the laptop shortcut: the partial sentence is typed.
- [ ] Turn off the overlay in settings; record again; the phone still shows Recording/Transcribing.
- [ ] Press the laptop shortcut with no phone connected: the "Phone not connected" error appears.
- [ ] Select a normal microphone: `netstat -ano | findstr 47615` shows nothing listening.
- [ ] Press "New code": the old phone page stops working until rescanned.
