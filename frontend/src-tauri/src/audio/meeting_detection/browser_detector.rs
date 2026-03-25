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
        return None;
    }

    let app = ax::UiElement::with_app_pid(pid);

    // Get AXWindows attribute — returns array of window elements
    let windows_val = match app.attr_value(ax::attr::windows()) {
        Ok(val) => val,
        Err(_) => return None,
    };

    // Safety: AXWindows returns a CFArray of AXUIElements
    let windows: &cf::ArrayOf<ax::UiElement> = unsafe {
        std::mem::transmute(windows_val.as_ref())
    };

    for i in 0..windows.len() {
        let window = &windows[i];

        // Read window title (reflects active tab in Chrome/Edge)
        let title_val = match window.attr_value(ax::attr::title()) {
            Ok(val) => val,
            Err(_) => continue,
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
                // Extract a clean meeting name from the title
                let meeting_name = extract_meeting_name(&title, pattern);
                return Some(BrowserMeetingInfo {
                    meeting_name: sanitize_meeting_name(&meeting_name),
                    pattern: pattern.to_string(),
                });
            }
        }

        // Title did not match any pattern — dropped immediately (privacy)
    }

    None
}

#[cfg(not(target_os = "macos"))]
pub fn check_browser_for_meeting(_pid: i32) -> Option<BrowserMeetingInfo> {
    None
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
