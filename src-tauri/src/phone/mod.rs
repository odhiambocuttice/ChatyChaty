//! An Android phone used as the microphone over local Wi-Fi.
//!
//! While "phone-wifi" is the selected microphone, `PhoneMicrophone` runs the
//! HTTPS server that the phone page connects to. The recorder reads the phone
//! audio through `PhoneMicrophone::capture`.

pub mod capture;
pub mod identity;
pub mod protocol;
pub mod server;

use crate::audio_toolkit::CaptureSink;
use crate::settings::{get_settings, write_settings};
use capture::PhoneCapture;
use protocol::PhoneState;
use serde::Serialize;
use server::{start_server, RunningServer, ServerConfig};
use specta::Type;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::watch;

/// The `selected_microphone` value that means "use the phone".
pub const PHONE_MICROPHONE: &str = "phone-wifi";

/// The port tried first when no port is saved yet.
const DEFAULT_PORT: u16 = 47615;

/// Sent to the frontend when the server starts or stops, or the phone connects
/// or disconnects. The payload is empty; read `get_phone_microphone_status`.
const CHANGED_EVENT: &str = "phone-microphone-changed";

const NO_NETWORK_ERROR: &str =
    "No Wi-Fi network found. Connect this computer to Wi-Fi and try again.";

/// What the settings page shows about the phone microphone.
#[derive(Serialize, Type, Debug, Clone)]
pub struct PhoneStatus {
    pub running: bool,
    /// The address the phone opens: `https://<ip>:<port>/?t=<token>`.
    pub url: Option<String>,
    /// A QR code of `url`, as SVG markup.
    pub qr_svg: Option<String>,
    pub connected: bool,
    pub error: Option<String>,
}

/// What the settings page shows, except `connected`, which the capture knows.
#[derive(Default)]
struct Shown {
    url: Option<String>,
    qr_svg: Option<String>,
    error: Option<String>,
}

pub struct PhoneMicrophone {
    capture: PhoneCapture<CaptureSink>,
    state: watch::Sender<PhoneState>,
    /// The running server. The async lock also keeps `sync` and `new_code`
    /// from running at the same time.
    server: tokio::sync::Mutex<Option<RunningServer>>,
    shown: Mutex<Shown>,
}

impl PhoneMicrophone {
    pub fn new() -> Self {
        Self {
            // Task 6 sets the chunk size the recorder needs.
            capture: PhoneCapture::new(512),
            state: watch::channel(PhoneState::Idle).0,
            server: tokio::sync::Mutex::new(None),
            shown: Mutex::new(Shown::default()),
        }
    }

    /// The shared phone capture. Every clone feeds the same recording.
    #[allow(dead_code)] // Task 6: the recorder records from this.
    pub fn capture(&self) -> PhoneCapture<CaptureSink> {
        self.capture.clone()
    }

    fn lock_shown(&self) -> std::sync::MutexGuard<'_, Shown> {
        self.shown.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn status(&self) -> PhoneStatus {
        let shown = self.lock_shown();
        PhoneStatus {
            running: shown.url.is_some(),
            url: shown.url.clone(),
            qr_svg: shown.qr_svg.clone(),
            connected: self.capture.is_connected(),
            error: shown.error.clone(),
        }
    }

    /// Starts the server when the phone is the selected microphone and stops it
    /// otherwise. Errors go to `status().error`.
    pub async fn sync(&self, app: &AppHandle) {
        let mut server = self.server.lock().await;
        let selected = get_settings(app).selected_microphone.as_deref() == Some(PHONE_MICROPHONE);
        match (selected, server.is_some()) {
            (true, false) => *server = self.start(app).await,
            (false, true) => {
                if let Some(running) = server.take() {
                    running.stop().await;
                }
                *self.lock_shown() = Shown::default();
            }
            (false, false) => {
                // Clear the error of an earlier failed start.
                let mut shown = self.lock_shown();
                if shown.error.is_none() {
                    return;
                }
                shown.error = None;
            }
            (true, true) => return,
        }
        emit_changed(app);
    }

    /// Makes a new pairing token. The connected phone is dropped and must scan
    /// the new QR code.
    pub async fn new_code(&self, app: &AppHandle) {
        let mut server = self.server.lock().await;
        let mut settings = get_settings(app);
        settings.phone_microphone_token = Some(identity::new_token());
        write_settings(app, settings);

        if let Some(running) = server.take() {
            running.stop().await;
            *self.lock_shown() = Shown::default();
            *server = self.start(app).await;
        }
        emit_changed(app);
    }

    /// Starts the server and fills in what the settings page shows. Returns
    /// `None` and records the error if it cannot start.
    async fn start(&self, app: &AppHandle) -> Option<RunningServer> {
        match self.try_start(app).await {
            Ok((running, url)) => {
                let qr_svg = qr_svg(&url);
                if qr_svg.is_none() {
                    log::warn!("Phone microphone: could not make the QR code");
                }
                *self.lock_shown() = Shown {
                    url: Some(url),
                    qr_svg,
                    error: None,
                };
                Some(running)
            }
            Err(error) => {
                log::error!("Phone microphone: the server did not start: {error}");
                *self.lock_shown() = Shown {
                    error: Some(error),
                    ..Shown::default()
                };
                None
            }
        }
    }

    async fn try_start(&self, app: &AppHandle) -> Result<(RunningServer, String), String> {
        let ip = identity::local_ipv4().ok_or_else(|| NO_NETWORK_ERROR.to_string())?;

        let mut settings = get_settings(app);
        let token = match settings.phone_microphone_token.clone() {
            Some(token) => token,
            None => {
                let token = identity::new_token();
                settings.phone_microphone_token = Some(token.clone());
                write_settings(app, settings.clone());
                token
            }
        };

        let dir = crate::portable::app_data_dir(app)
            .map_err(|e| format!("Could not find the app data folder: {e}"))?;
        let identity = identity::load_or_create_identity(&dir, ip)
            .map_err(|e| format!("Could not make the phone certificate: {e:#}"))?;

        let on_toggle: Arc<dyn Fn() + Send + Sync> = {
            let app = app.clone();
            // Only queues a message for the transcription coordinator; it never blocks.
            Arc::new(move || {
                crate::signal_handle::send_transcription_input(&app, "transcribe", "phone")
            })
        };
        let on_connection_change: Arc<dyn Fn(bool) + Send + Sync> = {
            let app = app.clone();
            Arc::new(move |_connected| emit_changed(&app))
        };

        let running = start_server(ServerConfig {
            port: settings.phone_microphone_port.unwrap_or(DEFAULT_PORT),
            token: token.clone(),
            identity,
            capture: self.capture.clone(),
            on_toggle,
            state: self.state.subscribe(),
            on_connection_change,
        })
        .await
        .map_err(|e| format!("Could not start the phone server: {e:#}"))?;

        if settings.phone_microphone_port != Some(running.port) {
            let mut settings = get_settings(app);
            settings.phone_microphone_port = Some(running.port);
            write_settings(app, settings);
        }

        let url = format!("https://{ip}:{}/?t={token}", running.port);
        Ok((running, url))
    }
}

/// Tells the phone page what the app is doing. Does nothing before the phone
/// microphone is set up.
pub fn notify_state(app: &AppHandle, state: PhoneState) {
    if let Some(phone) = app.try_state::<Arc<PhoneMicrophone>>() {
        phone.state.send_replace(state);
    }
}

fn emit_changed(app: &AppHandle) {
    if let Err(e) = app.emit(CHANGED_EVENT, ()) {
        log::warn!("Phone microphone: could not emit {CHANGED_EVENT}: {e}");
    }
}

/// A QR code for `url`, as SVG markup.
fn qr_svg(url: &str) -> Option<String> {
    let code = qrcode::QrCode::new(url.as_bytes()).ok()?;
    Some(
        code.render::<qrcode::render::svg::Color>()
            .min_dimensions(240, 240)
            .build(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_svg_is_svg() {
        let svg = qr_svg("https://192.168.1.20:47615/?t=0123456789abcdef0123456789abcdef").unwrap();
        assert!(svg.contains("<svg"));
        assert!(svg.ends_with("</svg>"));
    }
}
