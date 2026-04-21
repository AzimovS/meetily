/**
 * Placeholder text emitted by the Rust transcription worker for chunks that
 * failed to transcribe. Appears inline in the live transcript as a visible
 * failure marker, and must be filtered out of summary LLM input so the model
 * doesn't treat "failed chunk" as speech content.
 *
 * IMPORTANT: Keep in sync with Rust constant in
 * src-tauri/src/audio/transcription/constants.rs
 */
export const FAILED_CHUNK_PLACEHOLDER = 'failed chunk' as const;

/**
 * True when the transcript entry is a FAILED_CHUNK_PLACEHOLDER marker
 * rather than real speech. Prefer this over inline string comparison so
 * the source of truth stays greppable.
 */
export const isFailedChunkPlaceholder = (t: { text: string }): boolean =>
  t.text === FAILED_CHUNK_PLACEHOLDER;
