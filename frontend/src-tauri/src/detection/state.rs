//! Meeting-detection state machine.
//!
//! Pure logic — takes a snapshot of which bundles currently hold the mic
//! plus the current time, returns any events that should fire. All timing
//! is parameterized via `Instant`, so tests can simulate minutes of elapsed
//! time in microseconds.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use log::{debug, info};
use serde::Serialize;

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
    /// Time of continuous mic silence before firing MeetingEnded.
    pub end_silence: Duration,
    /// Time we require continuous mic activity after an `Ending` flicker
    /// to confirm the meeting is actually still going. Absorbs transient
    /// hand-off / mute / device-re-select blips.
    pub ending_reacquire_confirm: Duration,
    /// Cooldown after an explicit user dismissal (tap "ignore this app"
    /// on the banner — wired via the Tauri command surface).
    pub dismissal_cooldown: Duration,
}

impl DetectorConfig {
    pub const DEFAULT: Self = Self {
        known_sustain_duration: Duration::from_secs(10),
        unknown_sustain_duration: Duration::from_secs(30),
        end_silence: Duration::from_secs(30),
        ending_reacquire_confirm: Duration::from_secs(3),
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
    /// A previously-detected bundle released the mic; counting down to
    /// end-silence. `reacquire_since` tracks whether we've since seen
    /// the bundle come back — used to debounce flicker.
    Ending {
        bundle: String,
        silence_since: Instant,
        reacquire_since: Option<Instant>,
    },
}

/// Public snapshot of the detector's current phase. Returned by
/// `phase_snapshot()` for Tauri command consumers / UI / agents.
#[derive(Debug, Clone, Serialize)]
pub struct DetectorPhaseSnapshot {
    /// One of `"idle" | "sustaining" | "detected" | "ending"`.
    pub phase: &'static str,
    pub display_name: Option<String>,
    pub elapsed_ms: Option<u64>,
    pub remaining_ms: Option<u64>,
    pub is_recording: bool,
}

pub struct DetectorState {
    phase: Phase,
    /// Tracks bundles the user has explicitly dismissed. Reaped on a
    /// cadence (see `REAP_EVERY_N_ADVANCES`).
    dismissed: HashMap<String, Instant>,
    /// Increments each `advance()`. Triggers periodic dismissal reap.
    advance_count: u32,
    /// Pushed in from the audio layer via `set_recording()`. Gates
    /// whether MeetingEnded fires.
    is_recording: bool,
    config: DetectorConfig,
}

/// Reap expired dismissal entries every N advances. At 1s cadence
/// that's once per minute; cheap.
const REAP_EVERY_N_ADVANCES: u32 = 60;

impl DetectorState {
    pub fn new(config: DetectorConfig) -> Self {
        Self {
            phase: Phase::Idle,
            dismissed: HashMap::new(),
            advance_count: 0,
            is_recording: false,
            config,
        }
    }

    /// Update the recording flag. Called from the audio layer when
    /// recording starts/stops.
    pub fn set_recording(&mut self, recording: bool) {
        self.is_recording = recording;
    }

    /// Advance the state machine with a new snapshot. Returns an event
    /// if a detected/ended transition just fired.
    pub fn advance(&mut self, now: Instant, snapshot: &MicSnapshot) -> Option<DetectionEvent> {
        self.advance_count = self.advance_count.wrapping_add(1);
        if self.advance_count % REAP_EVERY_N_ADVANCES == 0 {
            self.reap_dismissed(now);
        }

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
                    info!("detection: Idle → Sustaining");
                    debug!("detection: Idle → Sustaining({})", bundle);
                    self.phase = Phase::Sustaining {
                        bundle,
                        since: now,
                    };
                }
                None
            }
            Phase::Sustaining { bundle, since } => {
                // Upgrade to a strictly-higher-priority candidate if
                // one has appeared since we entered Sustaining.
                if let Some(next) = candidate.as_deref() {
                    if next != bundle
                        && matcher::priority_of(next) < matcher::priority_of(&bundle)
                    {
                        info!("detection: Sustaining → Sustaining (priority upgrade)");
                        debug!("detection: Sustaining({}) → Sustaining({}) (upgraded)", bundle, next);
                        self.phase = Phase::Sustaining {
                            bundle: next.to_string(),
                            since: now,
                        };
                        return None;
                    }
                }
                if !snapshot.contains(&bundle) {
                    // Lost the sustain candidate before threshold — restart or go idle.
                    if let Some(next) = candidate {
                        info!("detection: Sustaining → Sustaining (switched)");
                        debug!("detection: Sustaining({}) → Sustaining({}) (switched)", bundle, next);
                        self.phase = Phase::Sustaining {
                            bundle: next,
                            since: now,
                        };
                    } else {
                        info!("detection: Sustaining → Idle (flicker, under threshold)");
                        debug!("detection: Sustaining({}) dropped", bundle);
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
                    let display_name = matcher::display_name(&bundle).to_string();
                    info!(
                        "detection: Sustaining → Detected ({:?} threshold); firing MeetingDetected {}",
                        threshold, display_name
                    );
                    debug!("detection: Detected bundle={}", bundle);
                    self.phase = Phase::Detected {
                        bundle: bundle.clone(),
                    };
                    return Some(DetectionEvent::MeetingDetected(DetectedMeeting {
                        display_name,
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
                info!("detection: Detected → Ending (mic released)");
                debug!("detection: Detected({}) → Ending", bundle);
                self.phase = Phase::Ending {
                    bundle,
                    silence_since: now,
                    reacquire_since: None,
                };
                None
            }
            Phase::Ending {
                bundle,
                silence_since,
                reacquire_since,
            } => {
                if snapshot.contains(&bundle) {
                    // Bundle came back. Start — or continue — tracking
                    // a sustained reacquire. Only flip back to Detected
                    // after it's held the mic continuously for
                    // `ending_reacquire_confirm` (absorbs sub-Ns flicker).
                    let reacquire_since = reacquire_since.unwrap_or(now);
                    if now.saturating_duration_since(reacquire_since)
                        >= self.config.ending_reacquire_confirm
                    {
                        info!("detection: Ending → Detected (mic reacquired, sustained)");
                        debug!("detection: Ending({}) → Detected", bundle);
                        self.phase = Phase::Detected { bundle };
                    } else {
                        debug!(
                            "detection: Ending reacquire-debouncing ({}ms / {}ms)",
                            now.saturating_duration_since(reacquire_since).as_millis(),
                            self.config.ending_reacquire_confirm.as_millis()
                        );
                        self.phase = Phase::Ending {
                            bundle,
                            silence_since,
                            reacquire_since: Some(reacquire_since),
                        };
                    }
                    return None;
                }
                // Still silent. If we'd been tracking a reacquire, drop
                // it; the silence_since clock continues.
                if reacquire_since.is_some() {
                    self.phase = Phase::Ending {
                        bundle: bundle.clone(),
                        silence_since,
                        reacquire_since: None,
                    };
                }
                if now.saturating_duration_since(silence_since) < self.config.end_silence {
                    return None;
                }
                // Sustained silence: fire end (if recording) and go idle.
                // We do NOT auto-dismiss the bundle here — a new meeting in
                // the same app after 30s of silence is a genuinely new
                // session, and should re-detect normally. The dismissal
                // cooldown only fires on explicit user action.
                let display_name = matcher::display_name(&bundle).to_string();
                let meeting = DetectedMeeting {
                    display_name: display_name.clone(),
                    bundle_id: bundle.clone(),
                };
                self.phase = Phase::Idle;

                if self.is_recording {
                    info!("detection: Ending → Idle; firing MeetingEnded {}", display_name);
                    debug!("detection: Ending({}) → Idle (fire end)", bundle);
                    Some(DetectionEvent::MeetingEnded(meeting))
                } else {
                    info!("detection: Ending → Idle; not recording, suppressing end-banner");
                    debug!("detection: Ending({}) → Idle (suppressed)", bundle);
                    None
                }
            }
        }
    }

    /// Suppress further detected-events for this bundle for the cooldown window.
    pub fn dismiss(&mut self, bundle_id: &str, now: Instant) {
        let until = now + self.config.dismissal_cooldown;
        self.dismissed.insert(bundle_id.to_string(), until);
        debug!("detection: dismissed bundle for {:?}", self.config.dismissal_cooldown);
    }

    fn is_dismissed(&self, bundle_id: &str, now: Instant) -> bool {
        self.dismissed
            .get(bundle_id)
            .map(|until| *until > now)
            .unwrap_or(false)
    }

    fn reap_dismissed(&mut self, now: Instant) {
        let before = self.dismissed.len();
        self.dismissed.retain(|_, until| *until > now);
        let after = self.dismissed.len();
        if before != after {
            debug!("detection: reaped {} expired dismissals ({} remain)", before - after, after);
        }
    }

    /// Produce a serializable snapshot of current phase + timing.
    pub fn phase_snapshot(&self, now: Instant) -> DetectorPhaseSnapshot {
        match &self.phase {
            Phase::Idle => DetectorPhaseSnapshot {
                phase: "idle",
                display_name: None,
                elapsed_ms: None,
                remaining_ms: None,
                is_recording: self.is_recording,
            },
            Phase::Sustaining { bundle, since } => {
                let threshold = if matcher::is_known(bundle) {
                    self.config.known_sustain_duration
                } else {
                    self.config.unknown_sustain_duration
                };
                let elapsed = now.saturating_duration_since(*since);
                DetectorPhaseSnapshot {
                    phase: "sustaining",
                    display_name: Some(matcher::display_name(bundle).to_string()),
                    elapsed_ms: Some(elapsed.as_millis() as u64),
                    remaining_ms: Some(threshold.saturating_sub(elapsed).as_millis() as u64),
                    is_recording: self.is_recording,
                }
            }
            Phase::Detected { bundle } => DetectorPhaseSnapshot {
                phase: "detected",
                display_name: Some(matcher::display_name(bundle).to_string()),
                elapsed_ms: None,
                remaining_ms: None,
                is_recording: self.is_recording,
            },
            Phase::Ending {
                bundle,
                silence_since,
                ..
            } => {
                let elapsed = now.saturating_duration_since(*silence_since);
                DetectorPhaseSnapshot {
                    phase: "ending",
                    display_name: Some(matcher::display_name(bundle).to_string()),
                    elapsed_ms: Some(elapsed.as_millis() as u64),
                    remaining_ms: Some(
                        self.config.end_silence.saturating_sub(elapsed).as_millis() as u64,
                    ),
                    is_recording: self.is_recording,
                }
            }
        }
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
        DetectorConfig::DEFAULT
    }

    #[test]
    fn idle_stays_idle_with_no_active_bundles() {
        let mut s = DetectorState::new(test_config());
        let t0 = Instant::now();
        assert_eq!(s.advance(t0, &snapshot(&[])), None);
        assert!(s.is_idle());
    }

    #[test]
    fn idle_ignores_blocklisted_bundles() {
        let mut s = DetectorState::new(test_config());
        let t0 = Instant::now();
        assert_eq!(s.advance(t0, &snapshot(&["com.meetily.ai"])), None);
        assert!(s.is_idle());
    }

    #[test]
    fn known_app_detects_after_short_threshold() {
        let mut s = DetectorState::new(test_config());
        let t0 = Instant::now();

        assert_eq!(s.advance(t0, &snapshot(&["us.zoom.xos"])), None);
        assert!(!s.is_idle());

        let t1 = t0 + Duration::from_secs(9);
        assert_eq!(s.advance(t1, &snapshot(&["us.zoom.xos"])), None);

        let t2 = t0 + Duration::from_secs(10);
        let ev = s.advance(t2, &snapshot(&["us.zoom.xos"]));
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

        s.advance(t0, &snapshot(&["com.niche.meetingtool"]));

        let t1 = t0 + Duration::from_secs(20);
        assert_eq!(s.advance(t1, &snapshot(&["com.niche.meetingtool"])), None);

        let t2 = t0 + Duration::from_secs(29);
        assert_eq!(s.advance(t2, &snapshot(&["com.niche.meetingtool"])), None);

        let t3 = t0 + Duration::from_secs(30);
        let ev = s.advance(t3, &snapshot(&["com.niche.meetingtool"]));
        assert!(matches!(
            ev,
            Some(DetectionEvent::MeetingDetected(ref m)) if m.display_name == "a meeting"
        ));
    }

    #[test]
    fn flicker_under_sustain_drops_to_idle() {
        let mut s = DetectorState::new(test_config());
        let t0 = Instant::now();

        s.advance(t0, &snapshot(&["us.zoom.xos"]));
        let t1 = t0 + Duration::from_secs(5);
        assert_eq!(s.advance(t1, &snapshot(&[])), None);
        assert!(s.is_idle());
    }

    #[test]
    fn unknown_app_fires_generic_banner() {
        let mut s = DetectorState::new(test_config());
        let t0 = Instant::now();

        s.advance(t0, &snapshot(&["com.niche.meetingtool"]));
        let t1 = t0 + Duration::from_secs(30);
        let ev = s.advance(t1, &snapshot(&["com.niche.meetingtool"]));
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
        s.set_recording(true);
        let t0 = Instant::now();

        s.advance(t0, &snapshot(&["us.zoom.xos"]));
        s.advance(t0 + Duration::from_secs(30), &snapshot(&["us.zoom.xos"]));
        assert!(s.is_detected());

        let t_released = t0 + Duration::from_secs(60);
        assert_eq!(s.advance(t_released, &snapshot(&[])), None);

        assert_eq!(
            s.advance(t_released + Duration::from_secs(29), &snapshot(&[])),
            None
        );

        let ev = s.advance(t_released + Duration::from_secs(30), &snapshot(&[]));
        assert!(matches!(
            ev,
            Some(DetectionEvent::MeetingEnded(ref m)) if m.bundle_id == "us.zoom.xos"
        ));
        assert!(s.is_idle());
    }

    #[test]
    fn end_does_not_fire_when_not_recording() {
        let mut s = DetectorState::new(test_config());
        // is_recording defaults to false
        let t0 = Instant::now();

        s.advance(t0, &snapshot(&["us.zoom.xos"]));
        s.advance(t0 + Duration::from_secs(30), &snapshot(&["us.zoom.xos"]));

        let t_released = t0 + Duration::from_secs(60);
        s.advance(t_released, &snapshot(&[]));
        let ev = s.advance(t_released + Duration::from_secs(30), &snapshot(&[]));
        assert_eq!(ev, None);
        assert!(s.is_idle());
    }

    #[test]
    fn ending_returns_to_detected_on_sustained_reacquire() {
        let mut s = DetectorState::new(test_config());
        s.set_recording(true);
        let t0 = Instant::now();

        s.advance(t0, &snapshot(&["us.zoom.xos"]));
        s.advance(t0 + Duration::from_secs(30), &snapshot(&["us.zoom.xos"]));

        // Release.
        let t_release = t0 + Duration::from_secs(60);
        s.advance(t_release, &snapshot(&[]));

        // Mic reacquired but only for 2s (under the 3s confirm) — stays Ending.
        let t_reacq = t_release + Duration::from_secs(10);
        s.advance(t_reacq, &snapshot(&["us.zoom.xos"]));
        assert!(!s.is_detected());

        // Still holding mic 3s after first reacquire — promote to Detected.
        let t_confirmed = t_reacq + Duration::from_secs(3);
        assert_eq!(s.advance(t_confirmed, &snapshot(&["us.zoom.xos"])), None);
        assert!(s.is_detected());
    }

    #[test]
    fn sub_3s_flicker_does_not_promote_from_ending() {
        let mut s = DetectorState::new(test_config());
        s.set_recording(true);
        let t0 = Instant::now();

        s.advance(t0, &snapshot(&["us.zoom.xos"]));
        s.advance(t0 + Duration::from_secs(30), &snapshot(&["us.zoom.xos"]));

        let t_release = t0 + Duration::from_secs(60);
        s.advance(t_release, &snapshot(&[]));

        // Mic tick of 1s, then silent again, then another 1s tick later.
        let t_flicker_start = t_release + Duration::from_secs(5);
        s.advance(t_flicker_start, &snapshot(&["us.zoom.xos"]));
        s.advance(t_flicker_start + Duration::from_secs(1), &snapshot(&[]));
        s.advance(t_flicker_start + Duration::from_secs(3), &snapshot(&["us.zoom.xos"]));
        // Short reacquires reset the reacquire clock; state stays Ending.
        assert!(!s.is_detected());
        assert!(!s.is_idle());

        // After another 30s of continuous silence from release, end fires.
        let t_end = t_release + Duration::from_secs(30);
        // Ensure we give silence before the end fires.
        s.advance(t_end - Duration::from_secs(1), &snapshot(&[]));
        let ev = s.advance(t_end, &snapshot(&[]));
        assert!(matches!(ev, Some(DetectionEvent::MeetingEnded(_))));
    }

    #[test]
    fn dismissal_suppresses_redetect_within_cooldown() {
        let mut s = DetectorState::new(test_config());
        let t0 = Instant::now();

        s.dismiss("us.zoom.xos", t0);

        let t_later = t0 + Duration::from_secs(300);
        s.advance(t_later, &snapshot(&["us.zoom.xos"]));
        let t_still_later = t_later + Duration::from_secs(60);
        assert_eq!(
            s.advance(t_still_later, &snapshot(&["us.zoom.xos"])),
            None
        );
        assert!(s.is_idle());
    }

    #[test]
    fn dismissal_expires_after_cooldown() {
        let mut s = DetectorState::new(test_config());
        let t0 = Instant::now();

        s.dismiss("us.zoom.xos", t0);

        let t_later = t0 + Duration::from_secs(660);
        s.advance(t_later, &snapshot(&["us.zoom.xos"]));
        let t_sustained = t_later + Duration::from_secs(30);
        let ev = s.advance(t_sustained, &snapshot(&["us.zoom.xos"]));
        assert!(matches!(ev, Some(DetectionEvent::MeetingDetected(_))));
    }

    #[test]
    fn natural_end_does_not_suppress_next_meeting() {
        let mut s = DetectorState::new(test_config());
        s.set_recording(true);
        let t0 = Instant::now();

        s.advance(t0, &snapshot(&["us.zoom.xos"]));
        s.advance(t0 + Duration::from_secs(30), &snapshot(&["us.zoom.xos"]));
        let t_released = t0 + Duration::from_secs(60);
        s.advance(t_released, &snapshot(&[]));
        let ev_end = s.advance(t_released + Duration::from_secs(30), &snapshot(&[]));
        assert!(matches!(ev_end, Some(DetectionEvent::MeetingEnded(_))));
        assert!(s.is_idle());

        // Not recording for the second session to avoid interference,
        // but that's unrelated — point is re-detect should fire.
        s.set_recording(false);
        let t_new = t_released + Duration::from_secs(120);
        s.advance(t_new, &snapshot(&["us.zoom.xos"]));
        let ev_new = s.advance(t_new + Duration::from_secs(30), &snapshot(&["us.zoom.xos"]));
        assert!(matches!(
            ev_new,
            Some(DetectionEvent::MeetingDetected(ref m)) if m.bundle_id == "us.zoom.xos"
        ));
    }

    #[test]
    fn higher_priority_app_wins_when_both_active() {
        let mut s = DetectorState::new(test_config());
        let t0 = Instant::now();

        s.advance(t0, &snapshot(&["com.google.Chrome", "us.zoom.xos"]));
        let ev = s.advance(
            t0 + Duration::from_secs(30),
            &snapshot(&["com.google.Chrome", "us.zoom.xos"]),
        );
        match ev {
            Some(DetectionEvent::MeetingDetected(m)) => assert_eq!(m.bundle_id, "us.zoom.xos"),
            other => panic!("expected MeetingDetected, got {:?}", other),
        }
    }

    #[test]
    fn sustaining_upgrades_to_higher_priority_late_arrival() {
        let mut s = DetectorState::new(test_config());
        let t0 = Instant::now();

        // Chrome arrives first; sustain clock starts.
        s.advance(t0, &snapshot(&["com.google.Chrome"]));
        // 5s later, Zoom joins — should upgrade and reset the sustain clock.
        let t5 = t0 + Duration::from_secs(5);
        s.advance(t5, &snapshot(&["com.google.Chrome", "us.zoom.xos"]));

        // 10s after upgrade (t5+10 = t0+15): Zoom crosses its 10s known-app
        // threshold. Chrome's clock would have been ignored.
        let t15 = t5 + Duration::from_secs(10);
        let ev = s.advance(t15, &snapshot(&["com.google.Chrome", "us.zoom.xos"]));
        match ev {
            Some(DetectionEvent::MeetingDetected(m)) => assert_eq!(m.bundle_id, "us.zoom.xos"),
            other => panic!("expected MeetingDetected for Zoom, got {:?}", other),
        }
    }

    #[test]
    fn phase_snapshot_idle() {
        let s = DetectorState::new(test_config());
        let snap = s.phase_snapshot(Instant::now());
        assert_eq!(snap.phase, "idle");
        assert!(snap.display_name.is_none());
        assert!(!snap.is_recording);
    }

    #[test]
    fn phase_snapshot_sustaining_reports_remaining() {
        let mut s = DetectorState::new(test_config());
        let t0 = Instant::now();
        s.advance(t0, &snapshot(&["us.zoom.xos"]));
        let snap = s.phase_snapshot(t0 + Duration::from_secs(3));
        assert_eq!(snap.phase, "sustaining");
        assert_eq!(snap.display_name.as_deref(), Some("Zoom"));
        // 10s threshold - 3s elapsed = 7000ms remaining
        assert!(snap.remaining_ms.unwrap() <= 7_000 && snap.remaining_ms.unwrap() > 6_500);
    }

    #[test]
    fn dismissed_reaper_removes_expired_entries() {
        let mut s = DetectorState::new(test_config());
        let t0 = Instant::now();
        s.dismiss("com.example.one", t0);
        s.dismiss("com.example.two", t0);
        assert_eq!(s.dismissed.len(), 2);

        // Jump past cooldown + past the reap cadence to trigger cleanup.
        let t_past = t0 + Duration::from_secs(700);
        for _ in 0..REAP_EVERY_N_ADVANCES {
            s.advance(t_past, &snapshot(&[]));
        }
        assert_eq!(s.dismissed.len(), 0);
    }
}
