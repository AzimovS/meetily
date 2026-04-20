---
title: "macOS Notifications Silently Dropped in `tauri dev` + Library Validation on Debug Bundle"
slug: macos-dev-build-notifications-and-signing
category: build-errors
problem_type: macos-bundle-identity
component: notifications
severity: high
symptoms:
  - "tauri-plugin-notification reports success but no top-right banner appears"
  - "Notification Center remains empty despite 'Successfully showed Tauri notification' log"
  - "Banner still missing from signed bundle even when NC entries land correctly"
  - "osascript display notification works, proving OS permissions are fine"
  - "tauri dev runs target/debug/meetily directly without .app bundle"
  - "pnpm exec tauri build --debug bundle fails with 'cannot be opened because of a problem'"
  - "dyld error: Library not loaded: /opt/homebrew/opt/onnxruntime/lib/libonnxruntime.1.24.3.dylib"
  - "mapping process and mapped file (non-platform) have different Team IDs"
  - "Hardened runtime + ad-hoc signature rejects Homebrew dylib with Team ID"
tags:
  - macos
  - tauri
  - tauri-v2
  - notifications
  - code-signing
  - hardened-runtime
  - library-validation
  - bundle-identifier
  - dyld
  - onnxruntime
  - homebrew
  - usernoted
  - dev-mode
date: 2026-04-19
related:
  - docs/solutions/build-errors/macos-updater-missing-network-entitlement.md
  - docs/BUILDING.md
  - docs/brainstorms/2026-04-18-notification-debug-dropdown-brainstorm.md
---

# macOS Notifications Silently Dropped in `tauri dev` + Library Validation on Debug Bundle

## Problem

During local dev testing of a new "Debug Notifications" dropdown on macOS (Apple Silicon, Terminal.app), OS-level banner notifications from Meetily never appeared in the top-right of the screen. The `tauri-plugin-notification` path returned success and the Rust logs clearly showed:

```
[INFO  app_lib::notifications::manager] Notification system initialized successfully
[INFO  app_lib::notifications::system] Successfully showed Tauri notification: Meetily
[INFO  app_lib::notifications::system] Updated system notification permission: true
```

Yet nothing was delivered visually — Notification Center itself was also empty. A control `osascript -e 'display notification "test" with title "test"'` worked fine from the same terminal, confirming the system's notification daemon and Terminal.app permissions were healthy.

The issue was deceptive: every API contract reported success. Running `./clean_run.sh` (which wraps `tauri dev`) launched `target/debug/meetily` directly. Switching to `pnpm exec tauri build --debug` produced a bundle, but double-clicking `meetily.app` instead crashed at load with a Gatekeeper dialog ("meetily cannot be opened because of a problem") backed by a `dyld` error:

```
dyld[44855]: Library not loaded: /opt/homebrew/opt/onnxruntime/lib/libonnxruntime.1.24.3.dylib
  Reason: tried: '/opt/homebrew/opt/onnxruntime/lib/libonnxruntime.1.24.3.dylib'
  (code signature in <...>/libonnxruntime.1.24.3.dylib not valid for use in process:
   mapping process and mapped file (non-platform) have different Team IDs)
```

So the dev binary silently swallowed notifications, and the bundle refused to launch at all.

## Root Cause

### 1. No `CFBundleIdentifier` in `tauri dev`

`tauri dev` launches the raw `target/debug/meetily` binary, which has no `Info.plist` and therefore no `CFBundleIdentifier`. macOS's `UNUserNotificationCenter` needs an app identity to route a notification to; without one it accepts the API call, returns success, and drops the delivery. The well-known Tauri quirk of notifications inheriting the parent Terminal.app icon in dev mode is a separate symptom of the same "no bundle identity" root cause — here delivery fails outright.

### 2. Hardened runtime + ad-hoc signature + Homebrew dylib Team ID mismatch

`pnpm exec tauri build --debug` signs the bundle ad-hoc with hardened runtime enabled. `codesign -dv` reports:

```
CodeDirectory v=20500 size=224047 flags=0x10002(adhoc,runtime) hashes=6991+7
Signature=adhoc
TeamIdentifier=not set
```

The bundle dynamically links `libonnxruntime.1.24.3.dylib` from `/opt/homebrew/opt/onnxruntime/...`, which carries its own non-empty Team ID (Homebrew signs prebuilt binaries). With hardened runtime enabled, macOS **library validation** requires every loaded dylib's Team ID to match the process's. An ad-hoc process has no Team ID, so the Homebrew dylib is rejected and dyld refuses to load it.

The fix is to keep the ad-hoc signature (so we retain a real bundle identity and `CFBundleIdentifier`) but strip the hardened runtime flag, relaxing library validation. Signature flags transition from `0x10002(adhoc,runtime)` to `0x2(adhoc)`.

## Working Solution

```bash
# From the frontend/ directory of the repo
cd /Users/azimov/Desktop/dev/cc-experiments/meetily/frontend

# 1. Build a proper .app bundle. The updater pubkey in tauri.conf.json
#    forces providing a matching private signing key at build time.
#    ~/.tauri/meetily-azimov.key matches the pubkey in tauri.conf.json (key ID 38BE547B5995DF1E).
TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.tauri/meetily-azimov.key)" \
TAURI_SIGNING_PRIVATE_KEY_PASSWORD="" \
pnpm exec tauri build --debug
# Watch for: "Finished" and a bundle path ending in meetily.app.

# 2. Re-sign the bundle WITHOUT hardened runtime so library validation
#    does not reject the Homebrew onnxruntime dylib (Team ID mismatch).
APP=/Users/azimov/Desktop/dev/cc-experiments/meetily/target/debug/bundle/macos/meetily.app
codesign --force --deep --sign - "$APP"
# Watch for: codesign -dv flags transition 0x10002(adhoc,runtime) -> 0x2(adhoc).

# 3. Launch via `open` so LaunchServices registers com.meetily.ai
#    as a real app identity (required for UNUserNotificationCenter delivery).
open "$APP"

# 4. On first launch, macOS prompts:
#    "Would meetily like to send notifications?" -> click Allow.
#    The app then appears in System Settings -> Notifications as "meetily".

# 5. In the running app, trigger:
#    About -> Debug Notifications -> Generic test notification
#    A top-right banner should appear with the Meetily icon.
```

Note: the Cargo workspace root is at the repo root, **not** inside `frontend/src-tauri/`, so the bundle path begins with `meetily/target/...` and not `meetily/frontend/src-tauri/target/...`.

## Verification

Check each failure mode independently:

- **Hardened runtime stripped.** `codesign -dv "$APP" 2>&1 | grep flags` must print `flags=0x2(adhoc)`. If it still shows `0x10002(adhoc,runtime)`, step 2 did not take and the app will fail to launch with a dyld library-validation error.
- **Bundle identity is live.** Rust logs on startup contain `Notification system initialized successfully` followed by `Updated system notification permission: true`, and System Settings → Notifications lists a dedicated "meetily" entry (not Terminal).
- **Plugin sees permission.** In DevTools (Cmd+Shift+I), run:
  ```js
  await window.__TAURI_INTERNALS__.invoke('get_notification_stats')
  ```
  The response must include `system_permission_granted: true`, `consent_given: true`, and both DND fields `false`.
- **End-to-end delivery.** About → Debug Notifications → "Generic test notification" produces a visible top-right banner showing the Meetily icon (not the Terminal.app icon, and not nothing).

## Prevention

The single highest-leverage fix is making the bundled debug build reproducible with one command. Add a helper script so nobody has to re-derive the build + re-sign + open sequence next time.

**`frontend/scripts/test-notifications.sh`** (preferred; keeps logic out of `package.json`):

```bash
#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

: "${TAURI_SIGNING_PRIVATE_KEY:?Set TAURI_SIGNING_PRIVATE_KEY (e.g. export TAURI_SIGNING_PRIVATE_KEY=\"\$(cat ~/.tauri/meetily-azimov.key)\")}"

# Cargo workspace root is above frontend/, not inside src-tauri/.
BUNDLE="../target/debug/bundle/macos/meetily.app"

echo "==> Building debug bundle..."
pnpm exec tauri build --debug

echo "==> Stripping hardened runtime (ad-hoc re-sign)..."
codesign --force --deep --sign - "$BUNDLE"

echo "==> Verifying signature flags..."
if codesign -dv "$BUNDLE" 2>&1 | grep -q "(adhoc,runtime)"; then
  echo "ERROR: hardened runtime still present — library validation will reject Homebrew dylibs." >&2
  exit 1
fi

echo "==> Launching..."
open "$BUNDLE"
```

Then in `frontend/package.json`:

```json
"scripts": {
  "test:notifications": "bash scripts/test-notifications.sh"
}
```

Now `pnpm test:notifications` is the entire loop. The `codesign -dv` guard catches the hardened-runtime regression immediately instead of at launch time with a cryptic dyld error.

**Config-level fix (not yet applied):** split macOS bundle config so `hardenedRuntime: false` only applies to `--debug` builds (via a `tauri.conf.debug.json` overlay invoked as `pnpm exec tauri build --config tauri.conf.debug.json --debug`). Do **not** disable hardened runtime globally — notarization of release builds *requires* hardened runtime; Apple's notary service rejects uploads without it.

**Apple Developer ID cert vs. ad-hoc:** a real Developer ID ($99/yr) signs with a valid Team ID, which lets hardened runtime coexist with third-party dylibs. For a team shipping signed releases this is worth it; for dev-only notification testing, ad-hoc re-signing is free and sufficient.

## Test Cases

1. **Init log check.** Launch the app with `RUST_LOG=info`; grep terminal output for `Notification system initialized successfully`. Absent = plugin didn't register; no further tests will pass.
2. **Debug dropdown trigger, one per type.** Open **About → Debug Notifications** and fire each variant. Expected: banner appears within ~1s *and* entry lands in Notification Center. Cross-check against default prefs — `show_recording_started` and `show_recording_stopped` default to `false`, so those two surface a bottom `toast.info` explaining suppression (from the pre-check handler) rather than firing. All other types default to `true`.
3. **`get_notification_stats` DevTools call.** Run `await window.__TAURI_INTERNALS__.invoke('get_notification_stats')` in DevTools. Expect `{ consent_given: true, system_permission_granted: true, manual_dnd_active: false, system_dnd_active: false, recording_notifications_enabled: true, meeting_reminders_enabled: true }`. `system_permission_granted: false` means the first-launch prompt was dismissed and the app needs its entry removed from System Settings → Notifications before retrying.

**Optional automated test:** a Rust unit test in `frontend/src-tauri/src/notifications/` that feeds each `DebugNotificationKind` variant into `debug_show_notification`'s dispatch match and asserts the correct `NotificationManager` method is called, using a trait-based mock. Low integration value (real bugs live in macOS permission/signing territory, not dispatch logic) but cheap insurance against enum drift.

## Gotchas to Watch For

- **First-launch permission prompt is one-shot per bundle ID.** If you dismissed it, open System Settings → Notifications → remove the "meetily" entry, then relaunch — there is no API to re-prompt.
- **Every rebuild re-applies hardened runtime.** The re-sign step is not sticky; the helper script above must run end-to-end each time after `tauri build --debug`.
- **Homebrew dylib upgrades invalidate library-load assumptions.** `brew upgrade onnxruntime` (or `whisper-cpp`, `ffmpeg`) can change the embedded dylib hashes or path; rebuild + re-sign after any `brew upgrade`.
- **`killall usernoted`** resets the notification daemon when banners stop appearing despite successful delivery. Not a Meetily bug, and easy to misattribute.
- **Scheduled Summary silently batches non-time-sensitive notifications.** System Settings → Notifications → Scheduled Summary. If "meetily" is in the list, banners queue for the next digest window and never appear live.
- **Focus modes** (Do Not Disturb, Work, custom) suppress banners without any log signal — check the top-right menu bar Control Center before concluding the app is broken.
- **"Deliver Quietly"** is a per-app setting macOS exposes via the subtle menu on any notification banner. Once enabled, delivery goes to Notification Center silently. Revert via the Alert Style panel in System Settings → Notifications → meetily.

## Phase 2: NS → UN migration (2026-04-20)

### New symptom after phase 1 was fixed

Once the signed bundle was working (phase 1 above), the **NC entries landed correctly but banners still didn't appear**. System Settings → Notifications → meetily showed Alert Style = Temporary (Banners), all toggles on. Flags in `~/Library/Preferences/com.apple.ncprefs.plist` differed from working apps (e.g. Mail, Zoom) in the high-bit banner-style flags — but toggling them via the UI had no effect. Distinct from phase 1: NC delivery works; only banner presentation fails.

### Root cause

`tauri-plugin-notification@2.3.1` → `notify-rust@4.12` → `mac-notification-sys@0.6.9` (`objc/notify.m` lines 34/42/134) calls `[NSUserNotificationCenter deliverNotification:]`. `NSUserNotification` was deprecated in macOS 10.14 (2018) and its banner path has been progressively neutered in every release since. On macOS 26 (Tahoe), delivery to NC still works but banner presentation never fires. Additionally, even if NS did work, it has no equivalent of `UNUserNotificationCenterDelegate::willPresentNotification:`, so foreground-app notifications would never show banners anyway — macOS requires that hook to return `.banner` / `.list` / `.sound` for a banner to appear while the sending app is frontmost.

Upstream fix is stalled: [mac-notification-sys PR #51](https://github.com/h4llow3En/mac-notification-sys/pull/51) migrating to the modern `UNUserNotificationCenter` API is a draft from Nov 2025; [tauri-apps/plugins-workspace RFC #2134](https://github.com/tauri-apps/plugins-workspace/issues/2134) has no implementation.

### Working solution

Bypass `tauri-plugin-notification` on macOS. Route notifications directly through `UNUserNotificationCenter` via `objc2-user-notifications`. Keep the plugin for Windows and Linux. Implementation in `frontend/src-tauri/src/notifications/macos_un.rs`; dispatch is `#[cfg(target_os = "macos")]`-gated in `frontend/src-tauri/src/notifications/system.rs`. See [docs/plans/2026-04-20-fix-macos-notification-banner-delivery-plan.md](../../plans/2026-04-20-fix-macos-notification-banner-delivery-plan.md) (gitignored — local only) for full rationale.

### Verification

- Build and re-sign the bundle using the phase 1 procedure above.
- Launch: `open .../target/debug/bundle/macos/meetily.app`.
- **First launch re-prompts for notification permission** — click Allow. NS and UN grants live in separate slots, so existing users will see a fresh prompt. If you previously denied, remove the meetily entry from System Settings → Notifications and relaunch.
- About → Debug Notifications → any variant → a top-right banner appears within ~1s with the Meetily icon, and an entry lands in Notification Center.
- Rust logs include `Installed UNUserNotificationCenterDelegate (willPresent → Banner|List|Sound)` once on first send, and `UN present: id=<uuid> title="…" level=<Active|TimeSensitive|...>` on every send.

### Gotchas specific to phase 2

- **Re-prompt is one-shot per bundle ID.** If denied, there is no API to re-prompt. Remove the "meetily" entry from System Settings → Notifications and relaunch.
- **`Critical` priority does not map to `UNNotificationInterruptionLevel::Critical`.** The Critical level requires Apple's Critical Alerts entitlement ($99 Developer Program + separate approval). We map Critical → TimeSensitive, which does not require the entitlement and still shows prominent banners.
- **Stale `/Applications/meetily.app` installs.** The prior debug-dropdown investigation found three LaunchServices registrations sharing bundle ID `com.meetily.ai` (production install, `target/debug` bundle, `target/release` bundle). ncprefs.plist binds notification preferences to the first-registered path. If banners still fail after phase 2, check `plutil -extract apps xml1 -o - ~/Library/Preferences/com.apple.ncprefs.plist | grep -B 2 -A 8 "com.meetily"` — the `path:` key should point to the bundle you're actually running.
- **`Retained<UNUserNotificationCenter>` is not `Send`.** Tauri commands require `Send` futures. `macos_un::show` must build the ObjC handles in a scope that drops before the `.await` on the oneshot. See the implementation for the pattern.

## Related

### Internal

- [macos-updater-missing-network-entitlement.md](./macos-updater-missing-network-entitlement.md) — Sibling macOS code-signing silent-failure case: hardened runtime + entitlements blocking a Rust-level feature that works in dev mode. Same "test from signed bundle, not `tauri dev`" lesson.
- [../../BUILDING.md](../../BUILDING.md) — Primary build guide. Currently documents `pnpm tauri:dev` / `pnpm tauri:build` without mentioning `TAURI_SIGNING_PRIVATE_KEY`, ad-hoc re-signing, or `libonnxruntime.dylib` library-validation quirks. Good candidate to update alongside this solution.
- [../../architecture.md](../../architecture.md) — Brief Tauri Core / Rust boundary overview. Context for why notifications require a native bundle, not the WebView.
- [../../brainstorms/2026-04-18-notification-debug-dropdown-brainstorm.md](../../brainstorms/2026-04-18-notification-debug-dropdown-brainstorm.md) — Origin discussion that surfaced the dev-mode notification failure.
- `docs/plans/2026-04-18-feat-notification-debug-dropdown-plan.md` — Implementation plan for the debug dropdown (path is gitignored; may not be checked into the repo).

### External

- [Tauri v2 — macOS Code Signing Guide](https://v2.tauri.app/distribute/sign/macos/) — Covers `TAURI_SIGNING_PRIVATE_KEY`, Developer ID signing, hardened runtime, and `tauri build --debug` workflow.
- [Tauri Notification Plugin](https://v2.tauri.app/plugin/notification/) — Why notifications require a bundled `.app` with a valid bundle identifier.
- [Apple — Hardened Runtime Entitlements](https://developer.apple.com/documentation/security/hardened_runtime) — Library validation, which rejects Homebrew dylibs under ad-hoc signatures.
- [Apple `codesign(1)` — Code Signing Guide](https://developer.apple.com/library/archive/documentation/Security/Conceptual/CodeSigningGuide/Procedures/Procedures.html) — Reference for `codesign --force --deep --sign -` ad-hoc re-signing semantics.
