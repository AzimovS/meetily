//! Meeting-detection state machine.
//!
//! Pure logic — takes a snapshot of which bundles currently hold the mic
//! plus the current time, returns any events that should fire. All timing
//! is parameterized via `Instant`, so tests can simulate minutes of elapsed
//! time in microseconds.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use log::info;

use crate::detection::matcher;
use crate::detection::types::{DetectedMeeting, DetectionEvent, MicSnapshot};

#[derive(Debug, Clone, Copy)]
pub struct DetectorConfig {
    /// Sustain threshold for bundles in the allowlist (Zoom, Teams,
    /// browsers, etc). High-confidence — fire fast.
    pub known_sustain_duration: Duration,
    /// Sustain threshold for unknown bundles. Conservative flicker
    /// guard so random apps briefly grabbing the mic don't fire a
    /// banner.
    pub unknown_sustain_duration: Duration,
    pub end_silence: Duration,
    pub dismissal_cooldown: Duration,
}

impl DetectorConfig {
    pub const DEFAULT: Self = Self {
        known_sustain_duration: Duration::from_secs(10),
        unknown_sustain_duration: Duration::from_secs(30),
        end_silence: Duration::from_secs(30),
        dismissal_cooldown: Duration::from_secs(600),
    };
}

impl Default for DetectorConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    /// Nothing interesting is happening.
    Idle,
    /// A bundle has started holding the mic but hasn't hit the sustain threshold.
    Sustaining { bundle: String, since: Instant },
    /// A bundle crossed the threshold; notification was fired.
    Detected { bundle: String },
    /// A previously-detected bundle released the mic; counting down to end-silence.
    Ending {
        bundle: String,
        silence_since: Instant,
    },
}

pub struct DetectorState {
    phase: Phase,
    dismissed: HashMap<String, Instant>,
    config: DetectorConfig,
}

impl DetectorState {
    pub fn new(config: DetectorConfig) -> Self {
        Self {
            phase: Phase::Idle,
            dismissed: HashMap::new(),
            config,
        }
    }

    /// Advance the state machine with a new snapshot.
    ///
    /// `is_recording` gates whether a `MeetingEnded` event is emitted — we only
    /// fire end-banners while the user is actually recording.
    pub fn advance(
        &mut self,
        now: Instant,
        snapshot: &MicSnapshot,
        is_recording: bool,
    ) -> Option<DetectionEvent> {
        let candidate = matcher::pick_best(
            snapshot
                .active_bundles
                .iter()
                .filter(|b| !self.is_dismissed(b, now))
                .map(String::as_str),
        )
        .map(|s| s.to_string());

        match self.phase.clone() {
            Phase::Idle => {
                if let Some(bundle) = candidate {
                    info!("detection: Idle → Sustaining({})", bundle);
                    self.phase = Phase::Sustaining {
                        bundle,
                        since: now,
                    };
                }
                None
            }
            Phase::Sustaining { bundle, since } => {
                if !snapshot.contains(&bundle) {
                    // Lost the sustain candidate before threshold — restart or go idle.
                    if let Some(next) = candidate {
                        info!("detection: Sustaining({}) → Sustaining({}) (switched)", bundle, next);
                        self.phase = Phase::Sustaining {
                            bundle: next,
                            since: now,
                        };
                    } else {
                        info!("detection: Sustaining({}) → Idle (flicker, under threshold)", bundle);
                        self.phase = Phase::Idle;
                    }
                    return None;
                }
                let threshold = if matcher::is_known(&bundle) {
                    self.config.known_sustain_duration
                } else {
                    self.config.unknown_sustain_duration
                };
                if now.saturating_duration_since(since) >= threshold {
                    info!(
                        "detection: Sustaining({}) → Detected ({:?} threshold); firing MeetingDetected",
                        bundle, threshold
                    );
                    self.phase = Phase::Detected {
                        bundle: bundle.clone(),
                    };
                    return Some(DetectionEvent::MeetingDetected(DetectedMeeting {
                        display_name: matcher::display_name(&bundle).to_string(),
                        bundle_id: bundle,
                    }));
                }
                None
            }
            Phase::Detected { bundle } => {
                if snapshot.contains(&bundle) {
                    return None;
                }
                // Mic released — enter Ending and start counting.
                info!("detection: Detected({}) → Ending (mic released)", bundle);
                self.phase = Phase::Ending {
                    bundle,
                    silence_since: now,
                };
                None
            }
            Phase::Ending {
                bundle,
                silence_since,
            } => {
                if snapshot.contains(&bundle) {
                    // Flicker: still in the meeting. Back to Detected, no event.
                    info!("detection: Ending({}) → Detected (mic reacquired)", bundle);
                    self.phase = Phase::Detected { bundle };
                    return None;
                }
                if now.saturating_duration_since(silence_since) < self.config.end_silence {
                    return None;
                }
                // Sustained silence: fire end (if recording) and go idle.
                // We do NOT auto-dismiss the bundle here — a new meeting in
                // the same app after 30s of silence is a genuinely new
                // session, and should re-detect normally. The dismissal
                // cooldown only fires on explicit user action (future
                // "ignore this app" button, Phase 4).
                let meeting = DetectedMeeting {
                    display_name: matcher::display_name(&bundle).to_string(),
                    bundle_id: bundle.clone(),
                };
                self.phase = Phase::Idle;

                if is_recording {
                    info!("detection: Ending({}) → Idle; firing MeetingEnded", bundle.clone());
                    Some(DetectionEvent::MeetingEnded(meeting))
                } else {
                    info!(
                        "detection: Ending({}) → Idle; not recording, suppressing end-banner",
                        bundle
                    );
                    None
                }
            }
        }
    }

    /// Suppress further detected-events for this bundle for the cooldown window.
    /// Called externally when the user dismisses the banner or taps through.
    pub fn dismiss(&mut self, bundle_id: &str, now: Instant) {
        let until = now + self.config.dismissal_cooldown;
        self.dismissed.insert(bundle_id.to_string(), until);
    }

    fn is_dismissed(&self, bundle_id: &str, now: Instant) -> bool {
        self.dismissed
            .get(bundle_id)
            .map(|until| *until > now)
            .unwrap_or(false)
    }

    #[cfg(test)]
    fn is_idle(&self) -> bool {
        matches!(self.phase, Phase::Idle)
    }

    #[cfg(test)]
    fn is_detected(&self) -> bool {
        matches!(self.phase, Phase::Detected { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(bundles: &[&str]) -> MicSnapshot {
        MicSnapshot {
            active_bundles: bundles.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn test_config() -> DetectorConfig {
        DetectorConfig {
            known_sustain_duration: Duration::from_secs(10),
            unknown_sustain_duration: Duration::from_secs(30),
            end_silence: Duration::from_secs(30),
            dismissal_cooldown: Duration::from_secs(600),
        }
    }

    #[test]
    fn idle_stays_idle_with_no_active_bundles() {
        let mut s = DetectorState::new(test_config());
        let t0 = Instant::now();
        assert_eq!(s.advance(t0, &snapshot(&[]), false), None);
        assert!(s.is_idle());
    }

    #[test]
    fn idle_ignores_blocklisted_bundles() {
        let mut s = DetectorState::new(test_config());
        let t0 = Instant::now();
        assert_eq!(s.advance(t0, &snapshot(&["com.meetily.ai"]), false), None);
        assert!(s.is_idle());
    }

    #[test]
    fn known_app_detects_after_short_threshold() {
        let mut s = DetectorState::new(test_config());
        let t0 = Instant::now();

        assert_eq!(s.advance(t0, &snapshot(&["us.zoom.xos"]), false), None);
        assert!(!s.is_idle());

        // 9s: still under the 10s known-app threshold.
        let t1 = t0 + Duration::from_secs(9);
        assert_eq!(s.advance(t1, &snapshot(&["us.zoom.xos"]), false), None);

        // 10s: crosses threshold, fires MeetingDetected.
        let t2 = t0 + Duration::from_secs(10);
        let ev = s.advance(t2, &snapshot(&["us.zoom.xos"]), false);
        assert!(matches!(
            ev,
            Some(DetectionEvent::MeetingDetected(ref m)) if m.bundle_id == "us.zoom.xos" && m.display_name == "Zoom"
        ));
        assert!(s.is_detected());
    }

    #[test]
    fn unknown_app_waits_for_long_threshold() {
        let mut s = DetectorState::new(test_config());
        let t0 = Instant::now();

        s.advance(t0, &snapshot(&["com.niche.meetingtool"]), false);

        // 20s: well past known threshold but under unknown threshold — no event.
        let t1 = t0 + Duration::from_secs(20);
        assert_eq!(s.advance(t1, &snapshot(&["com.niche.meetingtool"]), false), None);

        // 29s: still under unknown threshold.
        let t2 = t0 + Duration::from_secs(29);
        assert_eq!(s.advance(t2, &snapshot(&["com.niche.meetingtool"]), false), None);

        // 30s: fires.
        let t3 = t0 + Duration::from_secs(30);
        let ev = s.advance(t3, &snapshot(&["com.niche.meetingtool"]), false);
        assert!(matches!(
            ev,
            Some(DetectionEvent::MeetingDetected(ref m)) if m.display_name == "a meeting"
        ));
    }

    #[test]
    fn flicker_under_sustain_drops_to_idle() {
        let mut s = DetectorState::new(test_config());
        let t0 = Instant::now();

        s.advance(t0, &snapshot(&["us.zoom.xos"]), false);
        // Mic released after 5s, well under sustain threshold.
        let t1 = t0 + Duration::from_secs(5);
        assert_eq!(s.advance(t1, &snapshot(&[]), false), None);
        assert!(s.is_idle());
    }

    #[test]
    fn unknown_app_fires_generic_banner() {
        let mut s = DetectorState::new(test_config());
        let t0 = Instant::now();

        s.advance(t0, &snapshot(&["com.niche.meetingtool"]), false);
        let t1 = t0 + Duration::from_secs(30);
        let ev = s.advance(t1, &snapshot(&["com.niche.meetingtool"]), false);
        match ev {
            Some(DetectionEvent::MeetingDetected(m)) => {
                assert_eq!(m.display_name, "a meeting");
                assert_eq!(m.bundle_id, "com.niche.meetingtool");
            }
            other => panic!("expected MeetingDetected, got {:?}", other),
        }
    }

    #[test]
    fn end_fires_after_silence_when_recording() {
        let mut s = DetectorState::new(test_config());
        let t0 = Instant::now();

        // Detect.
        s.advance(t0, &snapshot(&["us.zoom.xos"]), true);
        s.advance(t0 + Duration::from_secs(30), &snapshot(&["us.zoom.xos"]), true);
        assert!(s.is_detected());

        // Mic released; Ending begins.
        let t_released = t0 + Duration::from_secs(60);
        assert_eq!(s.advance(t_released, &snapshot(&[]), true), None);

        // 29s of silence — no end yet.
        assert_eq!(
            s.advance(t_released + Duration::from_secs(29), &snapshot(&[]), true),
            None
        );

        // 30s of silence — fires MeetingEnded.
        let ev = s.advance(t_released + Duration::from_secs(30), &snapshot(&[]), true);
        assert!(matches!(
            ev,
            Some(DetectionEvent::MeetingEnded(ref m)) if m.bundle_id == "us.zoom.xos"
        ));
        assert!(s.is_idle());
    }

    #[test]
    fn end_does_not_fire_when_not_recording() {
        let mut s = DetectorState::new(test_config());
        let t0 = Instant::now();

        s.advance(t0, &snapshot(&["us.zoom.xos"]), false);
        s.advance(t0 + Duration::from_secs(30), &snapshot(&["us.zoom.xos"]), false);

        let t_released = t0 + Duration::from_secs(60);
        s.advance(t_released, &snapshot(&[]), false);
        let ev = s.advance(t_released + Duration::from_secs(30), &snapshot(&[]), false);
        assert_eq!(ev, None);
        assert!(s.is_idle());
    }

    #[test]
    fn ending_returns_to_detected_on_flicker() {
        let mut s = DetectorState::new(test_config());
        let t0 = Instant::now();

        s.advance(t0, &snapshot(&["us.zoom.xos"]), true);
        s.advance(t0 + Duration::from_secs(30), &snapshot(&["us.zoom.xos"]), true);

        // Release.
        let t_release = t0 + Duration::from_secs(60);
        s.advance(t_release, &snapshot(&[]), true);

        // Back to holding mic after 10s (within end_silence).
        let t_back = t_release + Duration::from_secs(10);
        assert_eq!(s.advance(t_back, &snapshot(&["us.zoom.xos"]), true), None);
        assert!(s.is_detected());
    }

    #[test]
    fn dismissal_suppresses_redetect_within_cooldown() {
        let mut s = DetectorState::new(test_config());
        let t0 = Instant::now();

        // Manually mark as dismissed.
        s.dismiss("us.zoom.xos", t0);

        // 5 minutes later — still within 10-min cooldown.
        let t_later = t0 + Duration::from_secs(300);
        s.advance(t_later, &snapshot(&["us.zoom.xos"]), false);
        let t_still_later = t_later + Duration::from_secs(60);
        assert_eq!(
            s.advance(t_still_later, &snapshot(&["us.zoom.xos"]), false),
            None
        );
        assert!(s.is_idle());
    }

    #[test]
    fn dismissal_expires_after_cooldown() {
        let mut s = DetectorState::new(test_config());
        let t0 = Instant::now();

        s.dismiss("us.zoom.xos", t0);

        // 11 minutes later: cooldown expired.
        let t_later = t0 + Duration::from_secs(660);
        s.advance(t_later, &snapshot(&["us.zoom.xos"]), false);
        let t_sustained = t_later + Duration::from_secs(30);
        let ev = s.advance(t_sustained, &snapshot(&["us.zoom.xos"]), false);
        assert!(matches!(ev, Some(DetectionEvent::MeetingDetected(_))));
    }

    #[test]
    fn natural_end_does_not_suppress_next_meeting() {
        let mut s = DetectorState::new(test_config());
        let t0 = Instant::now();

        // First meeting: detect and end naturally (recording).
        s.advance(t0, &snapshot(&["us.zoom.xos"]), true);
        s.advance(t0 + Duration::from_secs(30), &snapshot(&["us.zoom.xos"]), true);
        let t_released = t0 + Duration::from_secs(60);
        s.advance(t_released, &snapshot(&[]), true);
        let ev_end = s.advance(t_released + Duration::from_secs(30), &snapshot(&[]), true);
        assert!(matches!(ev_end, Some(DetectionEvent::MeetingEnded(_))));
        assert!(s.is_idle());

        // 2 minutes later: same app starts a new call. Should re-detect.
        let t_new = t_released + Duration::from_secs(120);
        s.advance(t_new, &snapshot(&["us.zoom.xos"]), false);
        let ev_new =
            s.advance(t_new + Duration::from_secs(30), &snapshot(&["us.zoom.xos"]), false);
        assert!(matches!(
            ev_new,
            Some(DetectionEvent::MeetingDetected(ref m)) if m.bundle_id == "us.zoom.xos"
        ));
    }

    #[test]
    fn higher_priority_app_wins_when_both_active() {
        let mut s = DetectorState::new(test_config());
        let t0 = Instant::now();

        s.advance(t0, &snapshot(&["com.google.Chrome", "us.zoom.xos"]), false);
        let ev = s.advance(
            t0 + Duration::from_secs(30),
            &snapshot(&["com.google.Chrome", "us.zoom.xos"]),
            false,
        );
        match ev {
            Some(DetectionEvent::MeetingDetected(m)) => assert_eq!(m.bundle_id, "us.zoom.xos"),
            other => panic!("expected MeetingDetected, got {:?}", other),
        }
    }
}
