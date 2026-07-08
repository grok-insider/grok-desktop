use async_trait::async_trait;

use grok_application::IsolationProbeError;

/// Hard host JSON Lines frame limit mirrored from the service.
pub(crate) const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

#[async_trait]
pub(crate) trait ProbeTransport: Send + Sync {
    async fn exchange(
        &self,
        request: &[u8],
        maximum_response_bytes: usize,
    ) -> Result<Vec<u8>, IsolationProbeError>;
}
