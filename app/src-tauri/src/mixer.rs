//! The G6's mixer: volume / mute of playback, monitoring and recording, and the
//! recording source. These are USB Audio Class feature units, not HID, so
//! they do not go through `g6-core`; the OS audio API owns them. macOS
//! (CoreAudio) and Windows (WASAPI) are implemented; elsewhere [`platform`]
//! gives [`Unsupported`] and the snapshot's `mixer` stays `null`.
//!
//! [`MixerBackend`] is the seam the [`Backend`](crate::backend::Backend) uses,
//! so its tests run on a fake mixer.

use std::fmt;
use std::sync::Arc;

use crate::api::{ApiError, ErrorCode, Mixer, MixerScope};

#[cfg(target_os = "macos")]
mod coreaudio;
#[cfg(windows)]
mod wasapi;

/// Why a mixer call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MixerError {
    /// Not supported here, the G6 audio device is not there, or the OS offers no (writable) such control.
    Unavailable(String),
    /// The OS audio API reported an error.
    Failed(String),
}

impl fmt::Display for MixerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MixerError::Unavailable(m) | MixerError::Failed(m) => f.write_str(m),
        }
    }
}

impl From<MixerError> for ApiError {
    fn from(e: MixerError) -> ApiError {
        let code = match e {
            MixerError::Unavailable(_) => ErrorCode::MixerUnavailable,
            MixerError::Failed(_) => ErrorCode::Internal,
        };
        ApiError::new(code, e.to_string())
    }
}

/// Called when any mixer property may have changed (any thread, keep it short).
pub type Notify = Box<dyn Fn() + Send + Sync>;

/// Access to the G6's audio controls. Values are the wire types of `api.rs`;
/// the caller validates ranges, the implementation validates what only it can
/// know (that the control exists and is writable).
pub trait MixerBackend: Send + Sync + 'static {
    /// Current controls, `None` when unsupported or the G6 audio device is not
    /// found. Never writes.
    fn read(&self) -> Option<Mixer>;
    /// Set both channels of `scope` to `volume` (0..1).
    fn set_volume(&self, scope: MixerScope, volume: f32) -> Result<(), MixerError>;
    fn set_mute(&self, scope: MixerScope, mute: bool) -> Result<(), MixerError>;
    /// Select the recording source (an id from `recording_source.options`).
    fn set_source(&self, id: u32) -> Result<(), MixerError>;
    /// Start calling `notify` when a control, or the presence of the device,
    /// may have changed. Called once; the notification is a hint, the caller
    /// re-reads with [`read`](Self::read).
    fn subscribe(&self, notify: Notify);
}

/// No mixer on this platform: the snapshot's `mixer` is `null` and the UI hides the tab.
pub struct Unsupported;

impl MixerBackend for Unsupported {
    fn read(&self) -> Option<Mixer> {
        None
    }
    fn set_volume(&self, _: MixerScope, _: f32) -> Result<(), MixerError> {
        Err(unsupported())
    }
    fn set_mute(&self, _: MixerScope, _: bool) -> Result<(), MixerError> {
        Err(unsupported())
    }
    fn set_source(&self, _: u32) -> Result<(), MixerError> {
        Err(unsupported())
    }
    fn subscribe(&self, _: Notify) {}
}

fn unsupported() -> MixerError {
    MixerError::Unavailable("the mixer is not supported on this platform".into())
}

/// The mixer of this platform (CoreAudio on macOS).
#[cfg(target_os = "macos")]
pub fn platform() -> Arc<dyn MixerBackend> {
    Arc::new(coreaudio::CoreAudioMixer::new())
}

/// The mixer of this platform (WASAPI on Windows).
#[cfg(windows)]
pub fn platform() -> Arc<dyn MixerBackend> {
    Arc::new(wasapi::WasapiMixer)
}

/// The mixer of this platform (none).
#[cfg(not(any(target_os = "macos", windows)))]
pub fn platform() -> Arc<dyn MixerBackend> {
    Arc::new(Unsupported)
}

#[cfg(test)]
pub use fake::FakeMixer;

#[cfg(test)]
mod fake {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use super::*;
    use crate::api::{MixerLevel, MixerSource, MixerSources};

    /// In-memory mixer for tests. Counts the calls that would reach the OS.
    pub struct FakeMixer {
        state: Mutex<Option<Mixer>>,
        notify: Mutex<Option<Notify>>,
        writes: AtomicUsize,
    }

    fn level(volume: f32, mute: Option<bool>) -> MixerLevel {
        MixerLevel {
            volume,
            volume_db: Some(volume * 60.0 - 60.0),
            mute,
        }
    }

    impl FakeMixer {
        /// A G6-like mixer: three scopes and four recording sources.
        pub fn g6() -> FakeMixer {
            let sources = ["Line In", "External Mic", "S/PDIF In", "What U Hear"];
            FakeMixer::with(Some(Mixer {
                playback: Some(level(0.5, Some(false))),
                monitoring: Some(level(0.75, Some(false))),
                recording: Some(level(0.7, Some(false))),
                recording_source: Some(MixerSources {
                    current: 1,
                    options: sources
                        .iter()
                        .zip(1u32..)
                        .map(|(n, id)| MixerSource {
                            id,
                            name: (*n).into(),
                        })
                        .collect(),
                }),
            }))
        }

        pub fn with(state: Option<Mixer>) -> FakeMixer {
            FakeMixer {
                state: Mutex::new(state),
                notify: Mutex::new(None),
                writes: AtomicUsize::new(0),
            }
        }

        /// Number of `set_*` calls received.
        pub fn writes(&self) -> usize {
            self.writes.load(Ordering::SeqCst)
        }

        /// Change the state "behind the app's back" (knob, other app) and notify.
        pub fn change(&self, f: impl FnOnce(&mut Option<Mixer>)) {
            f(&mut self.state.lock().unwrap());
            if let Some(n) = self.notify.lock().unwrap().as_ref() {
                n();
            }
        }

        fn with_level(
            &self,
            scope: MixerScope,
            f: impl FnOnce(&mut MixerLevel) -> Result<(), MixerError>,
        ) -> Result<(), MixerError> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            let mut guard = self.state.lock().unwrap();
            let m = guard.as_mut().ok_or_else(unsupported)?;
            let l = match scope {
                MixerScope::Playback => m.playback.as_mut(),
                MixerScope::Monitoring => m.monitoring.as_mut(),
                MixerScope::Recording => m.recording.as_mut(),
            };
            f(l.ok_or_else(|| MixerError::Unavailable("no such control".into()))?)
        }
    }

    impl MixerBackend for FakeMixer {
        fn read(&self) -> Option<Mixer> {
            self.state.lock().unwrap().clone()
        }
        fn set_volume(&self, scope: MixerScope, volume: f32) -> Result<(), MixerError> {
            self.with_level(scope, |l| {
                *l = level(volume, l.mute);
                Ok(())
            })
        }
        fn set_mute(&self, scope: MixerScope, mute: bool) -> Result<(), MixerError> {
            self.with_level(scope, |l| match l.mute {
                Some(_) => {
                    l.mute = Some(mute);
                    Ok(())
                }
                None => Err(MixerError::Unavailable("no mute".into())),
            })
        }
        fn set_source(&self, id: u32) -> Result<(), MixerError> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            let mut guard = self.state.lock().unwrap();
            let s = guard
                .as_mut()
                .and_then(|m| m.recording_source.as_mut())
                .ok_or_else(|| MixerError::Unavailable("no source selector".into()))?;
            s.current = id;
            Ok(())
        }
        fn subscribe(&self, notify: Notify) {
            *self.notify.lock().unwrap() = Some(notify);
        }
    }
}
