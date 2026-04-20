//! Hybrid allowlist + blocklist for mic-activity based meeting detection.
//!
//! Decision order per candidate bundle:
//! 1. Blocklist hit → filter out entirely (dictation, memo, self-filter).
//! 2. Allowlist hit → named banner with the app's display name.
//! 3. Otherwise → generic "Meeting detected" banner.

/// Self-filter: Meetily's own bundle. Must never self-detect.
pub const MEETILY_BUNDLE_ID: &str = "com.meetily.ai";

/// Priority-ordered allowlist. Earlier entries win when multiple apps
/// hold the mic simultaneously.
///
/// macOS bundle IDs here; Windows/Linux equivalents resolved in each
/// platform's signal sampler before reaching the matcher.
const KNOWN_MEETING_APPS: &[(&str, &str)] = &[
    ("us.zoom.xos", "Zoom"),
    ("com.microsoft.teams2", "Microsoft Teams"),
    ("com.microsoft.teams", "Microsoft Teams"),
    ("com.cisco.webexmeetingsapp", "Webex"),
    ("com.webex.meetingmanager", "Webex"),
    ("com.apple.FaceTime", "FaceTime"),
    ("com.hnc.Discord", "Discord"),
    ("com.tinyspeck.slackmacgap", "Slack"),
    ("com.google.Chrome", "a browser meeting"),
    ("com.google.Chrome.canary", "a browser meeting"),
    ("com.apple.Safari", "a browser meeting"),
    ("org.mozilla.firefox", "a browser meeting"),
    ("company.thebrowser.Browser", "a browser meeting"),
    ("com.microsoft.edgemac", "a browser meeting"),
    ("com.brave.Browser", "a browser meeting"),
    ("com.operasoftware.Opera", "a browser meeting"),
    ("com.vivaldi.Vivaldi", "a browser meeting"),
];

/// Blocklist: apps that legitimately hold the mic but are not meetings.
/// Includes Meetily itself, dictation tools, screen recorders.
const DEFINITELY_NOT_MEETINGS: &[&str] = &[
    // Self
    MEETILY_BUNDLE_ID,
    // System dictation / voice memos
    "com.apple.VoiceMemos",
    "com.apple.dictation",
    "com.apple.SpeechRecognitionCore",
    "com.apple.siri",
    "com.apple.assistantd",
    // Third-party dictation / transcription
    "will.flow.Wispr",
    "com.aliveseven.superwhisper",
    "com.chenyu.macwhisper",
    "com.flow.wispr",
    // Screen recorders
    "com.obsproject.obs-studio",
    "com.loom.desktop",
    "com.screenflow.ScreenFlow10",
    "com.screenflow.ScreenFlow11",
];

/// True if the bundle should be suppressed entirely (no banner, ever).
pub fn is_blocked(bundle_id: &str) -> bool {
    DEFINITELY_NOT_MEETINGS
        .iter()
        .any(|blocked| blocked.eq_ignore_ascii_case(bundle_id))
}

/// Priority rank (lower is higher priority). Returns `None` for unknown apps.
fn allowlist_rank(bundle_id: &str) -> Option<usize> {
    KNOWN_MEETING_APPS
        .iter()
        .position(|(b, _)| b.eq_ignore_ascii_case(bundle_id))
}

/// True if the bundle is in the curated allowlist of known meeting apps.
/// Detection uses a shorter sustain threshold for these — we're confident
/// it's a meeting app, so the longer flicker guard is unnecessary.
pub fn is_known(bundle_id: &str) -> bool {
    allowlist_rank(bundle_id).is_some()
}

/// Human-friendly name for the banner. Unknown apps get the generic label.
pub fn display_name(bundle_id: &str) -> &'static str {
    KNOWN_MEETING_APPS
        .iter()
        .find(|(b, _)| b.eq_ignore_ascii_case(bundle_id))
        .map(|(_, name)| *name)
        .unwrap_or("a meeting")
}

/// Pick the highest-priority non-blocked bundle from a list of active
/// mic-holders. Known apps beat unknown apps; ties broken by input order.
pub fn pick_best<'a, I>(active: I) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut best: Option<(&str, usize)> = None;
    for bundle in active {
        if is_blocked(bundle) {
            continue;
        }
        let rank = allowlist_rank(bundle).unwrap_or(usize::MAX);
        match best {
            None => best = Some((bundle, rank)),
            Some((_, r)) if rank < r => best = Some((bundle, rank)),
            _ => {}
        }
    }
    best.map(|(b, _)| b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_meetily_itself() {
        assert!(is_blocked("com.meetily.ai"));
    }

    #[test]
    fn blocks_dictation_apps() {
        assert!(is_blocked("com.apple.VoiceMemos"));
        assert!(is_blocked("will.flow.Wispr"));
        assert!(is_blocked("com.aliveseven.superwhisper"));
    }

    #[test]
    fn blocks_are_case_insensitive() {
        assert!(is_blocked("COM.MEETILY.AI"));
        assert!(is_blocked("com.Apple.VoiceMemos"));
    }

    #[test]
    fn does_not_block_meeting_apps() {
        assert!(!is_blocked("us.zoom.xos"));
        assert!(!is_blocked("com.google.Chrome"));
        assert!(!is_blocked("com.unknown.app"));
    }

    #[test]
    fn display_name_known_app() {
        assert_eq!(display_name("us.zoom.xos"), "Zoom");
        assert_eq!(display_name("com.microsoft.teams2"), "Microsoft Teams");
    }

    #[test]
    fn display_name_browsers_generic() {
        assert_eq!(display_name("com.google.Chrome"), "a browser meeting");
        assert_eq!(display_name("com.apple.Safari"), "a browser meeting");
    }

    #[test]
    fn display_name_unknown_app_generic() {
        assert_eq!(display_name("com.unknown.niche-meeting-app"), "a meeting");
    }

    #[test]
    fn pick_best_prefers_zoom_over_chrome() {
        let active = ["com.google.Chrome", "us.zoom.xos"];
        assert_eq!(pick_best(active.iter().copied()), Some("us.zoom.xos"));
    }

    #[test]
    fn pick_best_skips_blocked() {
        let active = ["com.meetily.ai", "us.zoom.xos"];
        assert_eq!(pick_best(active.iter().copied()), Some("us.zoom.xos"));
    }

    #[test]
    fn pick_best_unknown_only() {
        let active = ["com.unknown.app1", "com.unknown.app2"];
        // Unknown apps both rank usize::MAX; first wins.
        assert_eq!(pick_best(active.iter().copied()), Some("com.unknown.app1"));
    }

    #[test]
    fn pick_best_all_blocked_returns_none() {
        let active = ["com.meetily.ai", "com.apple.VoiceMemos"];
        assert_eq!(pick_best(active.iter().copied()), None);
    }

    #[test]
    fn pick_best_empty() {
        let active: Vec<&str> = vec![];
        assert_eq!(pick_best(active.iter().copied()), None);
    }
}
