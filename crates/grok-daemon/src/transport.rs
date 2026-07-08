use std::{sync::Arc, time::Duration};

use grok_protocol::v1;
use prost::Message;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tracing::warn;

use crate::Daemon;

/// Maximum encoded IPC message size. Large payloads use the encrypted blob store.
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;
const FRAME_HEADER_READ_TIMEOUT: Duration = Duration::from_secs(5);
const FRAME_BODY_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Bounded frame segment which exceeded its local read deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameReadPhase {
    /// Four-byte big-endian frame length.
    Header,
    /// Exact length-delimited Protobuf payload.
    Body,
}

impl std::fmt::Display for FrameReadPhase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Header => "header",
            Self::Body => "body",
        })
    }
}

/// Local IPC framing or protocol failure.
#[derive(Debug, Error)]
pub enum TransportError {
    /// Peer closed the connection between frames.
    #[error("connection closed")]
    Closed,
    /// Underlying local transport failed.
    #[error("I/O failure: {0}")]
    Io(#[from] std::io::Error),
    /// Peer did not finish a bounded frame segment before its deadline.
    #[error("frame {0} read timed out")]
    ReadTimeout(FrameReadPhase),
    /// Frame length exceeded the hard IPC limit.
    #[error("frame length {0} exceeds limit")]
    FrameTooLarge(usize),
    /// Protobuf payload was malformed.
    #[error("invalid protobuf: {0}")]
    Decode(#[from] prost::DecodeError),
    /// Response could not be encoded within the frame limit.
    #[error("encoded response exceeds frame limit")]
    EncodedResponseTooLarge,
}

/// Reads one big-endian length-delimited Protobuf envelope.
///
/// # Errors
///
/// Returns [`TransportError`] for connection, size, I/O, or decoding failures.
pub async fn read_frame<R>(reader: &mut R) -> Result<v1::Envelope, TransportError>
where
    R: AsyncRead + Unpin,
{
    read_frame_with_timeouts(reader, FRAME_HEADER_READ_TIMEOUT, FRAME_BODY_READ_TIMEOUT).await
}

async fn read_frame_with_timeouts<R>(
    reader: &mut R,
    header_timeout: Duration,
    body_timeout: Duration,
) -> Result<v1::Envelope, TransportError>
where
    R: AsyncRead + Unpin,
{
    read_frame_with_deadlines(reader, Some(header_timeout), body_timeout).await
}

async fn read_authenticated_frame<R>(reader: &mut R) -> Result<v1::Envelope, TransportError>
where
    R: AsyncRead + Unpin,
{
    // The paired Electron process may legitimately keep its authenticated
    // primary RPC connection idle. Body reads remain bounded so a partial
    // authenticated frame still cannot occupy a connection indefinitely.
    read_frame_with_deadlines(reader, None, FRAME_BODY_READ_TIMEOUT).await
}

async fn read_frame_with_deadlines<R>(
    reader: &mut R,
    header_timeout: Option<Duration>,
    body_timeout: Duration,
) -> Result<v1::Envelope, TransportError>
where
    R: AsyncRead + Unpin,
{
    let header = match header_timeout {
        Some(timeout) => tokio::time::timeout(timeout, reader.read_u32())
            .await
            .map_err(|_| TransportError::ReadTimeout(FrameReadPhase::Header))?,
        None => reader.read_u32().await,
    };
    let length = match header {
        Ok(length) => usize::try_from(length).unwrap_or(usize::MAX),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Err(TransportError::Closed);
        }
        Err(error) => return Err(TransportError::Io(error)),
    };
    if length > MAX_FRAME_BYTES {
        return Err(TransportError::FrameTooLarge(length));
    }
    let mut frame = vec![0; length];
    tokio::time::timeout(body_timeout, reader.read_exact(&mut frame))
        .await
        .map_err(|_| TransportError::ReadTimeout(FrameReadPhase::Body))??;
    Ok(v1::Envelope::decode(frame.as_slice())?)
}

/// Writes one big-endian length-delimited Protobuf envelope.
///
/// # Errors
///
/// Returns [`TransportError`] for oversize responses or local I/O failures.
pub async fn write_frame<W>(writer: &mut W, envelope: &v1::Envelope) -> Result<(), TransportError>
where
    W: AsyncWrite + Unpin,
{
    let length = envelope.encoded_len();
    if length > MAX_FRAME_BYTES || length > u32::MAX as usize {
        return Err(TransportError::EncodedResponseTooLarge);
    }
    let mut buffer = Vec::with_capacity(length);
    let wire_length = u32::try_from(length).map_err(|_| TransportError::EncodedResponseTooLarge)?;
    writer.write_u32(wire_length).await?;
    envelope
        .encode(&mut buffer)
        .map_err(|_| TransportError::EncodedResponseTooLarge)?;
    writer.write_all(&buffer).await?;
    writer.flush().await?;
    Ok(())
}

/// Serves requests sequentially on one client connection.
///
/// Independent connections remain concurrent; sequential handling within one
/// stream preserves request ordering and provides natural backpressure.
///
/// # Errors
///
/// Returns [`TransportError`] when framing or the underlying local stream fails.
pub async fn serve_connection<S>(stream: S, daemon: Arc<Daemon>) -> Result<(), TransportError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (mut reader, mut writer) = tokio::io::split(stream);
    let mut authenticated = false;
    loop {
        let request = match if authenticated {
            read_authenticated_frame(&mut reader).await
        } else {
            read_frame(&mut reader).await
        } {
            Ok(request) => request,
            Err(TransportError::Closed) => return Ok(()),
            Err(error) => return Err(error),
        };
        match daemon.handle(request).await {
            Ok(response) => {
                authenticated = true;
                write_frame(&mut writer, &response).await?;
            }
            Err(error) => {
                // Authentication and envelope failures close the connection so
                // an unpaired process cannot use the daemon as an oracle.
                warn!(error = %error, "rejecting local IPC envelope");
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use grok_protocol::{PROTOCOL_VERSION, v1::envelope};

    use super::*;

    #[tokio::test]
    async fn frame_round_trip_is_bounded() {
        let envelope = v1::Envelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: "request-1".into(),
            startup_nonce: vec![1; 32],
            deadline_unix_ms: 10,
            idempotency_key: String::new(),
            payload: Some(envelope::Payload::Request(v1::Request {
                operation: Some(v1::request::Operation::Health(v1::HealthRequest {})),
            })),
        };
        let (mut client, mut server) = tokio::io::duplex(4096);
        let write = tokio::spawn(async move { write_frame(&mut client, &envelope).await });
        let decoded = read_frame(&mut server).await.expect("read");
        write.await.expect("join").expect("write");
        assert_eq!(decoded.request_id, "request-1");
    }

    #[tokio::test]
    async fn partial_frame_header_has_a_fixed_read_deadline() {
        let (mut client, mut server) = tokio::io::duplex(16);
        client
            .write_all(&[0, 0])
            .await
            .expect("partial frame header");
        let error = read_frame_with_timeouts(
            &mut server,
            Duration::from_millis(20),
            Duration::from_secs(1),
        )
        .await
        .expect_err("partial header timeout");
        assert!(matches!(
            error,
            TransportError::ReadTimeout(FrameReadPhase::Header)
        ));
    }

    #[tokio::test]
    async fn partial_frame_body_has_a_fixed_read_deadline() {
        let (mut client, mut server) = tokio::io::duplex(64);
        client.write_u32(32).await.expect("frame length");
        client.write_all(&[1]).await.expect("partial frame body");
        let error = read_frame_with_timeouts(
            &mut server,
            Duration::from_secs(1),
            Duration::from_millis(20),
        )
        .await
        .expect_err("partial body timeout");
        assert!(matches!(
            error,
            TransportError::ReadTimeout(FrameReadPhase::Body)
        ));
    }

    #[tokio::test]
    async fn authenticated_idle_reuse_is_not_subject_to_the_header_timeout() {
        let envelope = v1::Envelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: "request-after-idle".into(),
            startup_nonce: vec![1; 32],
            deadline_unix_ms: 10,
            idempotency_key: String::new(),
            payload: Some(envelope::Payload::Request(v1::Request {
                operation: Some(v1::request::Operation::Health(v1::HealthRequest {})),
            })),
        };
        let (mut client, mut server) = tokio::io::duplex(4096);
        let write = tokio::spawn(async move {
            // This exceeds the 20 ms pre-auth deadline exercised above.
            tokio::time::sleep(Duration::from_millis(40)).await;
            write_frame(&mut client, &envelope).await
        });
        let decoded = tokio::time::timeout(
            Duration::from_secs(1),
            read_authenticated_frame(&mut server),
        )
        .await
        .expect("authenticated idle connection remains open")
        .expect("frame after authenticated idle");
        write.await.expect("writer task").expect("frame write");
        assert_eq!(decoded.request_id, "request-after-idle");
    }
}
