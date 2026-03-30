-- Add endpointUrl column to transcript_settings table.
-- For 'remote' provider, the model column currently stores the endpoint URL.
-- This migration adds a dedicated endpointUrl column and copies the URL there,
-- while keeping the URL in model as a backward-compatible fallback.

PRAGMA defer_foreign_keys = ON;

CREATE TABLE IF NOT EXISTS transcript_settings_new (
    id TEXT PRIMARY KEY,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    endpointUrl TEXT,
    whisperApiKey TEXT,
    deepgramApiKey TEXT,
    elevenLabsApiKey TEXT,
    groqApiKey TEXT,
    openaiApiKey TEXT,
    runpodApiKey TEXT
);

-- Plain INSERT (no OR IGNORE) -- the target table is empty so no PK conflicts.
-- For remote provider: copy URL to endpointUrl AND keep it in model (two-phase safety).
-- For other providers: copy as-is, endpointUrl stays NULL.
INSERT INTO transcript_settings_new (id, provider, model, endpointUrl, whisperApiKey, deepgramApiKey, elevenLabsApiKey, groqApiKey, openaiApiKey, runpodApiKey)
SELECT
    id,
    provider,
    model,
    CASE WHEN provider = 'remote' THEN model ELSE NULL END,
    whisperApiKey,
    deepgramApiKey,
    elevenLabsApiKey,
    groqApiKey,
    openaiApiKey,
    runpodApiKey
FROM transcript_settings;

DROP TABLE transcript_settings;

ALTER TABLE transcript_settings_new RENAME TO transcript_settings;
