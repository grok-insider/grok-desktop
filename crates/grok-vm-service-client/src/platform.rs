use std::sync::Arc;

#[cfg(not(windows))]
use async_trait::async_trait;
#[cfg(not(windows))]
use grok_application::IsolationProbeError;

use crate::transport::ProbeTransport;

pub(crate) fn transport() -> Arc<dyn ProbeTransport> {
    #[cfg(windows)]
    {
        Arc::new(windows::WindowsNamedPipeTransport::new())
    }
    #[cfg(not(windows))]
    {
        Arc::new(UnavailablePlatformTransport)
    }
}

#[cfg(not(windows))]
struct UnavailablePlatformTransport;

#[cfg(not(windows))]
#[async_trait]
impl ProbeTransport for UnavailablePlatformTransport {
    async fn exchange(
        &self,
        _request: &[u8],
        _maximum_response_bytes: usize,
    ) -> Result<Vec<u8>, IsolationProbeError> {
        Err(IsolationProbeError::Unavailable)
    }
}

#[cfg(windows)]
mod windows {
    use std::{io, sync::Arc, time::Duration};

    use async_trait::async_trait;
    use grok_application::IsolationProbeError;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::windows::named_pipe::{ClientOptions, NamedPipeClient},
        sync::Semaphore,
        task,
        time::{Instant, sleep, timeout},
    };
    use windows_sys::Win32::{
        Foundation::ERROR_PIPE_BUSY, Storage::FileSystem::SECURITY_IDENTIFICATION,
    };

    use crate::transport::ProbeTransport;

    /// Fixed, versioned production endpoint. Callers cannot select another broker.
    const SERVICE_PIPE_NAME: &str = r"\\.\pipe\GrokDesktop.VMService.v1";
    const EXCHANGE_TIMEOUT: Duration = Duration::from_millis(2_500);
    const CONNECT_RETRY_DELAY: Duration = Duration::from_millis(50);

    pub(super) struct WindowsNamedPipeTransport {
        qualification: Arc<Semaphore>,
    }

    impl WindowsNamedPipeTransport {
        pub(super) fn new() -> Self {
            Self {
                qualification: Arc::new(Semaphore::new(1)),
            }
        }
    }

    #[async_trait]
    impl ProbeTransport for WindowsNamedPipeTransport {
        async fn exchange(
            &self,
            request: &[u8],
            maximum_response_bytes: usize,
        ) -> Result<Vec<u8>, IsolationProbeError> {
            if request.is_empty()
                || request.last() != Some(&b'\n')
                || request.len() > maximum_response_bytes
            {
                return Err(IsolationProbeError::Protocol);
            }
            timeout(
                EXCHANGE_TIMEOUT,
                exchange(request, maximum_response_bytes, self.qualification.clone()),
            )
            .await
            .map_err(|_| IsolationProbeError::Unavailable)?
        }
    }

    async fn exchange(
        request: &[u8],
        maximum_response_bytes: usize,
        qualification: Arc<Semaphore>,
    ) -> Result<Vec<u8>, IsolationProbeError> {
        let pipe = connect(Instant::now() + EXCHANGE_TIMEOUT).await?;
        let permit = qualification
            .acquire_owned()
            .await
            .map_err(|_| IsolationProbeError::Unavailable)?;
        let qualified = task::spawn_blocking(move || {
            let server = crate::windows_identity::verify_pipe_server(&pipe)?;
            Ok::<_, IsolationProbeError>((pipe, server, permit))
        })
        .await
        .map_err(|_| IsolationProbeError::Unavailable)??;
        let (mut pipe, _verified_server, permit) = qualified;
        drop(permit);
        pipe.write_all(request)
            .await
            .map_err(|_| IsolationProbeError::Unavailable)?;
        pipe.flush()
            .await
            .map_err(|_| IsolationProbeError::Unavailable)?;
        read_frame(&mut pipe, maximum_response_bytes).await
    }

    async fn connect(deadline: Instant) -> Result<NamedPipeClient, IsolationProbeError> {
        loop {
            let mut options = ClientOptions::new();
            // SECURITY_SQOS_PRESENT is added by Tokio. Identification lets the
            // service query the kernel-provided client token without delegating
            // resource access or an impersonation-capable token to the server.
            options.security_qos_flags(SECURITY_IDENTIFICATION);
            match options.open(SERVICE_PIPE_NAME) {
                Ok(client) => return Ok(client),
                Err(error) if retryable_connect_error(&error) && Instant::now() < deadline => {
                    sleep(
                        CONNECT_RETRY_DELAY.min(deadline.saturating_duration_since(Instant::now())),
                    )
                    .await;
                }
                Err(_) => return Err(IsolationProbeError::Unavailable),
            }
        }
    }

    fn retryable_connect_error(error: &io::Error) -> bool {
        error.kind() == io::ErrorKind::NotFound
            || error.raw_os_error() == i32::try_from(ERROR_PIPE_BUSY).ok()
    }

    async fn read_frame(
        pipe: &mut NamedPipeClient,
        maximum_response_bytes: usize,
    ) -> Result<Vec<u8>, IsolationProbeError> {
        let mut response = Vec::with_capacity(maximum_response_bytes.min(8 * 1024));
        let mut chunk = [0_u8; 8 * 1024];
        loop {
            let read = pipe
                .read(&mut chunk)
                .await
                .map_err(|_| IsolationProbeError::Unavailable)?;
            if read == 0 {
                return Err(IsolationProbeError::Protocol);
            }
            let bytes = &chunk[..read];
            if let Some(newline) = bytes.iter().position(|byte| *byte == b'\n') {
                if newline + 1 != bytes.len()
                    || response.len().saturating_add(newline + 1) > maximum_response_bytes
                {
                    return Err(IsolationProbeError::Protocol);
                }
                response.extend_from_slice(&bytes[..=newline]);
                return Ok(response);
            }
            if response.len().saturating_add(read) >= maximum_response_bytes {
                return Err(IsolationProbeError::Protocol);
            }
            response.extend_from_slice(bytes);
        }
    }
}
