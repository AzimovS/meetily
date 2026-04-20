# Notification Debug Dropdown in About Section

**Date:** 2026-04-18
**Status:** Brainstorm

## Problem / Motivation

Meetily already emits OS-level notifications (recording started/stopped/paused/resumed, transcription complete, system error, meeting reminder) via `tauri-plugin-notification`. There is no way to trigger these on demand from the UI, which makes it hard to:

- Verify the OS notification channel works (permissions, DND, system settings)
- See what each notification looks like without starting a real meeting
- Reproduce notification-related bugs deterministically
- Confirm the per-type consent toggles in Preferences actually suppress the right notifications

Test commands already exist in Rust (`show_test_notification`, `test_notification_with_auto_consent`) but are not exposed anywhere in the UI. Only the generic "Test" notification is reachable today, and only via dev console.

## What We're Building

A **debug dropdown** placed next to the existing "Debug Updater" button at the top of the About section (`frontend/src/components/About.tsx`). When opened, it lists each notification type as a menu item. Clicking an item fires that exact notification through the real code path used by the app.

### Notification types exposed in the dropdown

Matches `NotificationType` in `frontend/src-tauri/src/notifications/types.rs`:

1. Recording started (with a fake meeting name like "Debug Meeting")
2. Recording stopped
3. Recording paused
4. Recording resumed
5. Transcription complete (with a fake file path)
6. Meeting reminder (e.g., 5 minutes)
7. System error (with a fake error message)
8. Generic test notification

### User-facing behavior

- Button label: "Debug Notifications" with a chevron, styled like "Debug Updater" (ghost variant, xs text)
- Dropdown opens on click; each item triggers one notification
- Respects the user's consent toggle and per-type preferences in Settings → Preferences
- If consent is off or a given type is disabled, the notification is silently suppressed — this is itself useful information, and a small `toast.info` informs the developer why nothing appeared
- No new permissions prompts; assumes the user has already granted notification permission through normal app flow

## Why This Approach

**Reuse the real code paths.** Each menu item calls the same internal helper used by the actual app (`show_recording_started_notification`, `show_recording_stopped_notification`, etc. in `notifications/commands.rs:286+`). This means:

- The debug button tests the thing users actually see, not a parallel implementation
- Consent and DND gating are exercised, not bypassed
- No duplicate notification content lives on the frontend

**Minimal Rust surface.** Today only two Tauri commands expose test notifications, and only for the generic "Test" type. We add one thin command per notification type (or a single command that takes an enum) that wraps the existing internal helpers. No new business logic.

**Placement matches existing pattern.** The About section already hosts "Debug Updater" for diagnosing the auto-updater. Adding "Debug Notifications" next to it creates a consistent "debug corner" rather than a new section.

## Key Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Scope | Dropdown of every notification type | Catches per-type bugs and consent-toggle bugs, not just "does the OS channel work?" |
| Consent behavior | Respect user consent | Matches real app behavior; suppression is useful debug signal |
| Placement | Next to "Debug Updater" in About header | Consistent with existing debug UI pattern |
| Backend approach | Add thin Tauri command(s) that call existing internal helpers | Reuses real code paths; avoids duplicating notification content on frontend |
| UI component | `DropdownMenu` from `components/ui/dropdown-menu.tsx` | Already in the codebase, consistent with other menus |
| Visibility | Always visible in About | Low risk (About is already a quiet page); matches "Debug Updater" which is always visible too. Can revisit if About gets cluttered. |

## Out of Scope (YAGNI)

- **Bypass-consent mode.** `test_notification_with_auto_consent` exists for extreme cases but would only confuse normal debugging flow. Skipped.
- **"Fire all in sequence" button.** Noisy and the dropdown already covers each case individually.
- **Dev-only gating.** About is a low-traffic page; hiding behind a flag adds complexity without clear benefit. Revisit if UX pushes back.
- **Notification inspection panel** (showing current consent state, DND status, permission status). Could be valuable but a separate feature.

## Open Questions

None — approach is clear enough to plan.

## Relevant Files

- `frontend/src/components/About.tsx` — where the new button lives (existing "Debug Updater" at lines 91–106 is the pattern to follow)
- `frontend/src-tauri/src/notifications/commands.rs` — where new Tauri debug commands will be added (internal helpers at lines 286–459 already exist)
- `frontend/src-tauri/src/notifications/types.rs` — enum of notification types
- `frontend/src-tauri/src/lib.rs:704,712` — where new commands will be registered in `invoke_handler`
- `frontend/src/components/ui/dropdown-menu.tsx` — dropdown component to use
- `frontend/src-tauri/NOTIFICATION_TESTING.md` — existing docs; may need an update noting the new UI button
