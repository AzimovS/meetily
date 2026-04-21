---
title: "Remote Transcription Silently Drops Chunks on Transient Failures; Toast-Only Signalling Hides Where"
date: 2026-04-21
category: runtime-errors
tags:
  - transcription
  - remote-provider
  - http-retry
  - exponential-backoff
  - tauri-events
  - error-classification
  - one-shot-toast
  - ffi-constants
  - silent-failure
  - user-feedback
module: frontend/src-tauri/src/audio/transcription
symptom: "Mid-meeting gaps in the live transcript with no user signal; occasional 'Transcription failed for an audio segment' toasts with no indication of which segment; bad API keys produced a silent stream of identical toasts with no recovery guidance"
root_cause: "RemoteProvider::transcribe made a single .send().await per chunk with no retry and no error classification; any transient 5xx, network blip, or rate-limit dropped that chunk's audio. Failures surfaced as transient toast notifications disconnected from the transcript location, and auth failures (401/403) were indistinguishable from other failure modes."
severity: high
---

# Remote Transcription Silently Drops Chunks on Transient Failures

## Problem

Users on the remote-transcription provider (OpenAI-compatible STT endpoints) saw:

1. **Intermittent gaps in the live transcript** with no indication anything was wrong — a 503 from Groq, a flaky Wi-Fi moment, or a brief upstream rate-limit silently dropped that chunk's audio. The transcript looked identical to silence at that position.
2. **Per-chunk toasts reading "Transcription failed for an audio segment"** with no indication of *which* segment — users could tell the system was struggling but not where the damage was.
3. **Bad API keys producing a toast flood** — every chunk generated the same generic toast with no guidance that the fix was in Transcription Settings.
4. **LLM summaries hallucinating around gaps** — when placeholder markers were eventually added but not filtered from the summary input, the summarizer rationalized "failed chunk" as speech content.

## Investigation

1. Grep'd the remote client for retry logic: a single `.send().await` at `remote_provider.rs:138`. No retry, no classification. Confirmed bug.
2. Compared to sibling `ollama/ollama.rs:131-159` which *does* have retry — 3 attempts, 300 ms exponential backoff, classify-by-string. Pattern exists in the codebase, just not applied here.
3. Traced error surfacing: `worker.rs` emitted `transcription-warning` → `useModalState.ts` → Sonner toast. Toast was timestamp-free.
4. Confirmed 401/403 was lumped with every other `EngineFailed(String)` — no way for the worker to route it distinctly.
5. Reviewed prior deferred scope: `docs/plans/2026-04-17-fix-remote-transcription-timeout-plan.md` explicitly listed "no retry on transient failure" and "auth failures surface as generic message" as known limitations to revisit. This work is that follow-up.

## Root Cause

Three root causes stacked:

1. **No error classification at the HTTP boundary.** Every non-2xx response and every transport error was mapped to the same `EngineFailed(String)`, so upstream had no way to decide "retry" vs "fail fast" vs "tell the user their key is wrong."
2. **Failure signal was detached from location.** Toasts are ephemeral and the user couldn't correlate them to transcript positions. The *transcript itself* had no indication that a chunk ever existed at that timestamp.
3. **String-based placeholder shared across Rust+TS without a constant.** Once we added a `failed chunk` marker to surface failures inline, the literal had to be duplicated at the emission site (Rust worker) and the filter site (TS summary input). A rename on one side would silently break the filter on the other — summaries would ingest "failed chunk" as speech.

## Why It Was Hard to Find

1. **Low-frequency transient**: the rate of recoverable 5xx/429 is low enough that most meetings work fine. Failures looked like "I guess I didn't say that clearly" until users noticed patterns.
2. **Toast UI makes correlation impossible**: you can't scroll back to "where was I in the meeting when that toast fired?"
3. **Tauri-event indirection**: the signal path is Rust `emit` → frontend `listen` → Sonner. Debugging why a signal does or doesn't surface requires following the event name across the FFI boundary.
4. **Auth-failure looked like any other failure**: a user with a bad API key had no prompt saying "check your key." They saw a generic toast and assumed the service was flaky.

## Solution

Four coordinated changes in `frontend/src-tauri/src/audio/transcription/`:

### 1. Retry with explicit error classification

`remote_provider.rs` wraps the HTTP call in a 3-attempt loop with 300/600 ms exponential backoff. A `RetryVerdict` enum classifies each failure:

```rust
enum RetryVerdict {
    Retry(String),     // transport errors, HTTP 5xx
    Terminal(String),  // HTTP 4xx (not auth), JSON parse, body read
    Auth(String),      // HTTP 401, 403
}
```

429 is **terminal, not retried** — it means "slow down"; replying with two more requests in a second is the wrong behavior. 4xx means the request itself is wrong (bad format, auth, not-found) and retry can't fix it.

### 2. Typed auth variant

Added `TranscriptionError::AuthFailed(String)` to the shared trait error enum. Lets the worker branch on 401/403 specifically without string-matching on error messages.

### 3. Inline placeholder instead of toast

`worker.rs` converts `EngineFailed | UnsupportedLanguage | AuthFailed` into a regular `TranscriptUpdate` with `text = FAILED_CHUNK_PLACEHOLDER`, preserving the chunk's `speaker`, `audio_start_time`, `audio_end_time`, and `duration`. The failure appears inline at its timestamp in the live transcript, the recording file, and the final stored transcript. No toast.

The placeholder does **not** trigger the first-speech-detected signal or the echo deduplicator — it isn't spoken content.

### 4. One-shot actionable toast for auth failures

`AUTH_ERROR_EMITTED: AtomicBool` (mirroring the existing `MODEL_UNLOADED_EMITTED` pattern) gates a single `transcription-error` event per session with message:
> *"Remote transcription rejected your credentials. Check your API key in Transcription settings."*

Reset alongside other per-session flags in `reset_speech_detected_flag()`. Placeholder still emits on every chunk; toast fires exactly once.

### 5. Twin constants across FFI

To keep the Rust emission site and TS filter site from drifting:

```rust
// frontend/src-tauri/src/audio/transcription/constants.rs
pub const FAILED_CHUNK_PLACEHOLDER: &str = "failed chunk";
```

```typescript
// frontend/src/constants/transcriptPlaceholders.ts
export const FAILED_CHUNK_PLACEHOLDER = 'failed chunk' as const;
export const isFailedChunkPlaceholder = (t: { text: string }): boolean =>
  t.text === FAILED_CHUNK_PLACEHOLDER;
```

Both files carry "Keep in sync with …" comments pointing at each other, following the precedent set by `audio/constants.rs` ↔ `constants/audioFormats.ts`.

Filter applied in `useSummaryGeneration.ts` before the summary text is assembled, so the LLM never sees the placeholder.

## Verification

Manual test matrix:

| Scenario | Expected |
|---|---|
| Healthy endpoint | No regressions, no `failed chunk` entries |
| Unreachable URL | Retry × 3 logs, inline `failed chunk` per chunk, no toast |
| Transient 5xx that recovers on attempt 2 | Log: "succeeded on attempt 2/3", normal transcript |
| Persistent 429 | Single attempt per chunk (no retry), inline marker, no toast spam |
| HTTP 401 (bad API key) | Exactly one toast pointing at settings + inline markers per chunk |
| Malformed JSON on 200 | Single attempt (terminal), inline marker |
| Summary over a meeting with failures | LLM input contains zero `failed chunk` entries |

Build verification:

```bash
cd frontend/src-tauri && cargo check   # passes, no new warnings
cd frontend && npx tsc --noEmit         # passes
```

## Prevention

### Design principles

1. **Classify before retrying.** An HTTP client must split failures into transport / 5xx / 429 / 4xx-auth / 4xx-terminal / body-parse before deciding. One blanket `.send().await?` is the bug. See `RetryVerdict` in `remote_provider.rs`.
2. **Every chunk that enters the pipeline exits with a visible outcome.** Success emits text; failure emits a placeholder at the same timestamp. Never drop a chunk into `continue;` silently — the worker's `chunks_queued == chunks_completed` invariant is the accounting backstop, not the UX.
3. **Toasts are one-shot and actionable; the stream is the log.** Use a session-scoped `AtomicBool` so a persistent failure produces one toast plus N inline markers — not N toasts or zero.
4. **Bounded retries with exponential backoff live at the boundary.** Constants (`MAX_ATTEMPTS`, `INITIAL_BACKOFF_MS`) sit next to the call. Do not let a retry loop wrap another retry loop upstream.
5. **Cross-FFI string contracts get twin named constants.** A magic string duplicated in Rust and TypeScript is a silent rename-drift bomb. Define once on each side with a "Keep in sync with …" comment.

### Review checklist

- Does every non-2xx branch have an explicit classification (retry / terminal / auth)?
- Is 429 handled distinctly from other 4xx (no retry storm)?
- Are transport errors retried, and body-parse errors not?
- Is there a bounded attempt count with backoff, not a `while true`?
- On exhaustion, does the caller see a structured error variant (not a `String`)?
- Is there a one-shot user-visible signal for auth/config failures, gated by an `AtomicBool`?
- Are secrets (API keys, tokens in URLs) stripped from logs and error messages?
- Is the response body truncated before being logged?

### Test scenarios (scriptable via mock server)

- Unreachable endpoint (connect refused) → retried, eventual `EngineFailed`.
- 5xx on attempt 1, 200 on attempt 2 → success, single transcript emitted.
- Persistent 429 → treated as terminal (no retry storm).
- 401 / 403 → no retry, `AuthFailed` variant, one-shot toast fires exactly once across many chunks.
- Malformed JSON on 200 → terminal.
- Connection reset mid-body-read → terminal.

Manual only: real provider outage mid-meeting (verify placeholder timestamps, LLM summary excludes them); wrong API key entered live in settings (verify single toast, not per-chunk spam).

### Anti-patterns to flag in review

- `.send().await?` that propagates a raw `reqwest::Error` — collapses every failure mode into one.
- Error surfaced only as a toast string with no corresponding in-stream marker.
- Magic placeholder strings duplicated across FFI without constants.
- `loop { … sleep; }` without a `MAX_ATTEMPTS` guard, or retrying inside a function that is itself already retried by its caller.
- Logging `reqwest::Error` directly when the endpoint URL may contain secrets (`e.without_url()` + a pre-sanitized URL is the fix).

## Related

- PR: [#39 feat(transcription): retry with backoff + in-transcript failure placeholder](https://github.com/AzimovS/meetily/pull/39)
- Prior plan that introduced the toast-warning path (now superseded): [docs/plans/2026-04-08-feat-surface-silent-transcription-failures-plan.md](../../plans/2026-04-08-feat-surface-silent-transcription-failures-plan.md)
- Prior plan that bumped timeouts and explicitly deferred retry: [docs/plans/2026-04-17-fix-remote-transcription-timeout-plan.md](../../plans/2026-04-17-fix-remote-transcription-timeout-plan.md)
- Current branch plan: [docs/plans/2026-04-21-feat-remote-transcription-retry-and-in-transcript-failure-placeholder-plan.md](../../plans/2026-04-21-feat-remote-transcription-retry-and-in-transcript-failure-placeholder-plan.md)
- Sibling retry implementation (undocumented, worth unifying eventually): `frontend/src-tauri/src/ollama/ollama.rs:131-159`
- Twin-constant precedent: `frontend/src-tauri/src/audio/constants.rs` ↔ `frontend/src/constants/audioFormats.ts`
- Commits: `13f7bcc` (retry + placeholder), `e60952a` (429 + filter + auth toast), `4498556` (twin constants)

### Known follow-ups (not in this solution)

- **Unbounded mpsc channel + serial worker**: one stuck chunk (worst case 40 s × 3 + 0.9 s backoff ≈ 121 s) stalls the queue and accumulates audio buffers. Pre-existing, now amplified by retry. Worth a dedicated PR with a bounded channel + stale-chunk drop policy.
- **`kind` field on `TranscriptUpdate`**: currently placeholder identity is text-equality. Structural field (`kind: "speech" | "failed_chunk"`) would let consumers filter without string-matching and is more robust against future placeholder types.
- **Retry pattern unification**: `ollama.rs` and `remote_provider.rs` both implement retry with similar shape but divergent constant names and different error-classification approaches. Extract a shared helper when a third retry site lands.
