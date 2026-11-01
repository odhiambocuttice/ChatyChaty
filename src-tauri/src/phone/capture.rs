//! Cuts audio streamed from the phone into fixed-size chunks for a recorder sink.

use handy_recorder::{AudioChunk, Sink};
use std::fmt;
use std::sync::{Arc, Mutex};

/// The phone app sends mono PCM at this rate.
pub const PHONE_SAMPLE_RATE: u32 = 16_000;

#[derive(Debug)]
pub enum PhoneStartError {
    NotConnected,
    AlreadyRecording,
}

impl fmt::Display for PhoneStartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotConnected => write!(f, "Phone not connected — scan the QR code in Settings"),
            Self::AlreadyRecording => write!(f, "Already recording"),
        }
    }
}

impl std::error::Error for PhoneStartError {}

struct Inner<S> {
    frames_per_chunk: usize,
    connected: bool,
    sink: Option<S>,
    buffer: Vec<f32>,
}

/// Buffers phone audio and hands it to a sink in `frames_per_chunk` blocks.
pub struct PhoneCapture<S: Sink> {
    inner: Arc<Mutex<Inner<S>>>,
}

impl<S: Sink> Clone for PhoneCapture<S> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<S: Sink> PhoneCapture<S> {
    pub fn new(frames_per_chunk: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                frames_per_chunk,
                connected: false,
                sink: None,
                buffer: Vec::new(),
            })),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner<S>> {
        // A poisoned lock only means a sink panicked; the state is still usable.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Ignored while recording: the chunk size must stay fixed for one recording.
    pub fn set_frames_per_chunk(&self, frames: usize) {
        let mut inner = self.lock();
        if inner.sink.is_none() {
            inner.frames_per_chunk = frames;
        }
    }

    /// Only flips the flag. A recording in progress keeps its sink and audio.
    pub fn set_connected(&self, connected: bool) {
        self.lock().connected = connected;
    }

    pub fn is_connected(&self) -> bool {
        self.lock().connected
    }

    pub fn start(&self, sink: S) -> Result<(), (PhoneStartError, S)> {
        let mut inner = self.lock();
        if !inner.connected {
            return Err((PhoneStartError::NotConnected, sink));
        }
        if inner.sink.is_some() {
            return Err((PhoneStartError::AlreadyRecording, sink));
        }
        inner.buffer.clear();
        inner.sink = Some(sink);
        Ok(())
    }

    /// While recording, sends every full block to the sink. Otherwise drops the audio.
    pub fn push_samples(&self, samples: &[f32]) {
        let mut guard = self.lock();
        let inner = &mut *guard;
        let Some(sink) = inner.sink.as_mut() else {
            return;
        };
        let frames = inner.frames_per_chunk;
        if frames == 0 {
            return;
        }
        inner.buffer.extend_from_slice(samples);
        let mut sent = 0;
        while inner.buffer.len() - sent >= frames {
            sink.process_chunk(AudioChunk {
                samples: &inner.buffer[sent..sent + frames],
                sample_rate: PHONE_SAMPLE_RATE,
                channels: 1,
                valid_frames: frames,
            });
            sent += frames;
        }
        inner.buffer.drain(..sent);
    }

    /// Sends the remainder as one zero-padded chunk and returns the sink.
    pub fn stop(&self) -> Option<S> {
        let mut guard = self.lock();
        let inner = &mut *guard;
        let mut sink = inner.sink.take()?;
        let remainder = inner.buffer.len();
        if remainder > 0 {
            let mut padded = std::mem::take(&mut inner.buffer);
            padded.resize(inner.frames_per_chunk.max(remainder), 0.0);
            sink.process_chunk(AudioChunk {
                samples: &padded,
                sample_rate: PHONE_SAMPLE_RATE,
                channels: 1,
                valid_frames: remainder,
            });
        }
        inner.buffer.clear();
        Some(sink)
    }
}

/// Decodes little-endian signed 16-bit PCM. An odd trailing byte is ignored.
pub fn decode_pcm16le(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b) as f32 / 32768.0)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use handy_recorder::CollectingSink;

    #[test]
    fn start_without_phone_fails() {
        let c = PhoneCapture::new(512);
        let (err, _sink) = c.start(CollectingSink::new()).unwrap_err();
        assert!(matches!(err, PhoneStartError::NotConnected));
    }

    #[test]
    fn chunks_and_flushes_remainder() {
        let c = PhoneCapture::new(512);
        c.set_connected(true);
        c.start(CollectingSink::new()).unwrap();
        c.push_samples(&vec![0.25; 700]);
        assert_eq!(c.stop().unwrap().into_samples(), vec![0.25; 700]);
    }

    #[test]
    fn second_start_is_rejected() {
        let c = PhoneCapture::new(512);
        c.set_connected(true);
        c.start(CollectingSink::new()).unwrap();
        let (err, _sink) = c.start(CollectingSink::new()).unwrap_err();
        assert!(matches!(err, PhoneStartError::AlreadyRecording));
    }

    #[test]
    fn audio_before_start_is_dropped() {
        let c = PhoneCapture::new(4);
        c.set_connected(true);
        c.push_samples(&[1.0; 10]);
        c.start(CollectingSink::new()).unwrap();
        c.push_samples(&[0.5; 4]);
        assert_eq!(c.stop().unwrap().into_samples(), vec![0.5; 4]);
    }

    #[test]
    fn stop_after_disconnect_keeps_audio() {
        let c = PhoneCapture::new(4);
        c.set_connected(true);
        c.start(CollectingSink::new()).unwrap();
        c.push_samples(&[0.5; 6]);
        c.set_connected(false);
        c.set_connected(true); // reconnect, same recording
        c.push_samples(&[0.5; 2]);
        assert_eq!(c.stop().unwrap().into_samples(), vec![0.5; 8]);
    }

    #[test]
    fn stop_when_idle_returns_none() {
        assert!(PhoneCapture::<CollectingSink>::new(4).stop().is_none());
    }

    #[test]
    fn decodes_pcm16le() {
        assert_eq!(
            decode_pcm16le(&[0x00, 0x40, 0x00, 0xC0, 0x01]),
            vec![0.5, -0.5]
        );
    }
}
