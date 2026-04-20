//! Signal samplers: platform-specific code that reports which bundles
//! currently hold the microphone.

use anyhow::Result;

use crate::detection::types::MicSnapshot;

pub mod mic_activity;

/// A source of `MicSnapshot` samples. Implementations are polled by the
/// detection service on a fixed cadence.
pub trait SignalSampler: Send + Sync {
    fn snapshot(&self) -> Result<MicSnapshot>;
}
