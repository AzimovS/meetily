//! Meeting auto-detection via mic-activity.
//!
//! Observes which non-Meetily apps hold the microphone and fires
//! `MeetingDetected` / `MeetingEnded` notifications at the standard
//! sustain/end-silence thresholds. See
//! `docs/plans/2026-04-20-feat-detect-meeting-start-and-end-plan.md`.

pub mod commands;
pub mod matcher;
pub mod service;
pub mod signals;
pub mod state;
pub mod types;

pub use service::{spawn, DetectionService};
