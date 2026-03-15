-- Add dedicated remote transcription API key column to transcript_settings
ALTER TABLE transcript_settings ADD COLUMN remoteApiKey TEXT;
