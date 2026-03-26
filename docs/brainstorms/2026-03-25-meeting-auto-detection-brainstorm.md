# Meeting Auto-Detection & Notification

**Date:** 2026-03-25
**Status:** Brainstorm
**Branch:** TBD (new branch off main)

## What We're Building

An automatic meeting detection system that monitors for active meeting applications, notifies the user when a meeting is detected ("Meeting detected — Start taking notes?"), and allows one-click recording start directly from the notification. When the meeting ends (app audio stops), recording auto-stops with a grace period.

**Core flow:**
1. Background monitoring detects a meeting app using audio (macOS: CoreAudio, Windows: process monitoring)
2. After 15 seconds of sustained activity, a native notification appears with a "Start Recording" action button
3. User clicks the button — recording starts immediately (no need to open the app window)
4. When the meeting app stops using audio, recording auto-stops after a 30s grace period
5. 10-minute cooldown per app prevents notification spam

## Why This Approach

**Audio detection over calendar integration:**
- Simpler scope — most infrastructure already exists in the codebase (`system_detector.rs`, notification system)
- Catches ad-hoc calls (Slack huddles, FaceTime, Discord) that calendar integration misses
- Calendar integration can be added later as an enhancement
- Aligns with how Granola, Notion, and Char all work (audio detection is the primary trigger)

**Notify-and-wait over auto-record:**
- Privacy-first product — user must consent to each recording
- Industry standard: Granola, Notion, and Char all require explicit user action
- Avoids recording conversations users didn't intend to capture
- Reduces legal/compliance risk

**Char's threshold/cooldown pattern (15s/10min):**
- Proven in production open-source app
- 15s threshold eliminates false positives from brief mic tests, system sounds, or quick voice messages
- 10min cooldown prevents repeated notifications for the same ongoing meeting
- Generation-based timer tracking prevents stale timers from firing

## Key Decisions

1. **Detection method:** Audio/process monitoring (not calendar integration)
2. **User interaction:** Notification with actionable "Start Recording" button (not auto-record)
3. **Auto-stop:** When meeting app audio stops, with 30s grace period
4. **Thresholds:** 15-second sustained activity before notification, 10-minute cooldown per app
5. **App filtering:** Default ignore list for non-meeting apps (dictation, screen recorders, IDEs, Siri), user-customizable
6. **Platforms:** macOS (CoreAudio property listeners) + Windows (process monitoring)
7. **Notification UX:** Actionable notification with "Start Recording" button — no need to open app window
8. **Browser meetings:** Detect via accessibility API / window title matching (e.g., "Meet - " prefix in tab titles). Critical for Google Meet support.
9. **Auto-stop grace period:** 30 seconds
10. **System tray:** Change tray icon state (e.g., dot or color shift) when monitoring is active

## Existing Infrastructure to Leverage

### Already Built (dormant)
- **`system_detector.rs`** — macOS CoreAudio `DEVICE_IS_RUNNING_SOMEWHERE` listener, app identification via PID. Emits `SystemAudioStarted(Vec<String>)` / `SystemAudioStopped` events. Tauri commands registered but frontend never calls them.
- **Notification system** — `MeetingReminder` type, `show_meeting_reminder()`, `NotificationAction::Button`, notification settings with DND respect. All exist but never triggered.
- **Auto-start mechanism** — `sessionStorage('autoStartRecording')` pattern and `start-recording-from-sidebar` event provide a working auto-start flow.
- **Tauri commands** — `start_system_audio_monitoring`, `stop_system_audio_monitoring`, `get_system_audio_monitoring_status` all registered.

### Needs Building
- **Meeting detection coordinator** — Service that ties system audio detection, threshold/cooldown logic, and notifications together
- **Threshold timer** — 15s sustained activity timer with generation tracking (prevents stale timer fires)
- **Cooldown tracker** — Per-app 10min cooldown to prevent notification spam
- **App ignore/allow list** — Categorized bundle ID list (macOS) / process name list (Windows) with defaults
- **Windows process monitor** — Poll running processes to detect known meeting apps
- **Browser meeting detector** — Accessibility API integration to read browser window/tab titles and match meeting URL patterns (meet.google.com, teams.microsoft.com, zoom.us)
- **Auto-stop logic** — Monitor for meeting app audio stop, wait 30s grace period, then stop recording
- **Actionable notification handler** — Wire "Start Recording" button in notification to trigger recording without opening window
- **Tray icon state** — Visual indicator in system tray when monitoring is active
- **Settings UI** — Toggle for meeting detection on/off, threshold/cooldown config, app list management

## Platform-Specific Design

### macOS (Primary)
- **Detection:** CoreAudio `DEVICE_IS_RUNNING_SOMEWHERE` property listener (event-driven, not polling)
- **App identification:** `ca::System::processes()` -> filter `is_running_input()` -> resolve PID to app via `NSRunningApplication`
- **Ignore list:** Bundle IDs (e.g., `com.apple.SpeechRecognitionCore`, `com.apple.dictation`, `com.meetily.app`)
- **Already exists:** `MacOSSystemAudioDetector` in `system_detector.rs`

### Windows
- **Detection:** Poll running processes every 3-5 seconds, check against known meeting app executable names
- **Known apps:** `zoom.exe`, `Teams.exe`, `slack.exe`, `discord.exe`, `webexmeetings.exe`, browser processes with meeting URLs
- **Ignore list:** Process names for non-meeting apps
- **Note:** Less precise than macOS — can detect app is running but not necessarily that audio is active. Process enumeration tells us "Zoom is running" not "Zoom is in a meeting." May need to combine with WASAPI session check as a future enhancement.

## Competitive Reference

| Aspect | Granola | Notion | Char | **Meetily (proposed)** |
|---|---|---|---|---|
| Detection | Calendar + mic | Process mic monitoring | CoreAudio + calendar | CoreAudio (macOS) + process (Windows) |
| Threshold | Unknown | Unknown | 15s + 10min cooldown | 15s + 10min cooldown |
| App filter | Unclear | Process names | Bundle ID categories | Bundle IDs (macOS) + process names (Windows) |
| Auto-record | No | No | No | No |
| Auto-stop | Calendar end / manual | Manual | Mic stops | App audio stops + 30s grace |
| Notification action | Prompt | Prompt | Confirm button | "Start Recording" button |

## Browser Meeting Detection (Critical Path)

Since the team uses Google Meet (browser-based), browser detection is a first-class requirement, not an afterthought.

**Approach: Accessibility API window/tab title matching**

- **macOS:** Use `AXUIElement` accessibility API to enumerate browser windows and read tab titles. Google Meet tabs show "Meet - [meeting name]" in the title bar. Teams web shows "Microsoft Teams" etc.
- **Windows:** Use `EnumWindows` / UI Automation API to read browser window titles. Same title-matching pattern.
- **Known meeting URL patterns to match in titles:**
  - `Meet - ` (Google Meet)
  - `Microsoft Teams` (Teams web)
  - `Zoom Meeting` / `Zoom Webinar`
  - Custom patterns (user-configurable)
- **Flow:** When CoreAudio (macOS) or process monitoring (Windows) detects a browser using audio, additionally check browser window titles. Only trigger notification if a meeting-related title is found. Regular browser audio (YouTube, Spotify) will not match and will be ignored.
- **Privacy note:** Only reads window titles, does not access page content or URLs.

## Resolved Questions

1. **Browser-based meetings:** Use accessibility API to read browser tab titles and match meeting patterns. Critical for Google Meet support.
2. **Grace period:** 30 seconds.
3. **System tray indicator:** Yes — change tray icon state when monitoring is active.

## Open Questions

None — all design questions resolved.
