use crate::phone::{PhoneMicrophone, PhoneStatus};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

fn phone(app: &AppHandle) -> Result<Arc<PhoneMicrophone>, String> {
    app.try_state::<Arc<PhoneMicrophone>>()
        .map(|state| state.inner().clone())
        .ok_or_else(|| "Phone microphone is not ready".to_string())
}

/// Starts or stops the phone server to match the selected microphone, then
/// returns what the settings page shows.
#[tauri::command]
#[specta::specta]
pub async fn get_phone_microphone_status(app: AppHandle) -> Result<PhoneStatus, String> {
    let phone = phone(&app)?;
    phone.sync(&app).await;
    Ok(phone.status())
}

/// Makes a new pairing code. The connected phone must scan the new QR code.
#[tauri::command]
#[specta::specta]
pub async fn new_phone_microphone_code(app: AppHandle) -> Result<PhoneStatus, String> {
    let phone = phone(&app)?;
    phone.new_code(&app).await;
    Ok(phone.status())
}
