//! Hybrid allowlist + blocklist for mic-activity based meeting detection.
//!
//! Decision order per candidate bundle:
//! 1. Blocklist hit → filter out entirely (dictation, memo, self-filter).
//! 2. Allowlist hit → named banner with the app's display name.
//! 3. Otherwise → generic "Meeting detected" banner.
//!
//! The "bundle" string the matcher looks up is platform-defined:
//! - macOS: bundle identifier (`us.zoom.xos`)
//! - Windows: executable basename (`Zoom.exe`)
//! - Linux: `application.process.binary` from PulseAudio (`zoom`)
//!
//! String comparison is case-insensitive on macOS/Windows (filesystem
//! semantics) and case-sensitive on Linux (process-name semantics).

/// A canonical "which meeting app is this?" identity. Platform-specific
/// aliases (exe names, bundle IDs, binary names) map into one of these.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum App {
    Zoom,
    Teams,
    Webex,
    FaceTime,
    Discord,
    Slack,
    /// Any browser — we can't tell which tab is open without reading URLs,
    /// and the whole point of Phase 1 was to avoid that. Shows as a
    /// generic "a browser meeting" banner.
    Browser,
}

impl App {
    pub const fn display_name(self) -> &'static str {
        match self {
            App::Zoom => "Zoom",
            App::Teams => "Microsoft Teams",
            App::Webex => "Webex",
            App::FaceTime => "FaceTime",
            App::Discord => "Discord",
            App::Slack => "Slack",
            App::Browser => "a browser meeting",
        }
    }

    /// Lower rank = higher priority. Used when multiple apps hold the
    /// mic simultaneously to pick the most meeting-ish candidate.
    const fn priority(self) -> u8 {
        match self {
            App::Zoom => 0,
            App::Teams => 1,
            App::Webex => 2,
            App::FaceTime => 3,
            App::Discord => 4,
            App::Slack => 5,
            App::Browser => 6,
        }
    }
}

// ---- Platform-specific alias tables ----------------------------------------
//
// Only the current target's table is compiled in. Each entry maps a
// platform-specific identifier to the canonical `App`.

#[cfg(target_os = "macos")]
const ALIASES: &[(&str, App)] = &[
    ("us.zoom.xos", App::Zoom),
    ("com.microsoft.teams2", App::Teams),
    ("com.microsoft.teams", App::Teams),
    ("com.cisco.webexmeetingsapp", App::Webex),
    ("com.webex.meetingmanager", App::Webex),
    ("com.apple.FaceTime", App::FaceTime),
    ("com.hnc.Discord", App::Discord),
    ("com.tinyspeck.slackmacgap", App::Slack),
    ("com.google.Chrome", App::Browser),
    ("com.google.Chrome.canary", App::Browser),
    ("com.apple.Safari", App::Browser),
    ("org.mozilla.firefox", App::Browser),
    ("company.thebrowser.Browser", App::Browser),
    ("com.microsoft.edgemac", App::Browser),
    ("com.brave.Browser", App::Browser),
    ("com.operasoftware.Opera", App::Browser),
    ("com.vivaldi.Vivaldi", App::Browser),
];

#[cfg(target_os = "windows")]
const ALIASES: &[(&str, App)] = &[
    ("Zoom.exe", App::Zoom),
    ("ZoomHost.exe", App::Zoom),
    ("CptHost.exe", App::Zoom), // Zoom's companion capture host process
    ("ms-teams.exe", App::Teams),
    ("Teams.exe", App::Teams),
    ("MSTeams.exe", App::Teams),
    ("webex.exe", App::Webex),
    ("WebexHost.exe", App::Webex),
    ("Cisco WebEx Start.exe", App::Webex),
    ("Discord.exe", App::Discord),
    ("slack.exe", App::Slack),
    ("chrome.exe", App::Browser),
    ("msedge.exe", App::Browser),
    ("firefox.exe", App::Browser),
    ("brave.exe", App::Browser),
    ("opera.exe", App::Browser),
    ("vivaldi.exe", App::Browser),
    ("arc.exe", App::Browser),
];

#[cfg(target_os = "linux")]
const ALIASES: &[(&str, App)] = &[
    ("zoom", App::Zoom),
    ("teams", App::Teams),
    ("teams-for-linux", App::Teams),
    ("webex", App::Webex),
    ("Discord", App::Discord),
    ("discord", App::Discord),
    ("slack", App::Slack),
    ("google-chrome", App::Browser),
    ("chrome", App::Browser),
    ("chromium", App::Browser),
    ("chromium-browser", App::Browser),
    ("firefox", App::Browser),
    ("firefox-esr", App::Browser),
    ("brave", App::Browser),
    ("brave-browser", App::Browser),
    ("vivaldi-bin", App::Browser),
    ("vivaldi-stable", App::Browser),
    ("opera", App::Browser),
];

// Minor-target fallback: empty aliases, matcher treats every candidate
// as unknown. The sampler itself is a stub on these platforms too.
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
const ALIASES: &[(&str, App)] = &[];

// ---- Platform-specific blocklists -----------------------------------------

#[cfg(target_os = "macos")]
const DEFINITELY_NOT_MEETINGS: &[&str] = &[
    // Self. Must stay in sync with `identifier` in tauri.conf.json.
    // If you fork and change the bundle ID, add your variant here.
    "com.meetily.ai",
    "com.meetily.ai.dev",
    "com.meetily.ai.debug",
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

#[cfg(target_os = "windows")]
const DEFINITELY_NOT_MEETINGS: &[&str] = &[
    // Self (tauri dev / prod builds)
    "meetily.exe",
    "Meetily.exe",
    // Bundled sidecars — any executable we ship that might open audio.
    // If the set in `tauri.conf.json` externalBin changes, update here.
    "llama-helper.exe",
    "ffmpeg.exe",
    // System / built-in
    "WindowsVoiceRecorder.exe",
    "VoiceRecorder.exe",
    "SoundRecorder.exe",
    "Cortana.exe",
    // Third-party dictation / transcription
    "Dragon.exe",
    "Wispr.exe",
    "Flow.exe",
    // Screen recorders
    "obs64.exe",
    "obs32.exe",
    "Loom.exe",
    "Camtasia.exe",
    "Camtasia.Studio.exe",
    "Bandicam.exe",
    "Fraps.exe",
];

#[cfg(target_os = "linux")]
const DEFINITELY_NOT_MEETINGS: &[&str] = &[
    // Self
    "meetily",
    // Bundled sidecars
    "llama-helper",
    "ffmpeg",
    // Audio infra (should never register as mic consumers, but defensive)
    "pulseaudio",
    "pipewire",
    "wireplumber",
    "speech-dispatcher",
    "ibus-daemon",
    // Screen recorders
    "obs",
    "SimpleScreenRecorder",
    "simplescreenrecorder",
    "kooha",
    "wf-recorder",
    "gpu-screen-recorder",
    // Dictation / voice input
    "nerd-dictation",
    "dragon",
];

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
const DEFINITELY_NOT_MEETINGS: &[&str] = &[];

// ---- Matching helpers ------------------------------------------------------

/// On macOS and Windows, filesystem/bundle IDs are case-insensitive.
/// On Linux, process names are case-sensitive.
fn keys_match(a: &str, b: &str) -> bool {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        a.eq_ignore_ascii_case(b)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        a == b
    }
}

fn lookup(bundle_id: &str) -> Option<App> {
    ALIASES
        .iter()
        .find(|(alias, _)| keys_match(alias, bundle_id))
        .map(|(_, app)| *app)
}

/// True if the bundle should be suppressed entirely (no banner, ever).
pub fn is_blocked(bundle_id: &str) -> bool {
    DEFINITELY_NOT_MEETINGS
        .iter()
        .any(|blocked| keys_match(blocked, bundle_id))
}

/// True if the bundle is in the curated allowlist of known meeting apps.
/// Detection uses a shorter sustain threshold for these — we're confident
/// it's a meeting app, so the longer flicker guard is unnecessary.
pub fn is_known(bundle_id: &str) -> bool {
    lookup(bundle_id).is_some()
}

/// Priority rank. Known apps return their `App::priority()` (0..=6),
/// unknown apps return `u16::MAX`. Lower rank = higher priority.
pub fn priority_of(bundle_id: &str) -> u16 {
    lookup(bundle_id)
        .map(|app| app.priority() as u16)
        .unwrap_or(u16::MAX)
}

/// Human-friendly name for the banner. Unknown apps get the generic label.
pub fn display_name(bundle_id: &str) -> &'static str {
    lookup(bundle_id)
        .map(App::display_name)
        .unwrap_or("a meeting")
}

/// Pick the highest-priority non-blocked bundle from a list of active
/// mic-holders. Known apps beat unknown apps; ties broken by input order.
pub fn pick_best<'a, I>(active: I) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut best: Option<(&str, u16)> = None;
    for bundle in active {
        if is_blocked(bundle) {
            continue;
        }
        // Known apps rank by `App::priority()` (0..=6); unknown apps
        // share the lowest rank so the first-observed one wins.
        let rank = lookup(bundle)
            .map(|app| app.priority() as u16)
            .unwrap_or(u16::MAX);
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

    // ---- Shared tests (platform-agnostic semantics) -----------------------

    #[test]
    fn display_name_unknown_app_generic() {
        assert_eq!(display_name("com.unknown.niche-meeting-app"), "a meeting");
    }

    #[test]
    fn pick_best_empty() {
        let active: Vec<&str> = vec![];
        assert_eq!(pick_best(active.iter().copied()), None);
    }

    #[test]
    fn app_display_names_are_stable() {
        assert_eq!(App::Zoom.display_name(), "Zoom");
        assert_eq!(App::Teams.display_name(), "Microsoft Teams");
        assert_eq!(App::Browser.display_name(), "a browser meeting");
    }

    #[test]
    fn app_priority_ordering() {
        assert!(App::Zoom.priority() < App::Teams.priority());
        assert!(App::Teams.priority() < App::Browser.priority());
    }

    // ---- macOS-specific tests ---------------------------------------------

    #[cfg(target_os = "macos")]
    mod macos {
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
            assert_eq!(pick_best(active.iter().copied()), Some("com.unknown.app1"));
        }

        #[test]
        fn pick_best_all_blocked_returns_none() {
            let active = ["com.meetily.ai", "com.apple.VoiceMemos"];
            assert_eq!(pick_best(active.iter().copied()), None);
        }

        #[test]
        fn is_known_recognises_known_apps() {
            assert!(is_known("us.zoom.xos"));
            assert!(is_known("com.google.Chrome"));
            assert!(!is_known("com.unknown.app"));
        }
    }

    // ---- Windows-specific tests -------------------------------------------

    #[cfg(target_os = "windows")]
    mod windows {
        use super::*;

        #[test]
        fn resolves_zoom_exe() {
            assert_eq!(display_name("Zoom.exe"), "Zoom");
            assert!(is_known("Zoom.exe"));
        }

        #[test]
        fn resolves_teams_new_and_legacy() {
            assert_eq!(display_name("ms-teams.exe"), "Microsoft Teams");
            assert_eq!(display_name("Teams.exe"), "Microsoft Teams");
            assert_eq!(display_name("MSTeams.exe"), "Microsoft Teams");
        }

        #[test]
        fn resolves_browsers_generically() {
            assert_eq!(display_name("chrome.exe"), "a browser meeting");
            assert_eq!(display_name("msedge.exe"), "a browser meeting");
            assert_eq!(display_name("firefox.exe"), "a browser meeting");
        }

        #[test]
        fn case_insensitive_match() {
            assert_eq!(display_name("ZOOM.EXE"), "Zoom");
            assert!(is_known("zoom.exe"));
        }

        #[test]
        fn blocks_voice_recorder() {
            assert!(is_blocked("WindowsVoiceRecorder.exe"));
            assert!(is_blocked("VoiceRecorder.exe"));
        }

        #[test]
        fn blocks_self() {
            assert!(is_blocked("meetily.exe"));
            assert!(is_blocked("Meetily.exe"));
        }

        #[test]
        fn blocks_obs() {
            assert!(is_blocked("obs64.exe"));
            assert!(is_blocked("obs32.exe"));
        }

        #[test]
        fn does_not_block_meeting_exes() {
            assert!(!is_blocked("Zoom.exe"));
            assert!(!is_blocked("chrome.exe"));
        }

        #[test]
        fn pick_best_prefers_zoom() {
            let active = ["chrome.exe", "Zoom.exe"];
            assert_eq!(pick_best(active.iter().copied()), Some("Zoom.exe"));
        }

        #[test]
        fn unknown_windows_app_generic() {
            assert_eq!(display_name("SomeNicheApp.exe"), "a meeting");
        }
    }

    // ---- Linux-specific tests ---------------------------------------------

    #[cfg(target_os = "linux")]
    mod linux {
        use super::*;

        #[test]
        fn resolves_zoom_binary() {
            assert_eq!(display_name("zoom"), "Zoom");
            assert!(is_known("zoom"));
        }

        #[test]
        fn resolves_teams_variants() {
            assert_eq!(display_name("teams"), "Microsoft Teams");
            assert_eq!(display_name("teams-for-linux"), "Microsoft Teams");
        }

        #[test]
        fn resolves_browsers_generically() {
            assert_eq!(display_name("firefox"), "a browser meeting");
            assert_eq!(display_name("chromium-browser"), "a browser meeting");
            assert_eq!(display_name("google-chrome"), "a browser meeting");
            assert_eq!(display_name("brave-browser"), "a browser meeting");
        }

        #[test]
        fn case_sensitive_match() {
            // Linux: process names are case-sensitive; "Zoom" (uppercase Z)
            // isn't the real binary name, so it doesn't match.
            assert_eq!(display_name("Zoom"), "a meeting");
            assert!(!is_known("Zoom"));
            assert!(is_known("zoom"));
        }

        #[test]
        fn discord_both_cases_registered() {
            // Discord registers as both "Discord" (AppImage) and "discord"
            // (distro packages); both are in the alias table.
            assert!(is_known("Discord"));
            assert!(is_known("discord"));
        }

        #[test]
        fn blocks_self() {
            assert!(is_blocked("meetily"));
        }

        #[test]
        fn blocks_screen_recorders() {
            assert!(is_blocked("obs"));
            assert!(is_blocked("SimpleScreenRecorder"));
        }

        #[test]
        fn does_not_block_meeting_binaries() {
            assert!(!is_blocked("zoom"));
            assert!(!is_blocked("firefox"));
        }

        #[test]
        fn pick_best_prefers_zoom() {
            let active = ["firefox", "zoom"];
            assert_eq!(pick_best(active.iter().copied()), Some("zoom"));
        }
    }
}
