//! Mic-activity signal sampler: reports which bundle IDs currently hold
//! the microphone.
//!
//! Platform implementations:
//! - **macOS**: CoreAudio (`kAudioDevicePropertyDeviceIsRunningSomewhere` +
//!   per-process `kAudioProcessPropertyIsRunningInput`, macOS 14.2+).
//! - **Everything else**: stub sampler returns empty snapshots and
//!   detection is a no-op. Windows (WASAPI) and Linux (PulseAudio)
//!   samplers are planned as follow-up work — see
//!   `docs/plans/2026-04-20-feat-meeting-detection-phase-2-windows-linux-plan.md`.
//!
//! If the platform sampler fails to construct (headless CI, audio
//! subsystem unavailable), `create()` falls back to the stub sampler so
//! the service loop keeps running idly rather than crashing the app.

#[cfg(target_os = "macos")]
pub mod macos;

pub mod stub;

use anyhow::Result;
#[cfg(target_os = "macos")]
use log::warn;

use crate::detection::signals::SignalSampler;

/// Build the best mic-activity sampler available on the current
/// platform. Never returns `Err` today — platform failures degrade to
/// the stub sampler so the detection service keeps running.
pub fn create() -> Result<Box<dyn SignalSampler>> {
    #[cfg(target_os = "macos")]
    {
        match macos::MacMicActivitySampler::new() {
            Ok(s) => Ok(Box::new(s)),
            Err(e) => {
                warn!(
                    "macOS mic-activity sampler init failed, detection disabled: {}",
                    e
                );
                Ok(Box::new(stub::StubMicActivitySampler))
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        Ok(Box::new(stub::StubMicActivitySampler))
    }
}
