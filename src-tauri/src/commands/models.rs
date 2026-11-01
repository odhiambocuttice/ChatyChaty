use crate::managers::model::{model_file, ModelManager};
use crate::managers::transcription::TranscriptionManager;
use crate::settings::{self, SpeechModel};
use log::{error, info};
use serde::Serialize;
use specta::Type;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};

/// What the settings window shows about the selected model.
#[derive(Debug, Clone, Serialize, Type)]
pub struct ModelStatus {
    pub name: String,
    pub size_mb: u64,
    pub is_downloaded: bool,
    pub is_downloading: bool,
    pub is_loaded: bool,
}

#[tauri::command]
#[specta::specta]
pub fn get_model_status(
    model_manager: State<'_, Arc<ModelManager>>,
    transcription_manager: State<'_, Arc<TranscriptionManager>>,
) -> ModelStatus {
    let file = model_manager.selected();
    ModelStatus {
        name: file.name.to_string(),
        size_mb: file.size_bytes / (1024 * 1024),
        is_downloaded: model_manager.is_downloaded(),
        is_downloading: model_manager.is_downloading(),
        is_loaded: transcription_manager.is_model_loaded(),
    }
}

/// A model the settings window offers.
#[derive(Debug, Clone, Serialize, Type)]
pub struct SpeechModelOption {
    pub id: SpeechModel,
    pub name: String,
    pub size_mb: u64,
    /// Whether it types while you speak, or only after you stop.
    pub streams: bool,
}

#[tauri::command]
#[specta::specta]
pub fn get_speech_models() -> Vec<SpeechModelOption> {
    [SpeechModel::Parakeet, SpeechModel::Qwen3Asr]
        .into_iter()
        .map(|id| {
            let file = model_file(id);
            SpeechModelOption {
                id,
                name: file.name.to_string(),
                size_mb: file.size_bytes / (1024 * 1024),
                streams: file.streams,
            }
        })
        .collect()
}

/// Switch to another speech model: stop a download of the old one and unload
/// it, then download the new one if needed and load it in the background, so
/// the settings window can switch back while the download runs.
#[tauri::command]
#[specta::specta]
pub async fn change_speech_model_setting(app: AppHandle, model: SpeechModel) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    if settings.speech_model == model {
        return Ok(());
    }
    info!("Switching speech model to {:?}", model);
    settings.speech_model = model;
    settings::write_settings(&app, settings);

    let model_manager = app.state::<Arc<ModelManager>>();
    if model_manager.is_downloading() {
        model_manager.cancel_download();
        // A cancelled download takes a moment to stop; a new one waits for it.
        while model_manager.is_downloading() {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
    let transcription_manager = app.state::<Arc<TranscriptionManager>>().inner().clone();
    tauri::async_runtime::spawn_blocking(move || transcription_manager.unload_model())
        .await
        .map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn(async move {
        if let Err(e) = ensure_model_ready(&app).await {
            error!("Could not get the new model ready: {e}");
        }
    });
    Ok(())
}

/// Make the model ready: download it if it is missing, then load it. Used at
/// startup and by the settings window's retry button.
pub async fn ensure_model_ready(app: &AppHandle) -> Result<(), String> {
    let model_manager = app.state::<Arc<ModelManager>>();
    let transcription_manager = app.state::<Arc<TranscriptionManager>>();

    match model_manager.download().await {
        Ok(true) => {}
        // Cancelled by the user: leave the model for the next attempt.
        Ok(false) => return Ok(()),
        Err(e) => {
            // Log as well as emit: the toast is transient, and failed downloads
            // have historically been undiagnosable because logs showed
            // nothing (#1579).
            error!("Model download failed: {e}");
            let _ = app.emit("model-download-failed", e.to_string());
            return Err(e.to_string());
        }
    }
    transcription_manager.initiate_model_load();
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn download_model(app: AppHandle) -> Result<(), String> {
    ensure_model_ready(&app).await
}

#[tauri::command]
#[specta::specta]
pub fn cancel_download(model_manager: State<'_, Arc<ModelManager>>) {
    model_manager.cancel_download();
}
