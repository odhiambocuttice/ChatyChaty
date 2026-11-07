use crate::input::{self, EnigoState};
use enigo::{Enigo, Keyboard};
use std::sync::{mpsc, Mutex, PoisonError};
use tauri::{AppHandle, Manager};
use tauri_plugin_clipboard_manager::ClipboardExt;

/// Serializes typing: two transcripts typed at once would interleave their
/// characters. Only taken by `paste`, which never runs on the main thread.
static PASTE_LOCK: Mutex<()> = Mutex::new(());

/// Runs `f` with the shared Enigo on the main thread (macOS TIS APIs require it)
/// and waits for the result. Runs inline if already on the main thread, so no deadlock.
pub(crate) fn with_enigo<T: Send + 'static>(
    app_handle: &AppHandle,
    f: impl FnOnce(&mut Enigo) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = mpsc::sync_channel(1);
    let app = app_handle.clone();
    app_handle
        .run_on_main_thread(move || {
            let result = (|| {
                // The frontend creates this after its permission screen, but
                // typing must not depend on that screen having been reached:
                // transcripts arrive from the tray and the CLI too. Creating it
                // here is safe because this already runs on the main thread.
                if app.try_state::<EnigoState>().is_none() {
                    app.manage(EnigoState::new()?);
                }
                let enigo_state = app
                    .try_state::<EnigoState>()
                    .ok_or("Enigo state unavailable")?;
                let mut enigo = enigo_state
                    .0
                    .lock()
                    .map_err(|e| format!("Failed to lock Enigo: {}", e))?;
                f(&mut enigo)
            })();
            let _ = tx.send(result);
        })
        .map_err(|e| format!("Failed to run input on the main thread: {}", e))?;
    // A dropped closure (event loop shutting down) closes the channel.
    rx.recv()
        .map_err(|_| "Main thread dropped the input request".to_string())?
}

/// The process ID of the app in front. Call on the main thread.
#[cfg(target_os = "macos")]
fn front_app_pid() -> Option<i32> {
    let app = objc2_app_kit::NSWorkspace::sharedWorkspace().frontmostApplication()?;
    Some(app.processIdentifier())
}

/// Not checked on other platforms, so typing never stops for this reason.
#[cfg(not(target_os = "macos"))]
fn front_app_pid() -> Option<i32> {
    None
}

/// Windows: an input method in Japanese (or another composition) mode turns the
/// typed Latin keys into kana ("Wouldblingど" was typed into a chat), so the
/// front window's IME is switched off for the typing and back on afterwards.
#[cfg(target_os = "windows")]
struct ImeGuard {
    ime_window: windows::Win32::Foundation::HWND,
}

#[cfg(target_os = "windows")]
impl ImeGuard {
    const IMC_GETOPENSTATUS: usize = 0x0005;
    const IMC_SETOPENSTATUS: usize = 0x0006;

    /// Switches the front window's IME off when it is on. None when there is
    /// no IME window or it is already off: nothing to put back.
    fn close_for_front_window() -> Option<Self> {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::Input::Ime::ImmGetDefaultIMEWnd;
        use windows::Win32::UI::WindowsAndMessaging::{
            GetForegroundWindow, SendMessageW, WM_IME_CONTROL,
        };
        unsafe {
            let ime_window = ImmGetDefaultIMEWnd(GetForegroundWindow());
            if ime_window.0.is_null() {
                return None;
            }
            let open = SendMessageW(
                ime_window,
                WM_IME_CONTROL,
                Some(WPARAM(Self::IMC_GETOPENSTATUS)),
                Some(LPARAM(0)),
            )
            .0 != 0;
            if !open {
                return None;
            }
            SendMessageW(
                ime_window,
                WM_IME_CONTROL,
                Some(WPARAM(Self::IMC_SETOPENSTATUS)),
                Some(LPARAM(0)),
            );
            log::info!("Switched the front window's input method off while typing");
            Some(Self { ime_window })
        }
    }
}

#[cfg(target_os = "windows")]
impl Drop for ImeGuard {
    fn drop(&mut self) {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, WM_IME_CONTROL};
        unsafe {
            SendMessageW(
                self.ime_window,
                WM_IME_CONTROL,
                Some(WPARAM(Self::IMC_SETOPENSTATUS)),
                Some(LPARAM(1)),
            );
        }
    }
}

/// Types text directly by simulating individual key presses.
fn paste_direct(text: &str, app_handle: &AppHandle) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let _ime = ImeGuard::close_for_front_window();
    // Keys go to whatever is in front. A live test typed the second half of a
    // reply into another window after the user clicked away, so typing stops
    // as soon as a different app comes to the front.
    let front_app = with_enigo(app_handle, |_| Ok(front_app_pid()))?;
    let key_delay_ms = crate::settings::get_settings(app_handle)
        .typing_speed
        .key_delay_ms();

    // The waiting and the pressing have to happen on different threads. Pacing
    // sleeps once per character, so doing that on the main thread would freeze
    // the window for as long as the transcript takes to type; but enigo's macOS
    // backend traps when it is driven from anywhere else. So this worker thread
    // keeps the rhythm and hands each individual key press to the main thread,
    // where each one costs microseconds.
    input::type_text_as_keystrokes(
        |stroke| {
            with_enigo(app_handle, move |enigo| {
                if front_app_pid() != front_app {
                    return Err("another app came to the front, so typing stopped".to_string());
                }
                match stroke {
                    input::Stroke::Key(key, direction) => enigo.key(key, direction),
                    input::Stroke::Text(character) => enigo.text(&character.to_string()),
                }
                .map_err(|e| format!("{}", e))
            })
        },
        text,
        key_delay_ms,
        input::CharacterInput::for_this_platform(),
    )
}

/// Pastes text in one go: puts it on the clipboard, presses Cmd+V (Ctrl+V on
/// Windows and Linux), then puts back what the clipboard held before.
fn paste_via_clipboard(text: &str, app_handle: &AppHandle) -> Result<(), String> {
    let clipboard = app_handle.clipboard();
    let saved = clipboard.read_text().ok().filter(|t| !t.is_empty());
    // Only probe for an image when there is no text: reading one decodes the
    // whole bitmap, and text is the common case.
    let saved_image = if saved.is_none() {
        clipboard.read_image().ok().map(|image| image.to_owned())
    } else {
        None
    };
    clipboard
        .write_text(text)
        .map_err(|e| format!("Could not write to the clipboard: {e}"))?;
    std::thread::sleep(std::time::Duration::from_millis(50));

    let result = with_enigo(app_handle, |enigo| {
        use enigo::{Direction, Key};
        // ANSI V key code on macOS, VK_V on Windows.
        #[cfg(target_os = "macos")]
        let (modifier, v) = (Key::Meta, Key::Other(9));
        #[cfg(target_os = "windows")]
        let (modifier, v) = (Key::Control, Key::Other(0x56));
        #[cfg(target_os = "linux")]
        let (modifier, v) = (Key::Control, Key::Unicode('v'));
        enigo
            .key(modifier, Direction::Press)
            .and_then(|_| enigo.key(v, Direction::Click))
            .and_then(|_| {
                std::thread::sleep(std::time::Duration::from_millis(50));
                enigo.key(modifier, Direction::Release)
            })
            .map_err(|e| format!("Could not press the paste keys: {e}"))
    });

    // Give the app time to read the clipboard before it changes back.
    std::thread::sleep(std::time::Duration::from_millis(300));
    // Put back what was there: text, else an image (a copied screenshot), else
    // clear it so the drafted text is not left behind.
    let _ = match (saved, saved_image) {
        (Some(previous), _) => clipboard.write_text(previous),
        (None, Some(image)) => clipboard.write_image(&image),
        (None, None) => clipboard.clear(),
    };
    result
}

/// Puts `text` into the focused app: typed one key press per character, or
/// pasted through the clipboard when that setting is on. Call from a worker
/// thread, never the main thread: it sleeps between keys (input injection
/// hops via `with_enigo`).
pub fn paste(text: String, app_handle: AppHandle) -> Result<(), String> {
    let _paste = PASTE_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    if crate::settings::get_settings(&app_handle).paste_with_clipboard {
        paste_via_clipboard(&text, &app_handle)
    } else {
        paste_direct(&text, &app_handle)
    }
}
