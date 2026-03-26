// audio/meeting_detection/browser_detector.rs
//
// Browser meeting detection via macOS Accessibility API (AXUIElement).
// Reads browser window titles to detect active meetings (Google Meet, Teams web, etc.)
//
// PRIVACY: Only window titles matching meeting patterns are returned.
// Unmatched titles are immediately discarded — never logged, stored, or emitted.

use super::{sanitize_meeting_name, MEETING_TITLE_PATTERNS};

/// Result of checking a browser for active meetings
#[derive(Debug, Clone)]
pub struct BrowserMeetingInfo {
    /// Sanitized meeting name extracted from the window title
    pub meeting_name: String,
    /// Which pattern matched
    pub pattern: String,
}

/// Check if accessibility permission is granted on macOS.
/// Returns true if the app has permission to read window titles.
#[cfg(target_os = "macos")]
pub fn check_accessibility_permission(prompt: bool) -> bool {
    if prompt {
        cidre::ax::is_process_trusted_with_prompt(true)
    } else {
        cidre::ax::is_process_trusted()
    }
}

#[cfg(not(target_os = "macos"))]
pub fn check_accessibility_permission(_prompt: bool) -> bool {
    false
}

/// Check if a browser app (identified by PID) has an active meeting tab.
///
/// Reads ONLY the top-level window titles (active tab per window).
/// This is fast (1-5ms) and sufficient because active meetings are
/// typically the foreground tab.
///
/// PRIVACY: Titles that don't match meeting patterns are immediately
/// dropped. They are never logged, stored, or returned.
#[cfg(target_os = "macos")]
pub fn check_browser_for_meeting(pid: i32) -> Option<BrowserMeetingInfo> {
    use cidre::{ax, cf};

    if !ax::is_process_trusted() {
        tracing::warn!(
            "Meeting detection: Accessibility permission not granted. \
             Grant access in System Settings > Privacy & Security > Accessibility. \
             Browser meeting detection (Google Meet) requires this permission."
        );
        // Prompt the user to grant permission
        let _ = ax::is_process_trusted_with_prompt(true);
        return None;
    }

    // The audio-producing process might be a helper (e.g., Chrome Helper).
    // We need to find the main browser process PID to read window titles.
    // Try the given PID first; if it has no windows, find the main app PID.
    let main_pid = find_main_browser_pid(pid);
    tracing::info!(
        "Meeting detection: checking browser pid={} (main_pid={}) for meeting tabs",
        pid, main_pid
    );

    let app = ax::UiElement::with_app_pid(main_pid);

    // Get AXWindows attribute — returns array of window elements
    let windows_val = match app.attr_value(ax::attr::windows()) {
        Ok(val) => val,
        Err(e) => {
            tracing::warn!(
                "Meeting detection: failed to get AXWindows for pid={}: {:?}. \
                 This usually means Accessibility permission is not granted to this app.",
                main_pid, e
            );
            return None;
        }
    };

    // Safety: AXWindows returns a CFArray of AXUIElements
    let windows: &cf::ArrayOf<ax::UiElement> = unsafe {
        std::mem::transmute(windows_val.as_ref())
    };

    tracing::info!("Meeting detection: browser has {} windows", windows.len());

    for i in 0..windows.len() {
        let window = &windows[i];

        // Read window title (reflects active tab in Chrome/Edge)
        let title_val = match window.attr_value(ax::attr::title()) {
            Ok(val) => val,
            Err(e) => {
                tracing::debug!("Meeting detection: failed to read title for window {}: {:?}", i, e);
                continue;
            }
        };

        // Safety: AXTitle returns a CFString
        let title_cf: &cf::String = unsafe {
            std::mem::transmute(title_val.as_ref())
        };
        let title = title_cf.to_string();

        // PRIVACY: Check against meeting patterns immediately.
        // If no match, the title is dropped here — never stored or logged.
        for pattern in MEETING_TITLE_PATTERNS {
            if title.contains(pattern) {
                let meeting_name = extract_meeting_name(&title, pattern);
                tracing::info!(
                    "Meeting detection: found meeting in browser — pattern='{}', name='{}'",
                    pattern, meeting_name
                );
                return Some(BrowserMeetingInfo {
                    meeting_name: sanitize_meeting_name(&meeting_name),
                    pattern: pattern.to_string(),
                });
            }
        }

        // Title did not match any pattern — dropped immediately (privacy)
        // Only log that we checked, not the actual title content
        tracing::debug!("Meeting detection: browser window checked, no meeting pattern matched");
    }

    None
}

#[cfg(not(target_os = "macos"))]
pub fn check_browser_for_meeting(_pid: i32) -> Option<BrowserMeetingInfo> {
    None
}

/// Find the main browser process PID from a helper process PID.
/// Chrome audio is typically produced by a helper/renderer process, but
/// window titles are only accessible on the main app process.
#[cfg(target_os = "macos")]
fn find_main_browser_pid(helper_pid: i32) -> i32 {
    use sysinfo::{Pid, System, ProcessesToUpdate};

    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);

    // Check if this PID has a known browser name — if so, it's already the main process
    if let Some(proc) = sys.process(Pid::from_u32(helper_pid as u32)) {
        let name = proc.name().to_string_lossy().to_string();
        if name == "Google Chrome" || name == "Microsoft Edge" || name == "Brave Browser" || name == "Arc" {
            return helper_pid;
        }

        // It's a helper — find the parent process or scan for the main browser
        if let Some(parent_pid) = proc.parent() {
            if let Some(parent) = sys.process(parent_pid) {
                let parent_name = parent.name().to_string_lossy().to_string();
                if parent_name == "Google Chrome" || parent_name == "Microsoft Edge"
                    || parent_name == "Brave Browser" || parent_name == "Arc"
                {
                    tracing::info!(
                        "Meeting detection: resolved helper pid={} to main browser pid={} ({})",
                        helper_pid, parent_pid.as_u32(), parent_name
                    );
                    return parent_pid.as_u32() as i32;
                }
            }
        }

        // Walk up the process tree (Chrome helpers can be nested)
        let mut current_pid = proc.parent();
        for _ in 0..5 {
            match current_pid {
                Some(pid) => {
                    if let Some(p) = sys.process(pid) {
                        let pname = p.name().to_string_lossy().to_string();
                        if pname == "Google Chrome" || pname == "Microsoft Edge"
                            || pname == "Brave Browser" || pname == "Arc"
                        {
                            return pid.as_u32() as i32;
                        }
                        current_pid = p.parent();
                    } else {
                        break;
                    }
                }
                None => break,
            }
        }
    }

    // Fallback: return the original PID
    helper_pid
}

/// Extract a clean meeting name from a browser window title.
/// Examples:
///   "Meet - Weekly Standup - Google Chrome" → "Weekly Standup"
///   "Team Meeting - Google Meet" → "Team Meeting"
///   "Microsoft Teams" → "Microsoft Teams"
fn extract_meeting_name(title: &str, matched_pattern: &str) -> String {
    // Google Meet format: "Meet - <name>" or "<name> - Google Meet"
    if matched_pattern == "Meet - " {
        // Title is like "Meet - Weekly Standup - Google Chrome"
        if let Some(after_meet) = title.strip_prefix("Meet - ") {
            // Remove browser suffix (e.g., " - Google Chrome")
            return strip_browser_suffix(after_meet);
        }
    }

    if matched_pattern == "Google Meet" {
        // Title might be "<name> - Google Meet - Google Chrome"
        if let Some(idx) = title.find("Google Meet") {
            let before = title[..idx].trim_end_matches(" - ").trim();
            if !before.is_empty() {
                return strip_browser_suffix(before);
            }
        }
    }

    // For other patterns, use the full title minus browser suffix
    strip_browser_suffix(title)
}

/// Remove common browser name suffixes from a title string
fn strip_browser_suffix(title: &str) -> String {
    let suffixes = [
        " - Google Chrome",
        " - Microsoft Edge",
        " - Brave",
        " - Arc",
        " - Mozilla Firefox",
        " - Safari",
    ];

    let mut result = title.to_string();
    for suffix in &suffixes {
        if let Some(stripped) = result.strip_suffix(suffix) {
            result = stripped.to_string();
            break;
        }
    }
    result.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_google_meet_name() {
        assert_eq!(
            extract_meeting_name("Meet - Weekly Standup - Google Chrome", "Meet - "),
            "Weekly Standup"
        );
    }

    #[test]
    fn test_extract_google_meet_name_edge() {
        assert_eq!(
            extract_meeting_name("Meet - 1:1 with Manager - Microsoft Edge", "Meet - "),
            "1:1 with Manager"
        );
    }

    #[test]
    fn test_extract_google_meet_alternative_format() {
        assert_eq!(
            extract_meeting_name("Team Planning - Google Meet - Google Chrome", "Google Meet"),
            "Team Planning"
        );
    }

    #[test]
    fn test_extract_teams_web() {
        let result = extract_meeting_name("Microsoft Teams - Google Chrome", "Microsoft Teams");
        assert_eq!(result, "Microsoft Teams");
    }

    #[test]
    fn test_strip_browser_suffix() {
        assert_eq!(strip_browser_suffix("Hello - Google Chrome"), "Hello");
        assert_eq!(strip_browser_suffix("Hello - Brave"), "Hello");
        assert_eq!(strip_browser_suffix("Hello"), "Hello");
    }
}
