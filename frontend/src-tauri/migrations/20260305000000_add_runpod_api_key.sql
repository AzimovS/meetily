-- Add dedicated RunPod API key column to transcript_settings
-- Idempotent via table recreation to handle cases where column may already exist
CREATE TABLE transcript_settings_new (
    id TEXT PRIMARY KEY,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    whisperApiKey TEXT,
    deepgramApiKey TEXT,
    elevenLabsApiKey TEXT,
    groqApiKey TEXT,
    openaiApiKey TEXT,
    runpodApiKey TEXT
);

INSERT INTO transcript_settings_new (id, provider, model, whisperApiKey, deepgramApiKey, elevenLabsApiKey, groqApiKey, openaiApiKey)
    SELECT id, provider, model, whisperApiKey, deepgramApiKey, elevenLabsApiKey, groqApiKey, openaiApiKey
    FROM transcript_settings;

DROP TABLE transcript_settings;

ALTER TABLE transcript_settings_new RENAME TO transcript_settings;
