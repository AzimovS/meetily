//! Mic-activity signal sampler: reports which bundle IDs currently hold
//! the microphone.
//!
//! Platform implementations:
//! - macOS: CoreAudio (`kAudioDevicePropertyDeviceIsRunningSomewhere` +
//!   per-process `kAudioProcessPropertyIsRunningInput`, macOS 14.2+).
//! - Windows, Linux: not yet implemented (Phase 2). The stub always
//!   returns an empty snapshot so detection is a no-op on those
//!   platforms until the signal is built.

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(not(target_os = "macos"))]
pub mod stub;

use anyhow::Result;

use crate::detection::signals::SignalSampler;

/// Build the best mic-activity sampler available on the current platform.
pub fn create() -> Result<Box<dyn SignalSampler>> {
    #[cfg(target_os = "macos")]
    {
        let sampler = macos::MacMicActivitySampler::new()?;
        Ok(Box::new(sampler))
    }

    #[cfg(not(target_os = "macos"))]
    {
        let sampler = stub::StubMicActivitySampler;
        Ok(Box::new(sampler))
    }
}
