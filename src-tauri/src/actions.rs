use crate::audio_feedback::{play_feedback_sound, play_feedback_sound_blocking, SoundType};
use crate::audio_toolkit::VadPolicy;
use crate::managers::audio::{recording_error_type, AudioRecordingManager};
use crate::managers::model::ModelManager;
use crate::managers::transcription::TranscriptionManager;
use crate::phone::{self, protocol::PhoneState};
use crate::settings::{get_settings, OverlayStyle};
use crate::shortcut;
use crate::tray::{set_tray_state, TrayIconState};
use crate::utils::{self, show_recording_overlay, show_transcribing_overlay};
use crate::TranscriptionCoordinator;
use log::{debug, error, warn};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tauri::Manager;
use tauri::{AppHandle, Emitter};
use tauri_specta::Event;

/// How a dictation ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum TranscriptionOutcome {
    /// The text was typed.
    Typed,
    /// Nothing was heard, so nothing was typed.
    Empty,
    /// The microphone sent only digital silence (e.g. muted).
    Silent,
    /// Transcribing or typing failed.
    Failed,
}

/// Emitted when a dictation finishes (not on cancel).
#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct TranscriptionCompleteEvent {
    pub outcome: TranscriptionOutcome,
    pub audio_seconds: f64,
    pub wait_seconds: f64,
}

fn emit_transcription_complete(
    app: &AppHandle,
    outcome: TranscriptionOutcome,
    sample_count: usize,
    stop_time: Instant,
) {
    let event = TranscriptionCompleteEvent {
        outcome,
        audio_seconds: sample_count as f64
            / crate::audio_toolkit::constants::WHISPER_SAMPLE_RATE as f64,
        wait_seconds: stop_time.elapsed().as_secs_f64(),
    };
    if let Err(e) = event.emit(app) {
        warn!("Failed to emit transcription complete: {e}");
    }
}

/// Nothing to type: `Silent` if the microphone sent only zeros, else `Empty`.
fn empty_outcome(input_silent: bool) -> TranscriptionOutcome {
    if input_silent {
        TranscriptionOutcome::Silent
    } else {
        TranscriptionOutcome::Empty
    }
}

/// Bumped per recording session, so a late typing run from an earlier session
/// doesn't reset the tray or hide the overlay of a newer one.
static RECORDING_SESSION: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, serde::Serialize)]
struct RecordingErrorEvent {
    error_type: String,
    detail: Option<String>,
}

/// Drop guard that finishes the transcription pipeline on every exit path.
struct FinishGuard(AppHandle);
impl Drop for FinishGuard {
    fn drop(&mut self) {
        if let Some(c) = self.0.try_state::<TranscriptionCoordinator>() {
            c.notify_processing_finished();
        }
        // The pipeline just freed its large transient buffers (captured PCM,
        // engine scratch); hand the cached pages back to the OS so they don't
        // sit in malloc arenas until they get swapped out (#1792).
        crate::memory::trim_freed_memory();
    }
}

// Shortcut Action Trait
pub trait ShortcutAction: Send + Sync {
    fn start(&self, app: &AppHandle, binding_id: &str, shortcut_str: &str);
    fn stop(&self, app: &AppHandle, binding_id: &str, shortcut_str: &str);
}

// Transcribe Action
struct TranscribeAction;

fn should_use_streaming_overlay(style: OverlayStyle, is_streaming: bool) -> bool {
    style == OverlayStyle::Live && is_streaming
}

impl ShortcutAction for TranscribeAction {
    fn start(&self, app: &AppHandle, binding_id: &str, _shortcut_str: &str) {
        let start_time = Instant::now();
        debug!("TranscribeAction::start called for binding: {}", binding_id);

        // Load model in the background (a no-op once it is loaded)
        let tm = app.state::<Arc<TranscriptionManager>>();
        let rm = app.state::<Arc<AudioRecordingManager>>();

        // Load ASR model and VAD model in parallel
        let kickoff_started = Instant::now();
        tm.initiate_model_load();
        let rm_clone = Arc::clone(&rm);
        std::thread::spawn(move || {
            if let Err(e) = rm_clone.preload_vad() {
                debug!("VAD pre-load failed: {}", e);
            }
        });
        let kickoff_elapsed = kickoff_started.elapsed();

        // Don't open the mic if nothing can transcribe the recording.
        if !tm.is_model_loaded() && !app.state::<Arc<ModelManager>>().is_downloaded() {
            warn!("Not starting recording: the model is not downloaded yet");
            return;
        }

        let binding_id = binding_id.to_string();
        RECORDING_SESSION.fetch_add(1, Ordering::SeqCst);
        let tray_started = Instant::now();
        set_tray_state(app, TrayIconState::Recording);
        let tray_elapsed = tray_started.elapsed();

        // Get the microphone mode to determine audio feedback timing
        let plan_started = Instant::now();
        let settings = get_settings(app);
        let is_always_on = settings.always_on_microphone;

        let model_streams = tm.expects_streaming();
        let vad_policy = if !settings.vad_enabled {
            VadPolicy::Disabled
        } else if model_streams {
            VadPolicy::Streaming
        } else {
            VadPolicy::Offline
        };
        if model_streams {
            tm.start_stream();
        }
        let plan_elapsed = plan_started.elapsed();

        // The live overlay needs a streaming model; any other gets the compact
        // pill instead of an oversized transparent live window.
        let overlay_started = Instant::now();
        match settings.overlay_style {
            OverlayStyle::Live if model_streams => utils::show_streaming_overlay(app),
            OverlayStyle::Live | OverlayStyle::Minimal => show_recording_overlay(app),
            // No overlay, but the phone page still shows that it is recording.
            OverlayStyle::None => phone::notify_state(app, PhoneState::Recording),
        }
        // Everything above runs before capture can begin, so each span here is
        // added keypress->capture latency.
        debug!(
            "start-path pre-recording steps: model_kickoff={:?} tray={:?} settings+stream_plan={:?} overlay={:?}",
            kickoff_elapsed,
            tray_elapsed,
            plan_elapsed,
            overlay_started.elapsed()
        );
        debug!("Microphone mode - always_on: {}", is_always_on);

        // Pause the music before the microphone opens: some headsets use one
        // audio device for both, and an open microphone would make an idle
        // device look busy.
        rm.pause_media();

        let mut recording_error: Option<anyhow::Error> = None;
        let recording_start_time = Instant::now();
        match rm.try_start_recording(&binding_id, vad_policy) {
            Ok(readiness) => {
                debug!(
                    "Recording request accepted in {:?}; waiting for first microphone samples",
                    recording_start_time.elapsed()
                );
                let generation = readiness.generation();
                let app_clone = app.clone();
                let rm_clone = Arc::clone(&rm);
                std::thread::spawn(move || {
                    if !readiness.wait() {
                        debug!("Microphone readiness wait ended without receiving samples");
                        return;
                    }

                    if !rm_clone.is_recording_readiness_current(generation) {
                        debug!("Microphone became ready for an inactive recording");
                        return;
                    }

                    debug!("Microphone is receiving samples; recording is ready");
                    utils::emit_recording_ready(&app_clone);

                    // The start chime is a readiness cue, so it must follow the
                    // first real input callback rather than Stream::play() or a
                    // fixed delay. The helper returns immediately when feedback
                    // is disabled.
                    if rm_clone.is_recording_readiness_current(generation) {
                        play_feedback_sound_blocking(&app_clone, SoundType::Start);
                    }
                });
            }
            Err(e) => {
                debug!("Failed to start recording: {}", e);
                recording_error = Some(e);
            }
        }

        if recording_error.is_none() {
            // Dynamically register the cancel shortcut in a separate task to avoid deadlock
            shortcut::register_cancel_shortcut(app);
        } else {
            // Starting failed (for example due to blocked microphone permissions).
            // Revert UI state so we don't stay stuck in the recording overlay.
            tm.cancel_stream();
            utils::hide_recording_overlay(app);
            set_tray_state(app, TrayIconState::Idle);
            // A rejected second start ("Already recording") must not resume
            // the music of the recording that is still going.
            if !rm.is_recording() {
                rm.resume_media();
            }
            if let Some(err) = recording_error {
                let _ = app.emit(
                    "recording-error",
                    RecordingErrorEvent {
                        error_type: recording_error_type(&err).to_string(),
                        detail: Some(format!("{err:#}")),
                    },
                );
            }
        }

        debug!(
            "TranscribeAction::start completed in {:?}",
            start_time.elapsed()
        );
    }

    fn stop(&self, app: &AppHandle, binding_id: &str, _shortcut_str: &str) {
        // Prevent a slow microphone from emitting a ready event or start chime
        // after the user has already requested stop.
        app.state::<Arc<AudioRecordingManager>>()
            .invalidate_recording_readiness();

        // Unregister the cancel shortcut when transcription stops
        shortcut::unregister_cancel_shortcut(app);

        let stop_time = Instant::now();
        debug!("TranscribeAction::stop called for binding: {}", binding_id);

        let ah = app.clone();
        let rm = Arc::clone(&app.state::<Arc<AudioRecordingManager>>());
        let tm = Arc::clone(&app.state::<Arc<TranscriptionManager>>());

        set_tray_state(app, TrayIconState::Transcribing);
        // Stop should give immediate visual feedback. Live streaming can keep
        // the larger panel, but it still switches from listening to a working
        // spinner while the stream finalizes. Non-streaming paths use the
        // compact transcribing pill (None no-ops in show_*).
        let style = get_settings(app).overlay_style;
        // Capture this before finalizing the stream so every later working state
        // targets the same overlay that was shown for this transcription.
        let use_streaming_overlay = should_use_streaming_overlay(style, tm.is_streaming());
        if use_streaming_overlay {
            // The live overlay stays up, so tell the phone page directly.
            phone::notify_state(app, PhoneState::Transcribing);
            tm.emit_stream_working();
        } else {
            show_transcribing_overlay(app);
        }

        // Play the music again as soon as the user stops talking.
        rm.resume_media();

        // Play audio feedback for recording stop
        play_feedback_sound(app, SoundType::Stop);

        let binding_id = binding_id.to_string(); // Clone binding_id for the async task
        let cancel_generation = rm.cancel_generation();

        tauri::async_runtime::spawn(async move {
            let _guard = FinishGuard(ah.clone());
            debug!(
                "Starting async transcription task for binding: {}",
                binding_id
            );

            let stop_recording_time = Instant::now();
            if let Some(samples) = rm.stop_recording(&binding_id, cancel_generation) {
                // Read now: by the time transcribing ends, another take may
                // have replaced it.
                let input_silent = rm.last_input_silent();
                debug!(
                    "Recording stopped and samples retrieved in {:?}, sample count: {}",
                    stop_recording_time.elapsed(),
                    samples.len()
                );

                if rm.was_cancelled_since(cancel_generation) {
                    debug!("Transcription operation cancelled after recording stop");
                    tm.cancel_stream();
                    utils::hide_recording_overlay(&ah);
                    set_tray_state(&ah, TrayIconState::Idle);
                    return;
                }

                if samples.is_empty() {
                    debug!("Recording produced no audio samples");
                    // Tear down any streaming worker so its channel doesn't leak
                    // and block the next start_stream.
                    tm.cancel_stream();
                    utils::hide_recording_overlay(&ah);
                    set_tray_state(&ah, TrayIconState::Idle);
                    emit_transcription_complete(&ah, empty_outcome(input_silent), 0, stop_time);
                } else {
                    let sample_count = samples.len();

                    // The live decode's result when it has one (all audio was
                    // already fed to it), otherwise a batch run of the samples.
                    // A cancel is surfaced as an error, so it never starts a
                    // batch run.
                    let transcription_time = Instant::now();
                    let transcription_result = tm.transcribe_recording(samples);

                    if rm.was_cancelled_since(cancel_generation) {
                        debug!("Transcription operation cancelled before typing");
                        utils::hide_recording_overlay(&ah);
                        set_tray_state(&ah, TrayIconState::Idle);
                        return;
                    }

                    match transcription_result {
                        Ok(transcription) => {
                            debug!(
                                "Transcription completed in {:?}: '{}'",
                                transcription_time.elapsed(),
                                utils::redact_text(&transcription)
                            );

                            if transcription.is_empty() {
                                utils::hide_recording_overlay(&ah);
                                set_tray_state(&ah, TrayIconState::Idle);
                                emit_transcription_complete(
                                    &ah,
                                    empty_outcome(input_silent),
                                    sample_count,
                                    stop_time,
                                );
                            } else {
                                let ah_clone = ah.clone();
                                let paste_time = Instant::now();
                                let rm_for_paste = Arc::clone(&rm);
                                // Taken before the typing thread exists, and so
                                // before the next session can start.
                                let session = RECORDING_SESSION.load(Ordering::SeqCst);
                                // Off the main thread: typing sleeps between keys, and
                                // typing into Chatelle's own windows needs the main thread free.
                                std::thread::Builder::new()
                                    .name("paste".into())
                                    .spawn(move || {
                                        if rm_for_paste.was_cancelled_since(cancel_generation) {
                                            debug!(
                                                "Transcription operation cancelled before typing"
                                            );
                                            utils::hide_recording_overlay(&ah_clone);
                                            set_tray_state(&ah_clone, TrayIconState::Idle);
                                            return;
                                        }

                                        let outcome =
                                            match utils::paste(transcription, ah_clone.clone()) {
                                                Ok(()) => {
                                                    debug!(
                                                        "Text pasted successfully in {:?}",
                                                        paste_time.elapsed()
                                                    );
                                                    TranscriptionOutcome::Typed
                                                }
                                                Err(e) => {
                                                    error!("Failed to type transcription: {}", e);
                                                    let _ = ah_clone.emit("paste-error", ());
                                                    TranscriptionOutcome::Failed
                                                }
                                            };
                                        // A new session may have started while
                                        // this ran; the UI is now its.
                                        if RECORDING_SESSION.load(Ordering::SeqCst) == session {
                                            utils::hide_recording_overlay(&ah_clone);
                                            set_tray_state(&ah_clone, TrayIconState::Idle);
                                        }
                                        emit_transcription_complete(
                                            &ah_clone,
                                            outcome,
                                            sample_count,
                                            stop_time,
                                        );
                                    })
                                    .map(|_| ())
                                    .unwrap_or_else(|e| {
                                        error!("Failed to start the typing thread: {:?}", e);
                                        utils::hide_recording_overlay(&ah);
                                        set_tray_state(&ah, TrayIconState::Idle);
                                        emit_transcription_complete(
                                            &ah,
                                            TranscriptionOutcome::Failed,
                                            sample_count,
                                            stop_time,
                                        );
                                    });
                            }
                        }
                        Err(err) => {
                            if rm.was_cancelled_since(cancel_generation) {
                                debug!(
                                    "Transcription operation cancelled after transcription error"
                                );
                                utils::hide_recording_overlay(&ah);
                                set_tray_state(&ah, TrayIconState::Idle);
                                return;
                            }

                            error!("Transcription failed: {}", err);
                            // Surface the failure to the UI (toast). The full
                            // message is also in chatelle.log via the line above.
                            let _ = ah.emit("transcription-error", err.to_string());
                            utils::hide_recording_overlay(&ah);
                            set_tray_state(&ah, TrayIconState::Idle);
                            emit_transcription_complete(
                                &ah,
                                TranscriptionOutcome::Failed,
                                sample_count,
                                stop_time,
                            );
                        }
                    }
                }
            } else {
                debug!("No samples retrieved from recording stop");
                // Tear down any streaming worker so its channel doesn't leak.
                tm.cancel_stream();
                utils::hide_recording_overlay(&ah);
                set_tray_state(&ah, TrayIconState::Idle);
                if !rm.was_cancelled_since(cancel_generation) {
                    emit_transcription_complete(&ah, TranscriptionOutcome::Empty, 0, stop_time);
                }
            }
        });

        debug!(
            "TranscribeAction::stop completed in {:?}",
            stop_time.elapsed()
        );
    }
}

// Cancel Action
struct CancelAction;

impl ShortcutAction for CancelAction {
    fn start(&self, app: &AppHandle, _binding_id: &str, _shortcut_str: &str) {
        utils::cancel_current_operation(app);
    }

    fn stop(&self, _app: &AppHandle, _binding_id: &str, _shortcut_str: &str) {
        // Nothing to do on stop for cancel
    }
}

// Draft Reply Action
struct DraftReplyAction;

fn restore_overlay_or_hide(app: &AppHandle) {
    let settings = crate::settings::get_settings(app);
    if settings.show_floating_dock && settings.overlay_style != crate::settings::OverlayStyle::None
    {
        crate::overlay::show_dock_overlay(app);
    } else {
        utils::hide_recording_overlay(app);
    }
}

/// True while a reply is being drafted, so a second press does not start a second request.
static DRAFTING: AtomicBool = AtomicBool::new(false);
static DRAFT_CANCEL_GENERATION: AtomicU64 = AtomicU64::new(0);
static HINT_SCREEN_JPEG: std::sync::Mutex<Option<Vec<u8>>> = std::sync::Mutex::new(None);
static HINT_START_TIME: AtomicU64 = AtomicU64::new(0);
static HINT_LOCKED_TAP: AtomicBool = AtomicBool::new(false);

/// Drafts on offer in the overlay, waiting for the user to pick one.
struct PendingChoices {
    drafts: Vec<String>,
    jpeg: Vec<u8>,
    hint: Option<String>,
}

static PENDING_CHOICES: std::sync::Mutex<Option<PendingChoices>> = std::sync::Mutex::new(None);

/// Cancels an in-flight draft reply operation.
pub fn cancel_drafting() {
    DRAFT_CANCEL_GENERATION.fetch_add(1, Ordering::SeqCst);
    DRAFTING.store(false, Ordering::SeqCst);
    HINT_LOCKED_TAP.store(false, Ordering::SeqCst);
    *PENDING_CHOICES.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// True while a draft is being written or waits to be picked, so the cancel
/// shortcut (Esc) works then too, not only while recording.
pub fn is_drafting() -> bool {
    DRAFTING.load(Ordering::SeqCst) || PENDING_CHOICES.lock().map(|p| p.is_some()).unwrap_or(false)
}

/// Shows the drafts in the overlay and waits for `choose_draft`; Esc cancels.
fn offer_choices(app: &AppHandle, drafts: Vec<String>, jpeg: Vec<u8>, hint: Option<String>) {
    let _ = app.emit("draft-choices", drafts.clone());
    *PENDING_CHOICES.lock().unwrap_or_else(|e| e.into_inner()) =
        Some(PendingChoices { drafts, jpeg, hint });
    set_tray_state(app, TrayIconState::Idle);
    crate::overlay::show_choosing_overlay(app);
    shortcut::register_cancel_shortcut(app);
}

/// Throws away the drafts on offer and writes three new ones from the same
/// screenshot and hint, asked not to repeat the ones just shown.
pub fn regenerate_choices(app: &AppHandle) -> Result<(), String> {
    rewrite_choices(app, None)
}

/// Rewrites the drafts on offer with a tone button's change ("shorter",
/// "longer", "funnier", "serious"), keeping what they say.
pub fn retone_choices(app: &AppHandle, tone: &str) -> Result<(), String> {
    let instruction =
        crate::reply::tone_instruction(tone).ok_or_else(|| format!("Unknown tone: {tone}"))?;
    rewrite_choices(app, Some(instruction))
}

/// Writes new drafts for the picker: fresh ones (no tone), or the ones on
/// offer changed by a tone instruction.
fn rewrite_choices(app: &AppHandle, tone: Option<&'static str>) -> Result<(), String> {
    let pending = PENDING_CHOICES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .ok_or("No drafts are waiting to be picked")?;
    if DRAFTING.swap(true, Ordering::SeqCst) {
        return Err("A reply is already being drafted".to_string());
    }
    let generation = DRAFT_CANCEL_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    set_tray_state(app, TrayIconState::Transcribing);
    utils::show_drafting_overlay(app);
    let app = app.clone();
    std::thread::spawn(move || {
        let shown = pending.drafts.join(" / ");
        let result = match tone {
            // Fresh replies: unlike the ones just shown.
            None => crate::reply::draft_for_typing(
                &pending.jpeg,
                pending.hint.as_deref(),
                Some(&shown),
                true,
                None,
            ),
            // The same replies, changed: they are the starting point.
            Some(instruction) => {
                let tone =
                    format!("{instruction} Keep what these replies say, only change how: {shown}");
                crate::reply::draft_for_typing(
                    &pending.jpeg,
                    pending.hint.as_deref(),
                    None,
                    true,
                    Some(&tone),
                )
            }
        };
        if DRAFT_CANCEL_GENERATION.load(Ordering::SeqCst) == generation {
            match result {
                Ok(drafts) if !drafts.is_empty() => {
                    offer_choices(&app, drafts, pending.jpeg, pending.hint);
                }
                Ok(_) => restore_overlay_or_hide(&app),
                Err(e) => {
                    error!("Failed to write new drafts: {e}");
                    let _ = app.emit("draft-reply-error", e);
                    set_tray_state(&app, TrayIconState::Idle);
                    restore_overlay_or_hide(&app);
                }
            }
        }
        DRAFTING.store(false, Ordering::SeqCst);
    });
    Ok(())
}

/// Types the draft the user picked (0-based) and learns from it as from any
/// typed draft.
pub fn choose_draft(app: &AppHandle, index: usize) -> Result<(), String> {
    let pending = PENDING_CHOICES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .ok_or("No drafts are waiting to be picked")?;
    let reply = pending
        .drafts
        .get(index)
        .cloned()
        .ok_or("There is no draft with that number")?;
    shortcut::unregister_cancel_shortcut(app);
    let app = app.clone();
    // Typing sleeps between keys: never on the command's thread.
    std::thread::spawn(move || {
        crate::reply::accept_draft(&reply);
        if let Some(coords) = crate::reply::get_last_input_box() {
            crate::input::focus_input_box(&app, coords);
        }
        if let Err(e) = crate::clipboard::paste(reply, app.clone()) {
            error!("Failed to type the chosen draft: {e}");
            let _ = app.emit("draft-reply-error", e.to_string());
        }
        restore_overlay_or_hide(&app);
    });
    Ok(())
}

impl ShortcutAction for DraftReplyAction {
    fn start(&self, app: &AppHandle, _binding_id: &str, _shortcut_str: &str) {
        if DRAFTING.swap(true, Ordering::SeqCst) {
            debug!("A reply is already being drafted");
            return;
        }
        let generation = DRAFT_CANCEL_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
        let app_clone = app.clone();

        // Immediate visual and audio cues: set tray to working, display drafting overlay pill, and play chime
        set_tray_state(app, TrayIconState::Transcribing);
        utils::show_drafting_overlay(app);
        play_feedback_sound(app, SoundType::Start);
        shortcut::register_cancel_shortcut(app);

        // Reading the screen and calling the model take seconds, and typing sleeps
        // between keys: none of it may run on the shortcut's thread.
        std::thread::spawn(move || {
            let choose = crate::settings::get_settings(&app_clone).choose_before_typing;
            let result = crate::reply::capture_screen_jpeg().and_then(|jpeg| {
                crate::reply::draft_for_typing(&jpeg, None, None, choose, None)
                    .map(|drafts| (jpeg, drafts))
            });
            shortcut::unregister_cancel_shortcut(&app_clone);

            if DRAFT_CANCEL_GENERATION.load(Ordering::SeqCst) != generation {
                debug!("Draft reply operation cancelled during drafting");
                utils::hide_recording_overlay(&app_clone);
                set_tray_state(&app_clone, TrayIconState::Idle);
                DRAFTING.store(false, Ordering::SeqCst);
                return;
            }

            match result {
                Ok((jpeg, drafts)) => {
                    play_feedback_sound(&app_clone, SoundType::Stop);
                    if DRAFT_CANCEL_GENERATION.load(Ordering::SeqCst) != generation {
                        debug!("Draft reply operation cancelled before typing");
                        utils::hide_recording_overlay(&app_clone);
                        set_tray_state(&app_clone, TrayIconState::Idle);
                        DRAFTING.store(false, Ordering::SeqCst);
                        return;
                    }

                    if drafts.len() > 1 {
                        offer_choices(&app_clone, drafts, jpeg, None);
                    } else if let Some(reply) = drafts.into_iter().next() {
                        if let Some(coords) = crate::reply::get_last_input_box() {
                            crate::input::focus_input_box(&app_clone, coords);
                        }
                        if let Err(e) = crate::clipboard::paste(reply, app_clone.clone()) {
                            error!("Failed to type the drafted reply: {e}");
                            let _ = app_clone.emit("draft-reply-error", e.to_string());
                        }
                        if DRAFT_CANCEL_GENERATION.load(Ordering::SeqCst) == generation {
                            restore_overlay_or_hide(&app_clone);
                            set_tray_state(&app_clone, TrayIconState::Idle);
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to draft a reply: {e}");
                    utils::hide_recording_overlay(&app_clone);
                    set_tray_state(&app_clone, TrayIconState::Idle);
                    let _ = app_clone.emit("draft-reply-error", e);
                }
            }
            DRAFTING.store(false, Ordering::SeqCst);
        });
    }

    fn stop(&self, _app: &AppHandle, _binding_id: &str, _shortcut_str: &str) {}
}

// Draft Reply With Hint Action: hold shortcut, speak voice guidance, and release to draft
struct DraftReplyWithHintAction;

impl ShortcutAction for DraftReplyWithHintAction {
    fn start(&self, app: &AppHandle, binding_id: &str, shortcut_str: &str) {
        let rm = app.state::<Arc<AudioRecordingManager>>();
        if rm.is_recording() {
            if HINT_LOCKED_TAP.load(Ordering::SeqCst) {
                // If recording was locked on by a previous tap, this second press stops it!
                HINT_LOCKED_TAP.store(false, Ordering::SeqCst);
                self.stop(app, binding_id, shortcut_str);
                return;
            }
            debug!("Already recording audio");
            return;
        }
        if DRAFTING.swap(true, Ordering::SeqCst) {
            debug!("A reply is already being drafted");
            return;
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        HINT_START_TIME.store(now, Ordering::SeqCst);
        HINT_LOCKED_TAP.store(false, Ordering::SeqCst);

        let _ = app.emit("hint-recording-started", ());

        // Kick off transcription model preloading if not yet loaded
        let tm = app.state::<Arc<TranscriptionManager>>();
        tm.initiate_model_load();

        // Capture screen immediately in the background while audio recording begins
        {
            let mut guard = HINT_SCREEN_JPEG.lock().unwrap_or_else(|e| e.into_inner());
            *guard = None;
        }
        std::thread::spawn(|| {
            if let Ok(jpeg) = crate::reply::capture_screen_jpeg() {
                let mut guard = HINT_SCREEN_JPEG.lock().unwrap_or_else(|e| e.into_inner());
                *guard = Some(jpeg);
            }
        });

        // Start recording audio
        rm.pause_media();
        match rm.try_start_recording(binding_id, VadPolicy::Offline) {
            Ok(readiness) => {
                let generation = readiness.generation();
                let app_clone = app.clone();
                let rm_clone = Arc::clone(&rm);
                std::thread::spawn(move || {
                    if !readiness.wait() || !rm_clone.is_recording_readiness_current(generation) {
                        return;
                    }
                    utils::emit_recording_ready(&app_clone);
                    if rm_clone.is_recording_readiness_current(generation) {
                        play_feedback_sound_blocking(&app_clone, SoundType::Start);
                    }
                });
            }
            Err(e) => {
                // Undo the start: stop() returns early when nothing is
                // recording, so it would never clear DRAFTING, and every
                // later draft would be refused as already running.
                warn!("Failed to start recording for voice hint: {e}");
                rm.resume_media();
                DRAFTING.store(false, Ordering::SeqCst);
                let _ = app.emit("hint-recording-stopped", ());
                let _ = app.emit(
                    "draft-reply-error",
                    format!("Could not start the microphone: {e}"),
                );
                set_tray_state(app, TrayIconState::Idle);
                restore_overlay_or_hide(app);
                return;
            }
        }

        set_tray_state(app, TrayIconState::Recording);
        crate::overlay::show_hinting_overlay(app);
        shortcut::register_cancel_shortcut(app);
    }

    fn stop(&self, app: &AppHandle, binding_id: &str, _shortcut_str: &str) {
        let rm = Arc::clone(&app.state::<Arc<AudioRecordingManager>>());
        if !rm.is_recording() {
            return;
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let start = HINT_START_TIME.load(Ordering::SeqCst);
        let elapsed = now.saturating_sub(start);

        // If stopped almost immediately from a shortcut release (< 250ms),
        // treat this as a tap that locks recording on so the user can speak freely.
        if elapsed < 250 && !HINT_LOCKED_TAP.load(Ordering::SeqCst) {
            debug!("Hint shortcut released within {elapsed}ms - locking recording on for tap");
            HINT_LOCKED_TAP.store(true, Ordering::SeqCst);
            return;
        }
        HINT_LOCKED_TAP.store(false, Ordering::SeqCst);

        let _ = app.emit("hint-recording-stopped", ());

        let tm = Arc::clone(&app.state::<Arc<TranscriptionManager>>());
        rm.invalidate_recording_readiness();
        shortcut::unregister_cancel_shortcut(app);

        let cancel_generation = rm.cancel_generation();
        let samples = rm.stop_recording(binding_id, cancel_generation);
        rm.resume_media();

        set_tray_state(app, TrayIconState::Transcribing);
        show_transcribing_overlay(app);
        play_feedback_sound(app, SoundType::Stop);

        let ah = app.clone();
        let generation = DRAFT_CANCEL_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;

        std::thread::spawn(move || {
            let _guard = FinishGuard(ah.clone());

            let hint = if let Some(audio) = samples {
                if !audio.is_empty() {
                    match tm.transcribe_recording(audio) {
                        Ok(t) => {
                            let trimmed = t.trim().to_string();
                            if trimmed.is_empty() {
                                None
                            } else {
                                Some(trimmed)
                            }
                        }
                        Err(e) => {
                            warn!("Failed to transcribe hint audio: {e}");
                            None
                        }
                    }
                } else {
                    None
                }
            } else {
                None
            };

            if DRAFT_CANCEL_GENERATION.load(Ordering::SeqCst) != generation
                || rm.was_cancelled_since(cancel_generation)
            {
                debug!("Draft with hint operation cancelled during hint processing");
                utils::hide_recording_overlay(&ah);
                set_tray_state(&ah, TrayIconState::Idle);
                DRAFTING.store(false, Ordering::SeqCst);
                return;
            }

            let jpeg = {
                let mut guard = HINT_SCREEN_JPEG.lock().unwrap_or_else(|e| e.into_inner());
                guard.take()
            };
            let jpeg_bytes = match jpeg {
                Some(b) => Ok(b),
                None => crate::reply::capture_screen_jpeg(),
            };

            let choose = crate::settings::get_settings(&ah).choose_before_typing;
            let result = jpeg_bytes.and_then(|bytes| {
                crate::reply::draft_for_typing(&bytes, hint.as_deref(), None, choose, None)
                    .map(|drafts| (bytes, drafts))
            });

            if DRAFT_CANCEL_GENERATION.load(Ordering::SeqCst) != generation
                || rm.was_cancelled_since(cancel_generation)
            {
                debug!("Draft with hint operation cancelled before typing");
                utils::hide_recording_overlay(&ah);
                set_tray_state(&ah, TrayIconState::Idle);
                DRAFTING.store(false, Ordering::SeqCst);
                return;
            }

            match result {
                Ok((bytes, drafts)) if drafts.len() > 1 => {
                    offer_choices(&ah, drafts, bytes, hint);
                }
                Ok((_, drafts)) => {
                    if let Some(reply) = drafts.into_iter().next() {
                        if let Some(coords) = crate::reply::get_last_input_box() {
                            crate::input::focus_input_box(&ah, coords);
                        }
                        if let Err(e) = crate::clipboard::paste(reply, ah.clone()) {
                            error!("Failed to type hint-drafted reply: {e}");
                            let _ = ah.emit("draft-reply-error", e.to_string());
                        }
                    }
                    if DRAFT_CANCEL_GENERATION.load(Ordering::SeqCst) == generation {
                        restore_overlay_or_hide(&ah);
                        set_tray_state(&ah, TrayIconState::Idle);
                    }
                }
                Err(e) => {
                    error!("Failed to draft reply with hint: {e}");
                    utils::hide_recording_overlay(&ah);
                    set_tray_state(&ah, TrayIconState::Idle);
                    let _ = ah.emit("draft-reply-error", e);
                }
            }
            DRAFTING.store(false, Ordering::SeqCst);
        });
    }
}

// Watch Chat Action
struct WatchChatAction;

impl ShortcutAction for WatchChatAction {
    fn start(&self, app: &AppHandle, _binding_id: &str, _shortcut_str: &str) {
        if crate::watch::is_watching() {
            crate::watch::stop_watch_session();
            play_feedback_sound(app, SoundType::Stop);
            let _ = app.emit("watch-state-changed", false);
        } else {
            match crate::watch::start_watch_session(app.clone()) {
                Ok(info) => {
                    play_feedback_sound(app, SoundType::Start);
                    let _ = app.emit("watch-state-changed", true);
                    log::info!("Started watching: {info}");
                }
                Err(e) => {
                    error!("Failed to start watch session: {e}");
                    let _ = app.emit("watch-error", e);
                }
            }
        }
    }

    fn stop(&self, _app: &AppHandle, _binding_id: &str, _shortcut_str: &str) {}
}

// Static Action Map
pub static ACTION_MAP: Lazy<HashMap<String, Arc<dyn ShortcutAction>>> = Lazy::new(|| {
    let mut map = HashMap::new();
    map.insert(
        "draft_reply".to_string(),
        Arc::new(DraftReplyAction) as Arc<dyn ShortcutAction>,
    );
    map.insert(
        "draft_reply_hint".to_string(),
        Arc::new(DraftReplyWithHintAction) as Arc<dyn ShortcutAction>,
    );
    map.insert(
        "watch_chat".to_string(),
        Arc::new(WatchChatAction) as Arc<dyn ShortcutAction>,
    );
    map.insert(
        "transcribe".to_string(),
        Arc::new(TranscribeAction) as Arc<dyn ShortcutAction>,
    );
    map.insert(
        "cancel".to_string(),
        Arc::new(CancelAction) as Arc<dyn ShortcutAction>,
    );
    map
});

pub fn trigger_draft_reply(app: &AppHandle) {
    if let Some(action) = ACTION_MAP.get("draft_reply") {
        action.start(app, "draft_reply", "");
    }
}

pub fn trigger_draft_reply_hint_start(app: &AppHandle) {
    if let Some(action) = ACTION_MAP.get("draft_reply_hint") {
        action.start(app, "draft_reply_hint", "");
    }
}

pub fn trigger_draft_reply_hint_stop(app: &AppHandle) {
    HINT_LOCKED_TAP.store(false, Ordering::SeqCst);
    if let Some(action) = ACTION_MAP.get("draft_reply_hint") {
        action.stop(app, "draft_reply_hint", "");
    }
}

pub fn trigger_draft_reply_hint_toggle(app: &AppHandle) {
    let rm = app.state::<Arc<AudioRecordingManager>>();
    if rm.is_recording() {
        trigger_draft_reply_hint_stop(app);
    } else {
        trigger_draft_reply_hint_start(app);
    }
}

pub fn trigger_transcribe_start(app: &AppHandle) {
    if let Some(action) = ACTION_MAP.get("transcribe") {
        action.start(app, "transcribe", "");
    }
}

pub fn trigger_transcribe_stop(app: &AppHandle) {
    if let Some(action) = ACTION_MAP.get("transcribe") {
        action.stop(app, "transcribe", "");
    }
}

pub fn trigger_transcribe_toggle(app: &AppHandle) {
    let rm = app.state::<Arc<crate::managers::audio::AudioRecordingManager>>();
    if rm.is_recording() {
        trigger_transcribe_stop(app);
    } else {
        trigger_transcribe_start(app);
    }
}

#[cfg(test)]
mod tests {
    use super::should_use_streaming_overlay;
    use crate::settings::OverlayStyle;

    #[test]
    fn live_overlay_uses_streaming_states_only_for_streaming_models() {
        assert!(should_use_streaming_overlay(OverlayStyle::Live, true));
        assert!(!should_use_streaming_overlay(OverlayStyle::Live, false));
        assert!(!should_use_streaming_overlay(OverlayStyle::Minimal, true));
        assert!(!should_use_streaming_overlay(OverlayStyle::None, true));
    }

    #[test]
    fn cancel_drafting_increments_generation_and_clears_drafting_flag() {
        use super::{cancel_drafting, DRAFTING, DRAFT_CANCEL_GENERATION};
        use std::sync::atomic::Ordering;

        DRAFTING.store(true, Ordering::SeqCst);
        let gen_before = DRAFT_CANCEL_GENERATION.load(Ordering::SeqCst);
        cancel_drafting();
        assert_eq!(DRAFTING.load(Ordering::SeqCst), false);
        assert_eq!(
            DRAFT_CANCEL_GENERATION.load(Ordering::SeqCst),
            gen_before + 1
        );
    }
}
