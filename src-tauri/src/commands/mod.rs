pub mod audio;
pub mod models;
pub mod phone;

use crate::settings::{get_settings, write_settings, AppSettings, LogLevel};
use crate::utils::cancel_current_operation;
use tauri::{AppHandle, Manager};
use tauri_plugin_opener::OpenerExt;

#[tauri::command]
#[specta::specta]
pub fn cancel_operation(app: AppHandle) {
    cancel_current_operation(&app);
}

#[tauri::command]
#[specta::specta]
pub fn get_app_settings(app: AppHandle) -> Result<AppSettings, String> {
    Ok(get_settings(&app))
}

#[tauri::command]
#[specta::specta]
pub fn get_default_settings() -> Result<AppSettings, String> {
    Ok(crate::settings::get_default_settings())
}

#[tauri::command]
#[specta::specta]
pub fn get_reply_provider_keys() -> Vec<crate::reply::ProviderKey> {
    crate::reply::provider_keys()
}

#[tauri::command]
#[specta::specta]
pub fn set_reply_provider_key(provider: String, key: String) -> Result<(), String> {
    crate::reply::save_provider_key(&provider, &key)
}

#[tauri::command]
#[specta::specta]
pub fn get_telegram_status(app: AppHandle) -> Result<crate::telegram::TelegramStatus, String> {
    let state = app.state::<std::sync::Arc<crate::telegram::TelegramState>>();
    Ok(state.get_status())
}

#[tauri::command]
#[specta::specta]
pub fn set_telegram_bot_token(
    app: AppHandle,
    token: String,
) -> Result<crate::telegram::TelegramStatus, String> {
    let token = token.trim();
    if token.is_empty() {
        let _ = crate::key_store::delete(crate::telegram::TELEGRAM_BOT_TOKEN_KEY);
        let _ = crate::key_store::delete(crate::telegram::TELEGRAM_CHAT_ID_KEY);
        let state = app.state::<std::sync::Arc<crate::telegram::TelegramState>>();
        *state.bot_token.lock().unwrap() = None;
        *state.paired_chat_id.lock().unwrap() = None;
        Ok(state.get_status())
    } else {
        let username = crate::telegram::get_bot_username(token)?;
        crate::key_store::write(crate::telegram::TELEGRAM_BOT_TOKEN_KEY, token)?;
        let state = app.state::<std::sync::Arc<crate::telegram::TelegramState>>();
        *state.bot_token.lock().unwrap() = Some(token.to_string());
        crate::telegram::start_polling(app.clone());
        log::info!("Telegram bot configured: @{username}");
        Ok(state.get_status())
    }
}

#[tauri::command]
#[specta::specta]
pub fn unlink_telegram(app: AppHandle) -> Result<crate::telegram::TelegramStatus, String> {
    let _ = crate::key_store::delete(crate::telegram::TELEGRAM_CHAT_ID_KEY);
    let state = app.state::<std::sync::Arc<crate::telegram::TelegramState>>();
    *state.paired_chat_id.lock().unwrap() = None;
    Ok(state.get_status())
}

#[tauri::command]
#[specta::specta]
pub fn toggle_watch_session(app: AppHandle) -> Result<bool, String> {
    if crate::watch::is_watching() {
        crate::watch::stop_watch_session();
        Ok(false)
    } else {
        crate::watch::start_watch_session(app)?;
        Ok(true)
    }
}

#[tauri::command]
#[specta::specta]
pub fn get_watch_status() -> Result<bool, String> {
    Ok(crate::watch::is_watching())
}

#[tauri::command]
#[specta::specta]
pub fn trigger_draft_reply(app: AppHandle) -> Result<(), String> {
    crate::actions::trigger_draft_reply(&app);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn trigger_draft_reply_hint_start(app: AppHandle) -> Result<(), String> {
    crate::actions::trigger_draft_reply_hint_start(&app);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn trigger_draft_reply_hint_stop(app: AppHandle) -> Result<(), String> {
    crate::actions::trigger_draft_reply_hint_stop(&app);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn trigger_regenerate_reply(app: AppHandle) -> Result<(), String> {
    crate::actions::trigger_regenerate_reply(&app);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn trigger_transcribe_toggle(app: AppHandle) -> Result<(), String> {
    crate::actions::trigger_transcribe_toggle(&app);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn show_dock_overlay(app: AppHandle) -> Result<(), String> {
    let _ = crate::shortcut::change_show_floating_dock_setting(app.clone(), true);
    crate::overlay::show_dock_overlay(&app);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn hide_dock_overlay(app: AppHandle) -> Result<(), String> {
    let _ = crate::shortcut::change_show_floating_dock_setting(app.clone(), false);
    crate::overlay::force_hide_overlay(&app);
    Ok(())
}

/// The log file's text, for the Logs tab. Rotation keeps the file at 500 KB,
/// so the whole file is small enough to show.
#[tauri::command]
#[specta::specta]
pub fn get_log_text(app: AppHandle) -> Result<String, String> {
    let path = crate::portable::app_log_dir(&app)
        .map_err(|e| format!("Failed to get log directory: {}", e))?
        .join("handy.log");
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("Failed to read {}: {}", path.display(), e)),
    }
}

#[tauri::command]
#[specta::specta]
pub fn get_log_dir_path(app: AppHandle) -> Result<String, String> {
    let log_dir = crate::portable::app_log_dir(&app)
        .map_err(|e| format!("Failed to get log directory: {}", e))?;

    Ok(log_dir.to_string_lossy().to_string())
}

#[specta::specta]
#[tauri::command]
pub fn set_log_level(app: AppHandle, level: LogLevel) -> Result<(), String> {
    let tauri_log_level: tauri_plugin_log::LogLevel = level.into();
    let log_level: log::Level = tauri_log_level.into();
    // Update the file log level atomic so the filter picks up the new level
    crate::FILE_LOG_LEVEL.store(
        log_level.to_level_filter() as u8,
        std::sync::atomic::Ordering::Relaxed,
    );

    let mut settings = get_settings(&app);
    settings.log_level = level;
    write_settings(&app, settings);

    Ok(())
}

#[specta::specta]
#[tauri::command]
pub fn open_log_dir(app: AppHandle) -> Result<(), String> {
    let log_dir = crate::portable::app_log_dir(&app)
        .map_err(|e| format!("Failed to get log directory: {}", e))?;

    let path = log_dir.to_string_lossy().as_ref().to_string();
    app.opener()
        .open_path(path, None::<String>)
        .map_err(|e| format!("Failed to open log directory: {}", e))?;

    Ok(())
}

/// Try to initialize Enigo (keyboard/mouse simulation).
/// On macOS, this will return an error if accessibility permissions are not granted.
#[specta::specta]
#[tauri::command]
pub fn initialize_enigo(app: AppHandle) -> Result<(), String> {
    use crate::input::EnigoState;

    // Check if already initialized
    if app.try_state::<EnigoState>().is_some() {
        log::debug!("Enigo already initialized");
        return Ok(());
    }

    // Try to initialize
    match EnigoState::new() {
        Ok(enigo_state) => {
            app.manage(enigo_state);
            log::info!("Enigo initialized successfully after permission grant");
            Ok(())
        }
        Err(e) => {
            if cfg!(target_os = "macos") {
                log::warn!(
                    "Failed to initialize Enigo: {} (accessibility permissions may not be granted)",
                    e
                );
            } else {
                log::warn!("Failed to initialize Enigo: {}", e);
            }
            Err(format!("Failed to initialize input system: {}", e))
        }
    }
}

/// Marker state to track if shortcuts have been initialized.
pub struct ShortcutsInitialized;

/// Initialize keyboard shortcuts.
/// On macOS, this should be called after accessibility permissions are granted.
/// This is idempotent - calling it multiple times is safe.
#[specta::specta]
#[tauri::command]
pub fn initialize_shortcuts(app: AppHandle) -> Result<(), String> {
    // Check if already initialized
    if app.try_state::<ShortcutsInitialized>().is_some() {
        log::debug!("Shortcuts already initialized");
        return Ok(());
    }

    // Initialize shortcuts
    crate::shortcut::init_shortcuts(&app);

    // Mark as initialized before reconciling the macOS Secure Input fallback.
    app.manage(ShortcutsInitialized);
    crate::secure_input::reconcile_fallback(&app);

    log::info!("Shortcuts initialized successfully");
    Ok(())
}
