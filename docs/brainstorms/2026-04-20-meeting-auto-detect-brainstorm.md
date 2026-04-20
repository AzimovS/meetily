# Meeting Auto-Detect + Richer Recording Notifications

**Date:** 2026-04-20
**Status:** Brainstorm (v2 — expanded after detection research)
**Scope:** Ships independently of calendar integration. No network, no OAuth. Accessibility permission required on macOS; no new permissions on Windows.

## Problem / Motivation

The most common failure mode for Meetily users is the "I forgot to press record" moment — the user is already in a call before they remember to capture it. The existing recording-started/stopped notifications (`frontend/src-tauri/src/notifications/`) are confirmations fired *after* the user clicks Record; they don't help at the moment of intent. Those notifications also default to **off** (`settings.rs:83-93`).

Meetings today come in two flavors:

- **Native-app meetings** — Zoom, Teams, Webex, Discord, Slack huddles, FaceTime. Detectable by process/bundle ID. Easy.
- **Browser meetings** — Google Meet, Jitsi, Whereby, Around, Teams-in-browser, Zoom-in-browser. These are tabs inside Chrome/Safari/Firefox/Arc. A naive process check sees "Chrome" — useless.

A meeting detector that only handles native apps will feel broken to the huge share of users whose main tool is Google Meet in Chrome. v1 must cover both.

## What We're Building

### 1. Composite signal detector (macOS)

Every 5s when idle, 1s when a candidate signal is active, collect:

- **Process signal**: `NSWorkspace.runningApplications` bundle-ID match against a hard-coded list (Zoom, Teams, Webex, Discord, Slack, FaceTime). No permission. Catches native apps.
- **Browser signal** (requires Accessibility): for Chromium family / Safari / Firefox / Arc, query the frontmost app via `AXUIElement`, walk to the active `AXWebArea`, read `kAXURLAttribute`. Match against a URL pattern list: `meet.google.com/*`, `*.zoom.us/j/*`, `teams.microsoft.com/*/meetup*`, `app.jitsi.net/*`, `whereby.com/*`, `around.co/*`, `huddle.app/*`. No Screen Recording required — Accessibility only.
- **Audio-activity signal** (macOS 14.4+, public API): CoreAudio process enumeration reads `kAudioProcessPropertyPID` + `kAudioProcessPropertyIsRunning`/`kAudioProcessPropertyIsRunningInput` to identify which PIDs are currently producing/consuming audio. No permission for enumeration. On macOS 14.3 and earlier, fall back to `AVCaptureDevice.isInUseByAnotherApplication` (weaker signal).

Fire `MeetingDetected` when either of these is true:

- `bundle_id ∈ native_meeting_apps`
- `frontmost_app ∈ browsers ∧ active_tab_url_matches_pattern ∧ pid_has_active_audio`

The AND-gating on the browser path eliminates false positives from "I have a Meet tab open in the background but haven't joined."

### 2. Composite signal detector (Windows)

- **Process signal**: `EnumProcesses` against the same app list. No permission.
- **Window-title signal**: `EnumWindows` + `GetWindowText` reads the focused window's title. Chrome's top-level window title equals the active tab title, so this catches browser meetings **when the tab is focused**. No permission.
- **Audio-activity signal**: `IAudioSessionManager2::GetSessionEnumerator` enumerates every PID with an active audio session, plus `AudioMeterInformation` for whether it's currently peaking. No permission.

Fire `MeetingDetected` when either:

- `bundle_id ∈ native_meeting_apps`
- `focused_window_title_matches_pattern ∧ pid_has_active_audio_session`

**Windows v1 gap (accepted):** backgrounded browser meeting tabs are invisible to this approach. We rely on the user bringing the tab to focus, which is what they usually do when actively participating. v2 closes the gap with an optional browser extension (see "What We're NOT Building").

### 3. Notification content

| Event | Title | Body | Action |
|-------|-------|------|--------|
| Meeting detected (native) | "Meeting detected" | "Zoom is active — record this meeting?" | "Start recording" |
| Meeting detected (browser) | "Meeting detected" | "Google Meet in Chrome — record this meeting?" | "Start recording" |
| Recording started (existing) | Unchanged | Unchanged | — |
| Recording stopped (enriched) | "Recording saved" | "42 min, 3 speakers — Open summary" | "Open summary" |

The "Start recording" action invokes `start_recording` with a `source: "detected"` marker and the platform name ("Zoom", "Google Meet", "Jitsi") as the meeting title hint. "Open summary" routes to the meeting detail page, showing a "Summary pending" state if summarization isn't finished.

### 4. Defaults and onboarding

- `show_recording_started = true` (was false)
- `show_recording_stopped = true` (was false)
- `show_meeting_detected = true` (new)
- **First-run Accessibility prompt** on macOS: an in-app modal explaining why ("Meetily detects meetings by reading your active window title and browser tab URL. No pixels, no audio, no network. You can revoke at any time in System Settings → Privacy & Security → Accessibility."), with a "Grant Accessibility" button that calls `AXIsProcessTrustedWithOptions` to trigger the system prompt. Skippable — detection falls back to native-app-only if declined.
- Existing users get a one-time onboarding toast explaining the new detection and how to opt out.

## Why This Approach

**Accessibility over Screen Recording.** `CGWindowListCopyWindowInfo` could read every window title, but since Catalina it requires Screen Recording permission — the permission users associate with "this app can see everything on my screen." Accessibility API reads the exact same titles plus browser URLs without that baggage. Raycast, AltTab, Rectangle, and MacWhisper all use Accessibility; it's a well-understood ask that converts.

**Composite signals beat single signals.** Process-only (MacWhisper's approach) misses browser meetings entirely — "Chrome is running" is useless. Audio-only (Krisp's approach) fires on voice memos, music recording, and every browser tab with getUserMedia. Combining process + URL + audio-activity gives us precision on both paths.

**Reuses existing notification scaffolding.** One new `NotificationType::MeetingDetected` variant, one new setting flag. Consent/DND/per-type gating (`manager.rs:277-305`) already applies.

**Windows tier 2 is honest.** We cover native apps fully and focused browser tabs fully. Backgrounded browser tabs are v2 via a browser extension. This is communicated in onboarding rather than hidden.

**No new runtime permissions on Windows.** All three Windows signals (EnumProcesses, EnumWindows, IAudioSessionManager2) work without admin and without special manifests.

## Approaches Considered

### A. Composite detector: process + AX URL + audio-activity (RECOMMENDED)

Ship all three signals with composite scoring. Requires Accessibility permission on macOS, none on Windows.

**Pros:** Covers native + browser meetings. Low false-positive rate. Uses public APIs only. Matches the precision of Raycast-tier desktop utilities.
**Cons:** Accessibility permission prompt may scare some users; fallback to native-only is degraded but functional. Windows backgrounded-tab gap in v1.

### B. Process detection only

Match bundle IDs / exe names; skip AX and audio signals.

**Pros:** Ships in days; no permissions; simplest code.
**Cons:** Does not detect Google Meet, Jitsi, or any browser-based meeting at all — which for many users *is* every meeting. MacWhisper does this and its support channel is full of "why didn't it detect my Meet call?" threads. Not viable.

### C. Browser extension as the primary path

Ship a Chrome/Edge/Firefox/Safari extension that pushes URL changes to the Tauri app via native messaging.

**Pros:** Most reliable browser detection; works for backgrounded tabs on Windows; no AX prompt.
**Cons:** Extension install is real friction (Chrome Web Store review, Firefox Add-ons, Safari requires App Store build). Doesn't help Zoom/Teams/Webex native at all — those still need process detection. Multiplies surfaces rather than replacing them. **Better as v2 reliability layer on top of v1.**

### D. CGWindowList titles (Screen Recording)

Use `CGWindowListCopyWindowInfo` to read Chrome's window title, which includes the active tab.

**Pros:** Simpler than AX. Reads background window titles too.
**Cons:** Requires Screen Recording permission — users refuse this permission much more often than Accessibility. Can't read tab URLs, only titles (so `"Meet"` vs `"Team Standup - Meet - Chrome"` depends on tab-title quirks). **Worse UX trade than B.**

## Key Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Detection signals (macOS) | Process **OR** (AX URL **AND** audio-active) | Precision on both native + browser paths |
| Detection signals (Windows) | Process **OR** (focused window title **AND** audio session) | Same shape; backgrounded tabs deferred to v2 |
| macOS audio API | CoreAudio process enum on 14.4+, `isInUseByAnotherApplication` fallback | Public, no permission, runtime-detected |
| Windows audio API | `IAudioSessionManager2` with `AudioMeterInformation` | No permission; per-PID activity |
| Browser URL source (macOS) | Accessibility API `AXWebArea` + `kAXURLAttribute` | Works across Chromium / Safari / Firefox / Arc; lower-stigma than Screen Recording |
| Browser URL source (Windows) | None in v1 — focused window title only | UIA URL reading is inconsistent; extension is the real fix (v2) |
| App list source | Hard-coded `const` in Rust | Simple; user-editable JSON is v2 |
| URL pattern list | Hard-coded `const` in Rust | Same; small, stable list of meeting services |
| Poll cadence | 5s idle, 1s when a candidate signal is active | Cheap when nothing is happening, responsive when it is |
| Re-notify debounce | Suppress same-app notify within 10 min of dismissal | Prevents pestering |
| Accessibility opt-in | First-run modal with plain-English rationale + skip option | Users understand what they're granting |
| Defaults | Start/stop/detected all ON | Feature is useless if users must opt in |
| Opt-out | Per-type toggles in existing Preferences | No new UI surface |
| Notification content | Platform name in body ("Google Meet in Chrome") | Tells the user what we detected; builds trust |

## Detection Truth Table

| Scenario | Process | AX URL | Audio | Result |
|----------|---------|--------|-------|--------|
| Zoom native app, in a call | ✅ Zoom | — | ✅ | **Detected (native path)** |
| Zoom native, idle in dock | ✅ Zoom | — | ❌ | Detected-candidate, no notify yet |
| Chrome on a Meet tab, in a call | ✅ Chrome | ✅ meet.google.com | ✅ | **Detected (browser path)** |
| Chrome on a Meet tab, muted & idle | ✅ Chrome | ✅ meet.google.com | ❌ | No notify |
| Chrome on a random tab | ✅ Chrome | ❌ | ❌ or ✅ | No notify |
| Music in Chrome | ✅ Chrome | ❌ | ✅ | No notify |
| Accessibility declined, Zoom native | ✅ Zoom | n/a | — | **Detected (process path)** |
| Accessibility declined, Chrome Meet | ✅ Chrome | n/a | ✅ | No notify (acceptable degraded state) |

## What We're NOT Building (YAGNI in v1)

- **Browser extension + native messaging** — v2. The AX path covers most macOS cases; extension closes the Windows gap and adds robustness. Distinct work item.
- **Calendar awareness** — sibling brainstorm (Google Calendar integration)
- **Attendee extraction** — sibling brainstorm
- **Chrome DevTools Protocol via `--remote-debugging-port`** — off by default, breaks Chrome sync, non-starter for consumer UX
- **Network / packet detection** — requires root/admin
- **Auto-record without user click** — user directive; also the legal gate
- **Editable meeting-app or URL list in UI** — v2
- **Smart "recurring meeting" grouping** — v2
- **Temporal / ML-based detection** — YAGNI

## Resolved Questions

1. **Both signals or either?** Composite OR with path-specific AND: native = process only, browser = URL + audio. Balances recall and precision.
2. **macOS browser detection in v1?** Yes — via Accessibility API. Research showed it's tractable and is the industry-standard approach (Raycast pattern).
3. **Audio-activity API on macOS?** Public CoreAudio process API on 14.4+, `isInUseByAnotherApplication` fallback. No private APIs.
4. **Windows browser detection in v1?** Focused tabs only; backgrounded tabs deferred to v2 extension. Communicated in onboarding.
5. **Bundle ID + URL lists.** Hard-coded const. User-editable JSON is v2.
6. **Stop notification click target.** Summary view with "Summary pending" state if not yet ready.
7. **Defaults.** ON by default, with onboarding toast + Accessibility prompt on macOS.

## Open Questions (worth probing before plan lock-in)

1. **macOS 14.4+ CoreAudio process API with Chrome.** Does `kAudioProcessPropertyIsRunningInput` fire for a Chrome renderer when a tab calls `getUserMedia`, or only for Chrome's main audio process? Needs a 30-min prototype. If only the main process fires, we may need to treat "Chrome has any audio activity" as the signal and rely on the URL to disambiguate which tab.
2. **Firefox AXURL reliability.** Chromium and Safari expose `AXURL` on their `AXWebArea` reliably; Firefox historically has patchy AX. Worth verifying in 2026 Firefox before shipping.
3. **Arc multi-window behavior.** Arc's workspace model may affect how `AXWebArea` surfaces the "active" URL. Probe anecdotally.
4. **Meeting-app list scope.** Include Slack huddles, Discord voice calls, FaceTime group calls by default? Each has different user expectations of "is this a meeting?"

## Next Steps

1. One-hour prototype covering the three unknowns above (macOS CoreAudio + Chrome, Firefox AXURL, Arc)
2. Run `/workflows:plan` on this brainstorm
3. Implementation sequence:
   1. Process detector (shared native foundation)
   2. macOS AX URL reader behind Accessibility opt-in
   3. macOS CoreAudio process enum + fallback
   4. Windows IAudioSessionManager2 reader
   5. Windows EnumWindows reader
   6. Composite scorer + debounce logic
   7. Notification wiring (new `MeetingDetected` type) + defaults flip
   8. Enriched stop notification (duration + summary link)
   9. Onboarding toast + Accessibility explainer modal
