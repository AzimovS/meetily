//! macOS mic-activity sampler via CoreAudio.
//!
//! Polls `kAudioDevicePropertyDeviceIsRunningSomewhere` on the default
//! input device as a cheap gate — if no process is running the mic at
//! all, we skip the per-process enumeration. When the gate is hot, we
//! walk the process list via `kAudioHardwarePropertyProcessObjectList`
//! and check `kAudioProcessPropertyIsRunningInput` on each. Bundle IDs
//! come from `kAudioProcessPropertyBundleID` (available macOS 14.2+).
//!
//! Polling at ~1s is fine even when the gate is hot — the per-process
//! enumeration is just reading audio-object properties, no syscalls
//! into real audio hardware. The plan's "event-driven" ideal is a
//! future optimization; polling measured idle CPU is effectively zero.

use anyhow::{anyhow, Result};
use cidre::core_audio as ca;
use log::{debug, trace, warn};

use crate::detection::signals::SignalSampler;
use crate::detection::types::MicSnapshot;

pub struct MacMicActivitySampler;

impl MacMicActivitySampler {
    pub fn new() -> Result<Self> {
        // Probe on construction so we fail fast if CoreAudio is unhappy
        // (e.g. headless CI, broken HAL).
        ca::System::default_input_device()
            .map_err(|e| anyhow!("Failed to get default input device: {:?}", e))?;
        Ok(Self)
    }

    fn snapshot_inner(&self) -> Result<MicSnapshot> {
        let device = ca::System::default_input_device()
            .map_err(|e| anyhow!("Failed to get default input device: {:?}", e))?;

        let is_running_somewhere = device
            .bool_prop(&ca::PropSelector::DEVICE_IS_RUNNING_SOMEWHERE.global_addr())
            .unwrap_or_else(|e| {
                // Property-read errors here are unusual — degrade to
                // "idle" but leave a breadcrumb so a wedged audio
                // subsystem doesn't fail silently forever.
                trace!("DeviceIsRunningSomewhere read failed: {:?}", e);
                false
            });

        if !is_running_somewhere {
            return Ok(MicSnapshot::default());
        }

        let processes = ca::System::processes()
            .map_err(|e| anyhow!("Failed to list audio processes: {:?}", e))?;

        let mut active = Vec::new();
        for proc in processes.iter() {
            let is_input = match proc.is_running_input() {
                Ok(v) => v,
                Err(e) => {
                    // Per-process input property isn't queryable on every
                    // process (or every macOS version). Skip quietly.
                    trace!("is_running_input unavailable for proc: {:?}", e);
                    continue;
                }
            };
            if !is_input {
                continue;
            }
            match proc.bundle_id() {
                Ok(bid) => {
                    let s = bid.to_string();
                    if !s.is_empty() {
                        active.push(s);
                    }
                }
                Err(e) => {
                    // Command-line tools and some helper processes lack a
                    // bundle ID; nothing to match against, skip.
                    trace!("bundle_id unavailable for proc: {:?}", e);
                }
            }
        }

        debug!("mic snapshot: {} active bundle(s) {:?}", active.len(), active);
        Ok(MicSnapshot {
            active_bundles: active,
        })
    }
}

impl SignalSampler for MacMicActivitySampler {
    fn snapshot(&self) -> Result<MicSnapshot> {
        match self.snapshot_inner() {
            Ok(s) => Ok(s),
            Err(e) => {
                warn!("mic-activity snapshot failed: {:?}", e);
                Ok(MicSnapshot::default())
            }
        }
    }
}
