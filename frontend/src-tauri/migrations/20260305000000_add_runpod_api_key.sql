-- Add dedicated RunPod API key column to transcript_settings
ALTER TABLE transcript_settings ADD COLUMN runpodApiKey TEXT;
