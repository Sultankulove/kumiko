//! Binary frames of process plugins (spec, section 16):
//!
//! | field     | size   | meaning                                             |
//! |-----------|--------|-----------------------------------------------------|
//! | length    | u32 LE | length of the rest of the frame, at most 16 MiB     |
//! | kind      | u8     | 0 JSON-RPC message, 1 stream data, 2 stream close   |
//! | stream_id | u32 LE | kinds 1 and 2 only                                  |
//! | payload   | rest   | UTF-8 JSON or raw bytes                             |

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use kumiko_protocol::common::StreamId;

/// Upper bound of `length`, i.e. of everything after the length field.
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

/// Largest JSON-RPC payload that fits into one frame.
pub const MAX_MESSAGE_LEN: usize = MAX_FRAME_LEN - 1;

const KIND_MESSAGE: u8 = 0;
const KIND_STREAM_DATA: u8 = 1;
const KIND_STREAM_CLOSE: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// A serialized JSON-RPC message.
    Message(Vec<u8>),
    StreamData {
        stream: StreamId,
        data: Vec<u8>,
    },
    StreamClose {
        stream: StreamId,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("frame length {0} exceeds the limit of {MAX_FRAME_LEN} bytes")]
    TooLarge(usize),
    #[error("frame length {0} is too small for its kind")]
    Truncated(usize),
    #[error("unknown frame kind {0}")]
    UnknownKind(u8),
}

impl Frame {
    /// Value of the `length` field.
    fn body_len(&self) -> usize {
        match self {
            Self::Message(payload) => 1 + payload.len(),
            Self::StreamData { data, .. } => 5 + data.len(),
            Self::StreamClose { .. } => 5,
        }
    }

    /// Reads one frame. `Ok(None)` means the peer closed the connection between frames.
    pub async fn read<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Option<Self>, FrameError> {
        let mut header = [0u8; 4];
        let mut filled = 0;
        while filled < header.len() {
            let n = reader.read(&mut header[filled..]).await?;
            if n == 0 {
                return if filled == 0 {
                    Ok(None)
                } else {
                    Err(io::Error::from(io::ErrorKind::UnexpectedEof).into())
                };
            }
            filled += n;
        }
        let len = u32::from_le_bytes(header) as usize;
        if len > MAX_FRAME_LEN {
            return Err(FrameError::TooLarge(len));
        }
        if len == 0 {
            return Err(FrameError::Truncated(len));
        }
        let kind = reader.read_u8().await?;
        let frame = match kind {
            KIND_MESSAGE => {
                let mut payload = vec![0; len - 1];
                reader.read_exact(&mut payload).await?;
                Self::Message(payload)
            }
            KIND_STREAM_DATA | KIND_STREAM_CLOSE => {
                if len < 5 {
                    return Err(FrameError::Truncated(len));
                }
                let stream = StreamId(reader.read_u32_le().await?);
                if kind == KIND_STREAM_CLOSE {
                    if len != 5 {
                        return Err(FrameError::Truncated(len));
                    }
                    Self::StreamClose { stream }
                } else {
                    let mut data = vec![0; len - 5];
                    reader.read_exact(&mut data).await?;
                    Self::StreamData { stream, data }
                }
            }
            other => return Err(FrameError::UnknownKind(other)),
        };
        Ok(Some(frame))
    }

    /// Writes one frame without flushing.
    pub async fn write<W: AsyncWrite + Unpin>(&self, writer: &mut W) -> Result<(), FrameError> {
        let len = self.body_len();
        if len > MAX_FRAME_LEN {
            return Err(FrameError::TooLarge(len));
        }
        let mut header = Vec::with_capacity(9);
        header.extend_from_slice(&(len as u32).to_le_bytes());
        match self {
            Self::Message(payload) => {
                header.push(KIND_MESSAGE);
                writer.write_all(&header).await?;
                writer.write_all(payload).await?;
            }
            Self::StreamData { stream, data } => {
                header.push(KIND_STREAM_DATA);
                header.extend_from_slice(&stream.0.to_le_bytes());
                writer.write_all(&header).await?;
                writer.write_all(data).await?;
            }
            Self::StreamClose { stream } => {
                header.push(KIND_STREAM_CLOSE);
                header.extend_from_slice(&stream.0.to_le_bytes());
                writer.write_all(&header).await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn encode(frame: &Frame) -> Vec<u8> {
        let mut out = Vec::new();
        frame.write(&mut out).await.unwrap();
        out
    }

    async fn decode(mut bytes: &[u8]) -> Result<Option<Frame>, FrameError> {
        Frame::read(&mut bytes).await
    }

    #[tokio::test]
    async fn frames_round_trip_with_exact_layout() {
        let message = Frame::Message(b"{}".to_vec());
        assert_eq!(encode(&message).await, [3, 0, 0, 0, 0, b'{', b'}']);

        let data = Frame::StreamData {
            stream: StreamId(0x0102_0304),
            data: vec![9, 8],
        };
        assert_eq!(encode(&data).await, [7, 0, 0, 0, 1, 4, 3, 2, 1, 9, 8]);

        let close = Frame::StreamClose {
            stream: StreamId(5),
        };
        assert_eq!(encode(&close).await, [5, 0, 0, 0, 2, 5, 0, 0, 0]);

        for frame in [message, data, close] {
            let bytes = encode(&frame).await;
            assert_eq!(decode(&bytes).await.unwrap(), Some(frame));
        }
    }

    #[tokio::test]
    async fn eof_between_frames_is_clean() {
        assert!(decode(&[]).await.unwrap().is_none());
        assert!(matches!(decode(&[3, 0]).await, Err(FrameError::Io(_))));
        assert!(matches!(
            decode(&[3, 0, 0, 0, 0, b'{']).await,
            Err(FrameError::Io(_))
        ));
    }

    #[tokio::test]
    async fn malformed_headers_are_rejected_before_allocating() {
        assert!(matches!(
            decode(&u32::MAX.to_le_bytes()).await,
            Err(FrameError::TooLarge(_))
        ));
        assert!(matches!(
            decode(&[0, 0, 0, 0]).await,
            Err(FrameError::Truncated(0))
        ));
        assert!(matches!(
            decode(&[2, 0, 0, 0, 1, 0]).await,
            Err(FrameError::Truncated(2))
        ));
        assert!(matches!(
            decode(&[6, 0, 0, 0, 2, 1, 0, 0, 0, 0]).await,
            Err(FrameError::Truncated(6))
        ));
        assert!(matches!(
            decode(&[1, 0, 0, 0, 7]).await,
            Err(FrameError::UnknownKind(7))
        ));
    }

    #[tokio::test]
    async fn oversized_frames_are_not_written() {
        let frame = Frame::Message(vec![0; MAX_MESSAGE_LEN + 1]);
        let mut out = Vec::new();
        assert!(matches!(
            frame.write(&mut out).await,
            Err(FrameError::TooLarge(_))
        ));
        assert!(out.is_empty());
    }
}
