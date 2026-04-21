// audio/transcription/constants.rs
//
// Constants shared across the transcription module and — where noted — across
// the FFI boundary with the frontend.

/// Placeholder text emitted by the transcription worker for chunks that
/// failed to transcribe. Rendered inline in the live transcript, persisted
/// via `RecordingSaver`, and filtered out of summary LLM input on the
/// frontend.
///
/// IMPORTANT: Keep in sync with `FAILED_CHUNK_PLACEHOLDER` in
/// `frontend/src/constants/transcriptPlaceholders.ts`. Any future consumer
/// of persisted transcripts that needs to distinguish real speech from
/// failure markers must compare against this constant.
pub const FAILED_CHUNK_PLACEHOLDER: &str = "failed chunk";
