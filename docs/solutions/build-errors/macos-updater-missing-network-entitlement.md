---
title: "Tauri Auto-Updater Silently Fails After Enabling macOS Notarization"
date: 2026-03-23
category: build-errors
tags:
  - tauri
  - macos
  - notarization
  - hardened-runtime
  - entitlements
  - auto-updater
  - silent-failure
  - codesigning
module: frontend/src-tauri
symptom: "App reports 'You are running the latest version' despite newer version available on GitHub Releases"
root_cause: "Missing com.apple.security.network.client entitlement causes hardened runtime to block outgoing HTTP requests from Rust binary, silently breaking Tauri updater plugin"
severity: critical
---

# Tauri Auto-Updater Silently Fails After Enabling macOS Notarization

## Problem

After enabling Developer ID signing and notarization for v0.1.3, the Tauri 2.x auto-updater silently stopped working. The app displayed "You are running the latest version" even when a newer release existed on GitHub. No error was shown to the user, no error appeared in logs, and the app otherwise functioned normally.

## Investigation

1. Verified `latest.json` endpoint: HTTP 200, correct version, valid platform/signature/asset -- server side is fine
2. Verified installed app is v0.1.3 via `Info.plist CFBundleShortVersionString`
3. Checked updater config (`tauri.conf.json`): endpoint correct, pubkey present
4. Cleared `~/Library/WebKit/com.meetily.ai` and `~/Library/Caches/com.meetily.ai` -- didn't help
5. Investigated dev/prod shared state (same `com.meetily.ai` identifier) -- red herring
6. Checked `entitlements.plist` -- **missing `com.apple.security.network.client`**
7. Compared signing across versions: v0.1.1/v0.1.2 were ad-hoc signed (hardened runtime not enforced), v0.1.3+ was Developer ID signed (hardened runtime enforced)

## Root Cause

macOS **hardened runtime** blocks all outgoing network requests from native code unless `com.apple.security.network.client` is declared in `entitlements.plist`. When the app moved from ad-hoc signing to Developer ID signing for notarization, hardened runtime became enforced. The Tauri updater plugin makes HTTP requests from Rust (via `reqwest`), not from the WebView, so `check()` silently fails.

| Version | Signing | Hardened Runtime Enforced | Updater |
|---------|---------|--------------------------|---------|
| v0.1.1 | Ad-hoc | No | Works |
| v0.1.2 | Ad-hoc | No | Works |
| v0.1.3 | Developer ID + notarized | **Yes** | **Broken** |
| v0.1.4 | Developer ID + notarized | **Yes** | **Broken** |
| v0.1.5 | Developer ID + notarized | Yes | **Fixed** |

## Why It Was Hard to Find

1. **Silent failure**: `check()` catches the network error internally and returns "no update available" instead of propagating the error
2. **Dev mode unaffected**: `tauri dev` does not enforce hardened runtime
3. **WebView works fine**: Frontend HTTP requests go through WebKit, not the Rust binary -- only Rust-level networking is blocked
4. **Endpoint is healthy**: `curl` returns correct data, ruling out server-side issues
5. **Ad-hoc signing worked**: Previous versions with ad-hoc signing had no restrictions

## Solution

Add `com.apple.security.network.client` to `entitlements.plist`:

```xml
<key>com.apple.security.network.client</key>
<true/>
```

This entitlement permits outgoing network connections from the native binary, which the Tauri updater, any `reqwest` HTTP calls, and Rust-level API clients all require.

## Verification

After rebuilding with the fix:

```bash
# Verify entitlement is present in the signed binary
codesign -d --entitlements - /Applications/meetily.app 2>&1 | grep "network.client"
```

Always test from the **installed DMG**, not from `tauri dev`.

## Prevention

### Checklist when enabling/modifying notarization

- [ ] Audit all Rust-level network calls (`reqwest`, `hyper`, `tauri-plugin-updater`, `tauri-plugin-http`)
- [ ] Ensure `com.apple.security.network.client` is in `entitlements.plist`
- [ ] Test the updater from the **signed DMG**, never from dev mode
- [ ] Treat `entitlements.plist` as security-critical config -- review in PRs

### CI validation (add to release workflow)

```yaml
- name: Verify required entitlements
  if: matrix.platform == 'macos-latest'
  run: |
    if ! grep -q "com.apple.security.network.client" frontend/src-tauri/entitlements.plist; then
      echo "ERROR: Missing network.client entitlement"
      exit 1
    fi
```

### Key principle

**WebView vs native binary is a critical distinction on macOS.** Features that work in the WebView portion of a Tauri app may silently break in the Rust portion under hardened runtime. Always test both paths after signing changes.

## Related

- [Tauri issue #13878: macOS Production Build Network Requests Blocked](https://github.com/tauri-apps/tauri/issues/13878)
- [Tauri docs issue #3171: Mandatory network.client entitlement](https://github.com/tauri-apps/tauri-docs/issues/3171)
- PR: https://github.com/AzimovS/meetily/pull/11
- Commit: `4df02fe` fix: add network entitlement for macOS updater and bump to v0.1.5
