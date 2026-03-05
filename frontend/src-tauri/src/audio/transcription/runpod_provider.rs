// audio/transcription/runpod_provider.rs
//
// RunPod remote transcription provider. Sends audio chunks to a RunPod
// serverless endpoint running faster-whisper and returns the transcription.

use super::provider::{TranscriptionError, TranscriptionProvider, TranscriptResult};
use async_trait::async_trait;
use log::{info, warn};

/// RunPod serverless transcription provider
pub struct RunPodProvider {
    endpoint_id: String,
    api_key: String,
    client: reqwest::Client,
}

impl RunPodProvider {
    pub fn new(endpoint_id: String, api_key: String) -> Result<Self, String> {
        // Validate endpoint ID contains only safe characters (alphanumeric, hyphens, underscores)
        if endpoint_id.is_empty() || !endpoint_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(format!("Invalid RunPod endpoint ID: must be alphanumeric (hyphens/underscores allowed), got '{}'", endpoint_id));
        }
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        info!("RunPod provider initialized for endpoint: {}", endpoint_id);
        Ok(Self {
            endpoint_id,
            api_key,
            client,
        })
    }

    /// Encode f32 audio samples as a 16kHz mono 16-bit PCM WAV, then base64-encode it.
    fn encode_audio_base64(audio: &[f32]) -> Result<String, TranscriptionError> {
        use base64::{engine::general_purpose::STANDARD, Engine};

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

        Ok(STANDARD.encode(&wav))
    }
}

#[async_trait]
impl TranscriptionProvider for RunPodProvider {
    async fn transcribe(
        &self,
        audio: Vec<f32>,
        language: Option<String>,
    ) -> std::result::Result<TranscriptResult, TranscriptionError> {
        let audio_base64 = Self::encode_audio_base64(&audio)?;

        let url = format!(
            "https://api.runpod.ai/v2/{}/runsync",
            self.endpoint_id
        );

        let lang = language.unwrap_or_else(|| "en".to_string());

        let body = serde_json::json!({
            "input": {
                "audio_base64": audio_base64,
                "language": lang,
                "transcription": "plain_text",
                "enable_vad": true
            }
        });

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                warn!("RunPod request failed: {}", e);
                TranscriptionError::EngineFailed(format!("RunPod request failed: {}", e))
            })?;

        let status = response.status();
        let response_text = response.text().await.map_err(|e| {
            TranscriptionError::EngineFailed(format!("Failed to read RunPod response: {}", e))
        })?;

        if !status.is_success() {
            warn!("RunPod returned HTTP {}: {}", status, response_text);
            return Err(TranscriptionError::EngineFailed(format!(
                "RunPod returned HTTP {}",
                status
            )));
        }

        let json: serde_json::Value = serde_json::from_str(&response_text).map_err(|e| {
            TranscriptionError::EngineFailed(format!("Invalid RunPod response JSON: {}", e))
        })?;

        let runpod_status = json["status"].as_str().unwrap_or("UNKNOWN");
        if runpod_status != "COMPLETED" {
            warn!("RunPod job status: {}", runpod_status);
            return Err(TranscriptionError::EngineFailed(format!(
                "RunPod job status: {}",
                runpod_status
            )));
        }

        let text = json["output"]["transcription"]
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
        Some(format!("runpod:{}", self.endpoint_id))
    }

    fn provider_name(&self) -> &'static str {
        "RunPod"
    }
}
