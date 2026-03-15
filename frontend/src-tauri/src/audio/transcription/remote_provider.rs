// audio/transcription/remote_provider.rs
//
// Remote transcription provider. Sends audio as a WAV file via
// multipart form upload to an OpenAI-compatible endpoint and returns
// the transcription.

use super::provider::{TranscriptionError, TranscriptionProvider, TranscriptResult};
use async_trait::async_trait;
use log::{info, warn};

/// Remote transcription provider
pub struct RemoteProvider {
    url: String,
    api_key: String,
    client: reqwest::Client,
}

impl RemoteProvider {
    pub fn new(url: String, api_key: String) -> Result<Self, String> {
        if url.is_empty() {
            return Err("Remote transcription URL not configured".to_string());
        }
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap_or_default();
        info!("Remote transcription provider initialized for URL: {}", url);
        Ok(Self {
            url,
            api_key,
            client,
        })
    }

    /// Encode f32 audio samples as a 16kHz mono 16-bit PCM WAV.
    fn encode_audio_wav(audio: &[f32]) -> Result<Vec<u8>, TranscriptionError> {
        let num_samples = audio.len();
        let data_size = num_samples * 2; // 16-bit = 2 bytes per sample
        let file_size = 36 + data_size;

        let mut wav = Vec::with_capacity(44 + data_size);

        // RIFF header
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(file_size as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVE");

        // fmt chunk
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes()); // chunk size
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM format
        wav.extend_from_slice(&1u16.to_le_bytes()); // mono
        wav.extend_from_slice(&16000u32.to_le_bytes()); // sample rate
        wav.extend_from_slice(&32000u32.to_le_bytes()); // byte rate (16000 * 2)
        wav.extend_from_slice(&2u16.to_le_bytes()); // block align
        wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample

        // data chunk
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data_size as u32).to_le_bytes());

        // Convert f32 [-1.0, 1.0] to i16
        for &sample in audio {
            let clamped = sample.clamp(-1.0, 1.0);
            let i16_val = (clamped * 32767.0) as i16;
            wav.extend_from_slice(&i16_val.to_le_bytes());
        }

        Ok(wav)
    }
}

#[async_trait]
impl TranscriptionProvider for RemoteProvider {
    async fn transcribe(
        &self,
        audio: Vec<f32>,
        _language: Option<String>,
    ) -> std::result::Result<TranscriptResult, TranscriptionError> {
        let wav_bytes = Self::encode_audio_wav(&audio)?;

        let file_part = reqwest::multipart::Part::bytes(wav_bytes)
            .file_name("audio.wav")
            .mime_str("audio/wav")
            .map_err(|e| {
                TranscriptionError::EngineFailed(format!("Failed to build multipart: {}", e))
            })?;

        let form = reqwest::multipart::Form::new()
            .part("file", file_part);

        let response = self
            .client
            .post(&self.url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .multipart(form)
            .send()
            .await
            .map_err(|e| {
                warn!("Remote transcription request failed: {}", e);
                TranscriptionError::EngineFailed(format!("Remote transcription request failed: {}", e))
            })?;

        let status = response.status();
        let response_text = response.text().await.map_err(|e| {
            TranscriptionError::EngineFailed(format!("Failed to read remote transcription response: {}", e))
        })?;

        if !status.is_success() {
            warn!("Remote transcription returned HTTP {}: {}", status, response_text);
            return Err(TranscriptionError::EngineFailed(format!(
                "Remote transcription returned HTTP {}",
                status
            )));
        }

        let json: serde_json::Value = serde_json::from_str(&response_text).map_err(|e| {
            TranscriptionError::EngineFailed(format!("Invalid remote transcription response JSON: {}", e))
        })?;

        let text = json["text"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();

        Ok(TranscriptResult {
            text,
            confidence: None,
            is_partial: false,
        })
    }

    async fn is_model_loaded(&self) -> bool {
        true // Model lives on the server
    }

    async fn get_current_model(&self) -> Option<String> {
        Some("remote".to_string())
    }

    fn provider_name(&self) -> &'static str {
        "Remote"
    }
}
