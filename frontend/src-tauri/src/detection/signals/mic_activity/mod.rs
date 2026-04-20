//! Mic-activity signal sampler: reports which bundle IDs currently hold
//! the microphone.
//!
//! Platform implementations:
//! - **macOS**: CoreAudio (`kAudioDevicePropertyDeviceIsRunningSomewhere` +
//!   per-process `kAudioProcessPropertyIsRunningInput`, macOS 14.2+).
//! - **Windows**: WASAPI `IAudioSessionManager2` session enumeration.
//! - **Linux**: PulseAudio introspection via `libpulse-binding` with a
//!   threaded mainloop bridge.
//! - **Other targets** (FreeBSD, etc.): stub sampler returns empty
//!   snapshots and detection is a no-op.
//!
//! If the platform sampler fails to construct (headless CI, audio
//! subsystem unavailable, PulseAudio daemon missing), `create()` falls
//! back to the stub sampler so the service loop keeps running idly
//! rather than crashing the app.

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(target_os = "linux")]
pub mod linux;

pub mod stub;

use anyhow::Result;
use log::warn;

use crate::detection::signals::SignalSampler;

/// Build the best mic-activity sampler available on the current
/// platform. Never returns `Err` today — platform failures degrade to
/// the stub sampler so the detection service keeps running.
pub fn create() -> Result<Box<dyn SignalSampler>> {
    #[cfg(target_os = "macos")]
    {
        match macos::MacMicActivitySampler::new() {
            Ok(s) => return Ok(Box::new(s)),
            Err(e) => {
                warn!(
                    "macOS mic-activity sampler init failed, detection disabled: {}",
                    e
                );
                return Ok(Box::new(stub::StubMicActivitySampler));
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        match windows::WindowsMicActivitySampler::new() {
            Ok(s) => return Ok(Box::new(s)),
            Err(e) => {
                warn!(
                    "Windows mic-activity sampler init failed, detection disabled: {}",
                    e
                );
                return Ok(Box::new(stub::StubMicActivitySampler));
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        match linux::LinuxMicActivitySampler::new() {
            Ok(s) => return Ok(Box::new(s)),
            Err(e) => {
                warn!(
                    "Linux mic-activity sampler init failed, detection disabled: {}",
                    e
                );
                return Ok(Box::new(stub::StubMicActivitySampler));
            }
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Ok(Box::new(stub::StubMicActivitySampler))
    }
}
