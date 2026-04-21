use anyhow::Result;

use crate::detection::signals::SignalSampler;
use crate::detection::types::MicSnapshot;

pub struct StubMicActivitySampler;

impl SignalSampler for StubMicActivitySampler {
    fn snapshot(&self) -> Result<MicSnapshot> {
        Ok(MicSnapshot::default())
    }
}
