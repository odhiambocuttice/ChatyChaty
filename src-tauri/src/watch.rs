//! Watches a designated chat window on the computer while away.
//!
//! Detects incoming messages, drafts replies using vision models,
//! and notifies the user via Telegram.

use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Manager};

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct WatchedWindow {
    pub window_id: u32,
    pub pid: i32,
    pub app_name: String,
    pub window_title: String,
    pub input_x_offset_from_bottom_left: i32,
    pub input_y_offset_from_bottom_left: i32,
}

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct PendingReply {
    pub message_id: i64,
    pub contact: String,
    pub incoming_text: String,
    pub draft: String,
}

static WATCHING: AtomicBool = AtomicBool::new(false);
static CURRENT_WINDOW: Mutex<Option<WatchedWindow>> = Mutex::new(None);
static PENDING_REPLY: Mutex<Option<PendingReply>> = Mutex::new(None);
static BASELINE_OVERRIDE: Mutex<Option<Vec<u8>>> = Mutex::new(None);
static CAFFEINATE_CHILD: Mutex<Option<std::process::Child>> = Mutex::new(None);

pub fn is_watching() -> bool {
    WATCHING.load(Ordering::SeqCst)
}

pub fn clear_pending_reply() {
    *PENDING_REPLY.lock().unwrap() = None;
}

pub fn watched_info() -> String {
    let guard = CURRENT_WINDOW.lock().unwrap();
    if let Some(ref win) = *guard {
        format!("{} ({})", win.app_name, win.window_title)
    } else {
        "None".to_string()
    }
}

#[cfg(target_os = "macos")]
fn get_front_window_macos() -> Result<(u32, i32, String, String, i32, i32, i32, i32), String> {
    use core_foundation::array::CFArray;
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;
    use core_graphics::display::CGWindowListCopyWindowInfo;
    use objc2_app_kit::NSWorkspace;

    const K_CG_NULL_WINDOW_ID: u32 = 0;
    const K_CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY: u32 = 1;
    const K_CG_WINDOW_LIST_EXCLUDE_DESKTOP_ELEMENTS: u32 = 16;

    let app = NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .ok_or_else(|| "Could not find frontmost application".to_string())?;
    let pid = app.processIdentifier();
    let app_name = app
        .localizedName()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "Chat App".to_string());

    let window_list_ref = unsafe {
        CGWindowListCopyWindowInfo(
            K_CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY | K_CG_WINDOW_LIST_EXCLUDE_DESKTOP_ELEMENTS,
            K_CG_NULL_WINDOW_ID,
        )
    };
    if window_list_ref.is_null() {
        return Err("Could not query window list from macOS".to_string());
    }

    let array: CFArray<CFDictionary<CFString, CFType>> =
        unsafe { CFArray::wrap_under_create_rule(window_list_ref) };

    for dict in array.iter() {
        // Check window layer (layer 0 is normal window)
        let layer_key = CFString::new("kCGWindowLayer");
        let is_layer_zero = dict
            .find(&layer_key)
            .and_then(|val| val.downcast::<CFNumber>())
            .and_then(|num| num.to_i32())
            .map(|l| l == 0)
            .unwrap_or(false);

        if !is_layer_zero {
            continue;
        }

        // Check owner PID
        let pid_key = CFString::new("kCGWindowOwnerPID");
        let owner_pid = dict
            .find(&pid_key)
            .and_then(|val| val.downcast::<CFNumber>())
            .and_then(|num| num.to_i32())
            .unwrap_or(-1);

        if owner_pid != pid {
            continue;
        }

        // Window ID
        let id_key = CFString::new("kCGWindowNumber");
        let win_id = dict
            .find(&id_key)
            .and_then(|val| val.downcast::<CFNumber>())
            .and_then(|num| num.to_i32())
            .unwrap_or(0) as u32;

        let name_key = CFString::new("kCGWindowName");
        let win_name = dict
            .find(&name_key)
            .and_then(|val| val.downcast::<CFString>())
            .map(|s| s.to_string())
            .unwrap_or_else(|| app_name.clone());

        // Bounds: dictionary with X, Y, Width, Height parsed with Apple's API
        let bounds_key = CFString::new("kCGWindowBounds");
        let mut rect = core_graphics::geometry::CGRect::new(
            &core_graphics::geometry::CGPoint::new(0.0, 0.0),
            &core_graphics::geometry::CGSize::new(800.0, 600.0),
        );
        #[link(name = "CoreGraphics", kind = "framework")]
        extern "C" {
            fn CGRectMakeWithDictionaryRepresentation(
                dict: *const std::ffi::c_void,
                rect: *mut core_graphics::geometry::CGRect,
            ) -> bool;
        }

        if let Some(bounds_val) = dict.find(&bounds_key) {
            unsafe {
                CGRectMakeWithDictionaryRepresentation(
                    bounds_val.as_CFType().as_CFTypeRef() as _,
                    &mut rect,
                );
            }
        }
        let x = rect.origin.x as i32;
        let y = rect.origin.y as i32;
        let w = rect.size.width as i32;
        let h = rect.size.height as i32;

        if win_id > 0 && w > 100 && h > 100 {
            return Ok((win_id, pid, app_name, win_name, x, y, w, h));
        }
    }

    Err(format!("Could not find an active window for {app_name}"))
}

#[cfg(target_os = "macos")]
fn get_front_window() -> Result<(u32, i32, String, String, i32, i32, i32, i32), String> {
    get_front_window_macos()
}

#[cfg(target_os = "windows")]
fn get_front_window() -> Result<(u32, i32, String, String, i32, i32, i32, i32), String> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId,
    };

    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() || hwnd.0.is_null() {
            return Err("No active window in front".to_string());
        }

        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));

        let mut title_buf = [0u16; 512];
        let len = GetWindowTextW(hwnd, &mut title_buf);
        let title = String::from_utf16_lossy(&title_buf[..len as usize]);
        let app_name = if title.is_empty() {
            "Chat Window".to_string()
        } else {
            title.clone()
        };

        let mut rect = RECT::default();
        let _ = GetWindowRect(hwnd, &mut rect);
        let x = rect.left;
        let y = rect.top;
        let w = rect.right - rect.left;
        let h = rect.bottom - rect.top;

        let win_id = hwnd.0 as usize as u32;
        Ok((win_id, pid as i32, app_name, title, x, y, w, h))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn get_front_window() -> Result<(u32, i32, String, String, i32, i32, i32, i32), String> {
    Err("Watch mode is currently supported on macOS and Windows".to_string())
}

/// Captures a specific window by its window ID as JPEG bytes.
#[cfg(target_os = "macos")]
pub fn capture_window_jpeg(window_id: u32) -> Result<Vec<u8>, String> {
    let path = std::env::temp_dir().join(format!("chatelle-watch-{}.jpg", std::process::id()));
    let result = (|| {
        // -x: no shutter sound, -o: no window drop shadow, -l: window id
        let captured = Command::new("screencapture")
            .args(["-x", "-o", "-t", "jpg", "-l", &window_id.to_string()])
            .arg(&path)
            .status()
            .map_err(|e| format!("Could not run screencapture: {e}"))?;

        if !captured.success() {
            return Err(
                "screencapture failed. Please allow Screen Recording for Chatelle.".to_string(),
            );
        }

        // Resize with sips to max 1600 side to stay within vision model limits
        let _ = Command::new("sips")
            .args(["-Z", "1600"])
            .arg(&path)
            .output();

        let bytes = std::fs::read(&path).map_err(|e| format!("Could not read screenshot: {e}"))?;
        if bytes.is_empty() {
            return Err("Captured screenshot is empty.".to_string());
        }
        Ok(bytes)
    })();

    let _ = std::fs::remove_file(&path);
    result
}

#[cfg(target_os = "windows")]
pub fn capture_window_jpeg(window_id: u32) -> Result<Vec<u8>, String> {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
        SRCCOPY,
    };
    use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

    let hwnd = HWND(window_id as usize as *mut _);
    let mut rect = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut rect);
    }
    let width = (rect.right - rect.left).max(1);
    let height = (rect.bottom - rect.top).max(1);

    let mut bgra = vec![0u8; width as usize * height as usize * 4];
    unsafe {
        let window_dc = GetDC(Some(hwnd));
        if window_dc.is_invalid() {
            return Err("Could not get window device context".to_string());
        }
        let memory = CreateCompatibleDC(Some(window_dc));
        let bitmap = CreateCompatibleBitmap(window_dc, width, height);
        let previous = SelectObject(memory, bitmap.into());

        // Try PrintWindow first (renders window even if occluded), fallback to BitBlt
        let printed = PrintWindow(hwnd, memory, PRINT_WINDOW_FLAGS(2)); // PW_RENDERFULLCONTENT
        if !printed.as_bool() {
            let _ = BitBlt(memory, 0, 0, width, height, Some(window_dc), 0, 0, SRCCOPY);
        }

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
        ReleaseDC(Some(hwnd), window_dc);
        if rows != height {
            return Err("Could not read window pixels".to_string());
        }
    }

    let rgb: Vec<u8> = bgra
        .chunks_exact(4)
        .flat_map(|pixel| [pixel[2], pixel[1], pixel[0]])
        .collect();
    let screenshot = image::RgbImage::from_raw(width as u32, height as u32, rgb)
        .ok_or_else(|| "The captured window image has an invalid size".to_string())?;

    let longest = width.max(height) as u32;
    let screenshot = if longest > 1600 {
        let scale = 1600.0 / longest as f64;
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
        .map_err(|e| format!("Could not encode JPEG: {e}"))?;
    Ok(jpeg)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn capture_window_jpeg(_window_id: u32) -> Result<Vec<u8>, String> {
    Err("Window capture is only supported on macOS and Windows".to_string())
}

pub fn capture_watched_window() -> Result<Vec<u8>, String> {
    let win = {
        let guard = CURRENT_WINDOW.lock().unwrap();
        guard.clone()
    };
    if let Some(win) = win {
        capture_window_jpeg(win.window_id)
    } else {
        Err("No chat window is currently being watched.".to_string())
    }
}

pub fn start_watch_session(app: AppHandle) -> Result<String, String> {
    if is_watching() {
        return Ok(watched_info());
    }

    // Get cursor position right when user triggers watch (assuming they clicked input box)
    let cursor = crate::input::get_cursor_position(&app).unwrap_or((400, 700));

    let (win_id, pid, app_name, win_name, wx, wy, _ww, wh) = get_front_window()?;

    // Calculate offset from bottom-left corner of the window
    let win_bottom = wy + wh;
    let input_x_offset = (cursor.0 - wx).max(10);
    let input_y_offset = (win_bottom - cursor.1).max(10);

    let watched = WatchedWindow {
        window_id: win_id,
        pid,
        app_name: app_name.clone(),
        window_title: win_name.clone(),
        input_x_offset_from_bottom_left: input_x_offset,
        input_y_offset_from_bottom_left: input_y_offset,
    };

    let display_name = format!("{app_name} ({win_name})");
    *CURRENT_WINDOW.lock().unwrap() = Some(watched);
    WATCHING.store(true, Ordering::SeqCst);

    // Keep display awake with caffeinate -di on macOS, SetThreadExecutionState on Windows
    #[cfg(target_os = "macos")]
    {
        let child = Command::new("caffeinate").args(["-di"]).spawn().ok();
        *CAFFEINATE_CHILD.lock().unwrap() = child;
    }

    #[cfg(target_os = "windows")]
    {
        use windows::Win32::System::Power::{
            SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
        };
        unsafe {
            SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED);
        }
    }

    // Spawn watcher background thread
    let app_clone = app.clone();
    std::thread::spawn(move || {
        run_watch_loop(app_clone, win_id);
    });

    info!("Started watch session for {display_name}");
    Ok(display_name)
}

pub fn stop_watch_session() {
    if !WATCHING.swap(false, Ordering::SeqCst) {
        return;
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(mut child) = CAFFEINATE_CHILD.lock().unwrap().take() {
            let _ = child.kill();
        }
    }

    #[cfg(target_os = "windows")]
    {
        use windows::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS};
        unsafe {
            SetThreadExecutionState(ES_CONTINUOUS);
        }
    }

    *CURRENT_WINDOW.lock().unwrap() = None;
    *PENDING_REPLY.lock().unwrap() = None;
    info!("Stopped watch session");
}

fn run_watch_loop(app: AppHandle, win_id: u32) {
    info!("Watch loop running for window ID {win_id}");
    let mut last_capture_bytes = match capture_window_jpeg(win_id) {
        Ok(b) => b,
        Err(e) => {
            warn!("Initial window capture failed: {e}");
            Vec::new()
        }
    };

    let mut last_reported_message = String::new();

    while is_watching() {
        std::thread::sleep(Duration::from_secs(3));

        if !is_watching() {
            break;
        }

        // If a new baseline was set (e.g. after sending a reply), adopt it
        if let Some(new_baseline) = BASELINE_OVERRIDE.lock().unwrap().take() {
            last_capture_bytes = new_baseline;
        }

        let current_bytes = match capture_window_jpeg(win_id) {
            Ok(b) => b,
            Err(e) => {
                warn!("Window capture failed (window closed or minimized?): {e}");
                // If window closed, notify telegram and stop
                notify_telegram_text(
                    &app,
                    "⚠️ *Watch stopped:* Chat window was closed or minimized.",
                );
                stop_watch_session();
                break;
            }
        };

        if has_visual_change(&last_capture_bytes, &current_bytes) {
            info!("Visual change detected in chat window, settling for 5s...");
            // Wait 5s for messages to settle
            std::thread::sleep(Duration::from_secs(5));
            if !is_watching() {
                break;
            }

            // Capture settled screenshot
            if let Ok(settled_bytes) = capture_window_jpeg(win_id) {
                last_capture_bytes = settled_bytes.clone();

                // Call vision model to draft reply
                match crate::reply::draft_reply_from_jpeg(&settled_bytes) {
                    Ok(draft) => {
                        if draft != "NO_CONVERSATION" && draft != last_reported_message {
                            last_reported_message = draft.clone();
                            send_telegram_alert(&app, &draft, settled_bytes);
                        }
                    }
                    Err(e) => {
                        warn!("Drafting reply from watch screenshot failed: {e}");
                    }
                }
            }
        }
    }
}

/// Simple image difference check based on byte length and sample diff
fn has_visual_change(a: &[u8], b: &[u8]) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    // If compressed JPEG byte size differs by more than 0.5%, there is meaningful movement/text
    let len_diff = (a.len() as isize - b.len() as isize).abs();
    let threshold = (a.len() / 200).max(500); // 0.5% or 500 bytes
    if len_diff > threshold as isize {
        return true;
    }

    // Quick sample comparison across bytes
    let sample_count = 100;
    let step_a = a.len() / sample_count;
    let step_b = b.len() / sample_count;
    let mut diff_count = 0;
    for i in 0..sample_count {
        if a.get(i * step_a) != b.get(i * step_b) {
            diff_count += 1;
        }
    }
    diff_count > 15
}

fn send_telegram_alert(app: &AppHandle, draft: &str, _jpeg: Vec<u8>) {
    let state = app.state::<Arc<crate::telegram::TelegramState>>();
    let token = {
        let guard = state.bot_token.lock().unwrap();
        guard.clone()
    };
    let chat_id = *state.paired_chat_id.lock().unwrap();

    let (Some(tok), Some(cid)) = (token, chat_id) else {
        warn!("Cannot send Telegram alert: bot not configured or paired");
        return;
    };

    let keyboard = serde_json::json!({
        "inline_keyboard": [
            [
                { "text": "✅ Send Reply", "callback_data": "send" },
                { "text": "👀 Show Screen", "callback_data": "show_screen" }
            ],
            [
                { "text": "❌ Ignore", "callback_data": "ignore" }
            ]
        ]
    });

    let text = format!("💬 *New message in chat!*\n\n📝 *Draft reply:*\n_{}_\n\nTap *Send Reply* to type and send on laptop.", draft);
    match crate::telegram::send_message(&tok, cid, &text, Some(keyboard)) {
        Ok(msg_id) => {
            *PENDING_REPLY.lock().unwrap() = Some(PendingReply {
                message_id: msg_id,
                contact: "Chat".to_string(),
                incoming_text: String::new(),
                draft: draft.to_string(),
            });
            info!("Telegram alert sent for message ID {msg_id}");
        }
        Err(e) => {
            error!("Failed to send Telegram alert: {e}");
        }
    }
}

fn notify_telegram_text(app: &AppHandle, text: &str) {
    let state = app.state::<Arc<crate::telegram::TelegramState>>();
    let token = {
        let guard = state.bot_token.lock().unwrap();
        guard.clone()
    };
    let chat_id = *state.paired_chat_id.lock().unwrap();
    if let (Some(tok), Some(cid)) = (token, chat_id) {
        let _ = crate::telegram::send_message(&tok, cid, text, None);
    }
}

/// Types and sends the pending reply into the watched chat window.
pub fn send_pending_reply(app: AppHandle, message_id: i64) -> Result<(), String> {
    // Atomically take the pending reply to guarantee one-time execution and prevent double-send races
    let Some(reply) = PENDING_REPLY.lock().unwrap().take() else {
        return Err("No pending reply to send.".to_string());
    };

    let win = {
        let guard = CURRENT_WINDOW.lock().unwrap();
        guard.clone()
    };

    let Some(win) = win else {
        return Err("Chat window is no longer being watched.".to_string());
    };

    let draft_text = reply.draft;

    info!("Typing reply from Telegram into window {}", win.window_id);

    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};
        // Activate app
        if let Some(app_obj) =
            NSRunningApplication::runningApplicationWithProcessIdentifier(win.pid)
        {
            app_obj.activateWithOptions(NSApplicationActivationOptions::empty());
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    #[cfg(target_os = "windows")]
    {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{
            SetForegroundWindow, ShowWindow, SW_RESTORE,
        };
        unsafe {
            let hwnd = HWND(win.window_id as usize as *mut _);
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    // Type text using clipboard / paste
    crate::clipboard::paste(draft_text.clone(), app.clone())?;

    // Press Enter to send
    std::thread::sleep(Duration::from_millis(200));
    let enigo_state = app
        .try_state::<crate::input::EnigoState>()
        .ok_or_else(|| "Input state not available".to_string())?;
    {
        use enigo::{Direction, Key, Keyboard};
        let mut enigo = enigo_state.0.lock().map_err(|e| e.to_string())?;
        let _ = enigo.key(Key::Return, Direction::Click);
    }

    // Allow UI to render the sent message bubble
    std::thread::sleep(Duration::from_millis(1200));

    // Capture confirmation screenshot and update watcher baseline
    // so the newly typed message bubble isn't detected as an incoming message
    let conf_jpeg = capture_window_jpeg(win.window_id).ok();
    if let Some(ref jpeg) = conf_jpeg {
        *BASELINE_OVERRIDE.lock().unwrap() = Some(jpeg.clone());
    }

    // Update Telegram message to confirmed
    let state = app.state::<Arc<crate::telegram::TelegramState>>();
    let token = {
        let guard = state.bot_token.lock().unwrap();
        guard.clone()
    };
    let chat_id = *state.paired_chat_id.lock().unwrap();
    if let (Some(tok), Some(cid)) = (token, chat_id) {
        let _ = crate::telegram::edit_message_text(
            &tok,
            cid,
            message_id,
            &format!("✅ *Sent:*\n_{}_", draft_text),
            None,
        );

        // Send confirmation screenshot
        if let Some(jpeg) = conf_jpeg {
            let _ = crate::telegram::send_photo(&tok, cid, &jpeg, Some("Sent confirmation"), None);
        }
    }

    Ok(())
}
