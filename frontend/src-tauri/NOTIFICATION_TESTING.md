# Testing Notifications on macOS

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
- A native macOS notification appear in the top-right corner
- Title: "Meetily"
- Body: "Recording has started" (or test message)
- The notification should appear like system notifications (microphone detected, etc.)

## Troubleshooting

### If notifications don't appear:

1. **Check macOS Notification Settings:**
   - Go to System Preferences → Notifications & Focus
   - Find your app in the list
   - Ensure notifications are enabled

2. **Check Do Not Disturb:**
   - Make sure Do Not Disturb is off
   - Or use: `await invoke('get_system_dnd_status')` to check

3. **Check Logs:**
   - Look for log messages about notification initialization
   - Check for permission/consent messages

4. **Manual Permission Request:**
   ```javascript
   await invoke('request_notification_permission');
   ```

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
- For development/testing, consent and permissions are automatically granted
- The system respects Do Not Disturb settings
- All notification preferences are saved locally