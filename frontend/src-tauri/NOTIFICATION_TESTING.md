# Testing Notifications on macOS

## Prerequisites: Must run from a signed `.app` bundle

`UNUserNotificationCenter` (the API we use on macOS) requires a real bundle identity — `CFBundleIdentifier` from the `.app`'s `Info.plist`. Running `target/debug/meetily` directly via `tauri dev` has no bundle, so notifications are silently dropped. To test notifications you must build and launch the actual bundle; see [docs/solutions/build-errors/macos-dev-build-notifications-and-signing.md](../../docs/solutions/build-errors/macos-dev-build-notifications-and-signing.md) for the one-command build + re-sign + open loop.

## UI: Debug Notifications Dropdown

The fastest way to test each notification type is **About → Debug Notifications** in the app. The dropdown exposes every notification type (recording started/stopped/paused/resumed, transcription complete, meeting reminder, system error, generic test) and fires each one through the real production code path.

Because it uses the production path, the dropdown **respects user consent and per-type preferences** in Settings → Preferences → Notifications. If a type is suppressed, the UI shows a `toast.info` explaining why instead of firing silently. To bypass consent entirely during deep troubleshooting, use `test_notification_with_auto_consent` from DevTools (see below).

## Quick Test Commands

### 1. Test Notification Immediately
To test if notifications are working, call this command from your frontend:

```javascript
// This will initialize the notification system and show a test notification
await invoke('test_notification_with_auto_consent');
```

### 2. Initialize Notification System First
If you want to initialize the notification system manually:

```javascript
// Initialize the notification system
await invoke('initialize_notification_manager_manual');

// Then show a test notification
await invoke('show_test_notification');
```

### 3. Recording Notifications
When you start recording, the app should automatically show a notification. The system will:

1. Check if notification manager is initialized
2. Automatically grant consent and permissions for testing
3. Show "Recording has started" notification

## Expected Behavior on macOS

When working correctly, you should see:
- A native macOS notification appear in the top-right corner (banner style)
- Title: "Meetily"
- Body: "Recording has started" (or test message)
- The notification should appear like system notifications (microphone detected, etc.)
- Rust logs: `UN present: id=<uuid> title="…" level=<Active|TimeSensitive|...>`

## Troubleshooting

### First-launch permission prompt is one-shot per bundle ID

On the first run of a new build, macOS prompts "Would meetily like to send notifications?" Click **Allow**. If you miss the prompt or click Don't Allow, there is no API to re-prompt — you must remove the app's entry from **System Settings → Notifications** and relaunch.

### If notifications don't appear:

1. **Confirm authorization status:**
   ```javascript
   await window.__TAURI_INTERNALS__.invoke('get_notification_stats')
   ```
   Must return `system_permission_granted: true`. If `false`, see the first-launch paragraph above.

2. **Confirm Alert Style is Banners (not None):**
   - System Settings → Notifications → meetily
   - Alert Style: **Temporary** or **Persistent** (not blank/None)
   - Show on Lock Screen / Notification Centre / Desktop all enabled
   - If a previous *production* install at `/Applications/meetily.app` exists, its stale entry can hijack your dev build's preferences (they share bundle ID `com.meetily.ai`). Remove it or see the solution doc for the LaunchServices cleanup.

3. **Focus / Scheduled Summary / Deliver Quietly:**
   - Check top-right Control Center: if Focus/DND is on, banners are suppressed.
   - System Settings → Notifications → Scheduled Summary: meetily must **not** be listed.
   - The "Deliver Quietly" toggle on a banner's kebab menu persists — if set, all future banners go to NC silently. Revert via Alert Style panel.

4. **Kick the notification daemon:**
   ```bash
   killall usernoted
   ```
   Harmless; restarts the system notification service. Try after confirming 1–3.

5. **Manual permission request:**
   ```javascript
   await invoke('request_notification_permission');
   ```
   Returns `true` if already granted; triggers the OS prompt if not-determined; returns `false` if denied (no re-prompt possible).

## Available Commands for Testing

```javascript
// System status
await invoke('is_notification_system_ready');
await invoke('get_system_dnd_status');
await invoke('get_notification_stats');

// Permissions and consent
await invoke('request_notification_permission');
await invoke('set_notification_consent', { consent: true });

// Testing
await invoke('test_notification_with_auto_consent');
await invoke('show_test_notification');

// Settings
await invoke('get_notification_settings');
```

## Development Notes

- The notification system is designed to work like native macOS notifications
- On macOS, sends go through `UNUserNotificationCenter` directly (see `src/notifications/macos_un.rs`) because `tauri-plugin-notification`'s `NSUserNotification` path doesn't deliver banners on modern macOS
- Windows and Linux still use `tauri-plugin-notification`
- `NotificationPriority` maps to `UNNotificationInterruptionLevel` as: Low→Passive, Normal→Active, High/Critical→TimeSensitive (Critical level requires an Apple-granted entitlement we don't have; TimeSensitive is the closest unentitled equivalent)
- For development/testing, consent and permissions are automatically granted
- The system respects Do Not Disturb settings
- All notification preferences are saved locally