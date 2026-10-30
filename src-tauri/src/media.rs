//! Pauses whatever is playing while Handy listens, and starts it again after.
//!
//! `pause` pauses only media that is playing right now and returns a record of
//! what it paused. `resume` plays exactly that again, so music the user had
//! already paused stays paused.
//!
//! - macOS has no public way to ask which app is playing media, so `pause`
//!   checks whether any other app is sending sound to an output device and then
//!   sends the Play/Pause media key. `resume` sends the key again.
//! - Windows asks the system media session list (the one behind the volume
//!   flyout) for sessions that are playing, and pauses and resumes those.
//! - Linux uses `playerctl`, which talks to any MPRIS player. Without
//!   `playerctl` installed it does nothing.
//!
//! Any failure is logged and ignored. Dictation never depends on this.

use log::{debug, warn};
use tauri::AppHandle;

/// What `pause` paused, handed back to `resume`.
#[derive(Debug)]
pub struct PausedMedia {
    /// Player names (Linux) or app ids (Windows) that were playing. macOS
    /// cannot tell players apart, so it leaves this empty.
    players: Vec<String>,
}

/// Pauses the media that is playing. Returns `None` when nothing was playing
/// or when pausing was not possible.
pub fn pause(app: &AppHandle) -> Option<PausedMedia> {
    let players = imp::pause(app)?;
    debug!("Paused media ({} named player(s))", players.len());
    Some(PausedMedia { players })
}

/// Plays again what `pause` paused.
pub fn resume(app: &AppHandle, paused: PausedMedia) {
    imp::resume(app, &paused.players);
    debug!("Resumed media");
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use enigo::{Direction, Key, Keyboard};
    use std::ffi::c_void;

    /// Mirrors CoreAudio's `AudioObjectPropertyAddress`.
    #[repr(C)]
    struct PropertyAddress {
        selector: u32,
        scope: u32,
        element: u32,
    }

    #[link(name = "CoreAudio", kind = "framework")]
    extern "C" {
        fn AudioObjectGetPropertyDataSize(
            object: u32,
            address: *const PropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            data_size: *mut u32,
        ) -> i32;
        fn AudioObjectGetPropertyData(
            object: u32,
            address: *const PropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            data_size: *mut u32,
            data: *mut c_void,
        ) -> i32;
    }

    const fn four_char_code(code: &[u8; 4]) -> u32 {
        u32::from_be_bytes(*code)
    }

    const SYSTEM_OBJECT: u32 = 1;
    /// `kAudioHardwarePropertyProcessObjectList`: one audio object per process
    /// that uses audio. Needs macOS 14.2; older systems fail the call.
    const PROCESS_OBJECT_LIST: u32 = four_char_code(b"prs#");
    /// `kAudioProcessPropertyPID`
    const PROCESS_PID: u32 = four_char_code(b"ppid");
    /// `kAudioProcessPropertyIsRunningOutput`
    const PROCESS_IS_RUNNING_OUTPUT: u32 = four_char_code(b"piro");

    fn address(selector: u32) -> PropertyAddress {
        PropertyAddress {
            selector,
            scope: four_char_code(b"glob"), // kAudioObjectPropertyScopeGlobal
            element: 0,                     // kAudioObjectPropertyElementMain
        }
    }

    /// Reads one `u32` property of an audio object.
    fn read_u32(object: u32, selector: u32) -> Option<u32> {
        let address = address(selector);
        let mut value: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        // SAFETY: `address`, `value` and `size` outlive the call, and `size`
        // matches the size of the `u32` that `data` points to.
        let status = unsafe {
            AudioObjectGetPropertyData(
                object,
                &address,
                0,
                std::ptr::null(),
                &mut size,
                (&mut value as *mut u32).cast(),
            )
        };
        (status == 0).then_some(value)
    }

    /// The audio object of every process that uses audio.
    fn audio_processes() -> Option<Vec<u32>> {
        let address = address(PROCESS_OBJECT_LIST);
        let mut size: u32 = 0;
        // SAFETY: `address` and `size` outlive the call.
        let status = unsafe {
            AudioObjectGetPropertyDataSize(SYSTEM_OBJECT, &address, 0, std::ptr::null(), &mut size)
        };
        if status != 0 {
            return None;
        }

        let mut objects = vec![0u32; size as usize / std::mem::size_of::<u32>()];
        // SAFETY: `objects` holds at least `size` bytes, and `size` is updated
        // to the number of bytes written.
        let status = unsafe {
            AudioObjectGetPropertyData(
                SYSTEM_OBJECT,
                &address,
                0,
                std::ptr::null(),
                &mut size,
                objects.as_mut_ptr().cast(),
            )
        };
        if status != 0 {
            return None;
        }
        objects.truncate(size as usize / std::mem::size_of::<u32>());
        Some(objects)
    }

    /// Whether another app is sending sound to an output device right now.
    ///
    /// This asks each process, not the output device: a device often stays "in
    /// use" long after the music stopped.
    fn another_app_is_playing() -> bool {
        let own_pid = std::process::id();
        audio_processes().is_some_and(|objects| {
            objects.into_iter().any(|object| {
                read_u32(object, PROCESS_PID) != Some(own_pid)
                    && read_u32(object, PROCESS_IS_RUNNING_OUTPUT).is_some_and(|out| out != 0)
            })
        })
    }

    fn press_play_pause(app: &AppHandle) -> bool {
        // Enigo must be driven from the main thread on macOS.
        crate::clipboard::with_enigo(app, |enigo| {
            enigo
                .key(Key::MediaPlayPause, Direction::Click)
                .map_err(|e| e.to_string())
        })
        .map_err(|e| warn!("Could not send the Play/Pause key: {e}"))
        .is_ok()
    }

    pub fn pause(app: &AppHandle) -> Option<Vec<String>> {
        if !another_app_is_playing() {
            debug!("Nothing is playing; not pausing");
            return None;
        }
        press_play_pause(app).then(Vec::new)
    }

    pub fn resume(app: &AppHandle, _players: &[String]) {
        press_play_pause(app);
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use super::*;
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSessionManager as SessionManager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
    };

    fn pause_playing_sessions() -> windows::core::Result<Vec<String>> {
        let manager = SessionManager::RequestAsync()?.get()?;
        let mut paused = Vec::new();
        for session in manager.GetSessions()? {
            let playing = session.GetPlaybackInfo()?.PlaybackStatus()? == PlaybackStatus::Playing;
            if playing && session.TryPauseAsync()?.get()? {
                paused.push(session.SourceAppUserModelId()?.to_string());
            }
        }
        Ok(paused)
    }

    fn play_sessions(app_ids: &[String]) -> windows::core::Result<()> {
        let manager = SessionManager::RequestAsync()?.get()?;
        for session in manager.GetSessions()? {
            if app_ids.contains(&session.SourceAppUserModelId()?.to_string()) {
                session.TryPlayAsync()?.get()?;
            }
        }
        Ok(())
    }

    /// Runs `f` on its own thread. Waiting on a WinRT call can block, which
    /// must not happen on a thread that pumps window messages.
    fn on_worker<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
        std::thread::spawn(f).join().ok()
    }

    pub fn pause(_app: &AppHandle) -> Option<Vec<String>> {
        let paused = on_worker(pause_playing_sessions)?
            .map_err(|e| warn!("Could not pause media: {e}"))
            .ok()?;
        (!paused.is_empty()).then_some(paused)
    }

    pub fn resume(_app: &AppHandle, players: &[String]) {
        let players = players.to_vec();
        if let Some(Err(e)) = on_worker(move || play_sessions(&players)) {
            warn!("Could not resume media: {e}");
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use std::process::Command;

    fn playerctl(args: &[&str]) -> Option<String> {
        let output = Command::new("playerctl").args(args).output().ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }

    pub fn pause(_app: &AppHandle) -> Option<Vec<String>> {
        let names = playerctl(&["--list-all"])?;
        let paused: Vec<String> = names
            .lines()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .filter(|name| {
                playerctl(&["--player", name, "status"]).is_some_and(|s| s.trim() == "Playing")
            })
            .filter(|name| playerctl(&["--player", name, "pause"]).is_some())
            .map(str::to_string)
            .collect();
        (!paused.is_empty()).then_some(paused)
    }

    pub fn resume(_app: &AppHandle, players: &[String]) {
        for name in players {
            playerctl(&["--player", name, "play"]);
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod imp {
    use super::*;

    pub fn pause(_app: &AppHandle) -> Option<Vec<String>> {
        None
    }

    pub fn resume(_app: &AppHandle, _players: &[String]) {}
}
