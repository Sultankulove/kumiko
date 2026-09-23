//! Binary streams (frame kinds 1 and 2).
//!
//! Streams are one-directional and their ids are chosen by the sending side, so both sides
//! may use the same number for different streams. The receiving side learns about a stream
//! from its first frame and hands a [`StreamReader`] to [`crate::Handler::stream`]; the id is
//! announced beforehand in some RPC result (e.g. `process/spawn`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc;

use kumiko_protocol::common::StreamId;

use crate::frame::Frame;
use crate::peer::RpcError;

/// Data is split into frames of at most this size.
pub const STREAM_CHUNK_LEN: usize = 1024 * 1024;

/// Sending half of an outgoing stream. Dropping it closes the stream.
#[derive(Debug)]
pub struct StreamWriter {
    id: StreamId,
    out: mpsc::Sender<Frame>,
    closed: bool,
}

impl StreamWriter {
    pub(crate) fn new(id: StreamId, out: mpsc::Sender<Frame>) -> Self {
        Self {
            id,
            out,
            closed: false,
        }
    }

    pub fn id(&self) -> StreamId {
        self.id
    }

    /// Sends bytes, waiting while the outgoing queue is full.
    pub async fn write(&mut self, data: &[u8]) -> Result<(), RpcError> {
        for chunk in data.chunks(STREAM_CHUNK_LEN) {
            self.out
                .send(Frame::StreamData {
                    stream: self.id,
                    data: chunk.to_vec(),
                })
                .await
                .map_err(|_| RpcError::ConnectionClosed)?;
        }
        Ok(())
    }

    /// Closes the stream after all data written so far.
    pub async fn close(mut self) -> Result<(), RpcError> {
        self.closed = true;
        self.out
            .send(Frame::StreamClose { stream: self.id })
            .await
            .map_err(|_| RpcError::ConnectionClosed)
    }
}

impl Drop for StreamWriter {
    fn drop(&mut self) {
        if self.closed {
            return;
        }
        let close = Frame::StreamClose { stream: self.id };
        if let Err(mpsc::error::TrySendError::Full(close)) = self.out.try_send(close) {
            // Keep the close frame behind the queued data instead of dropping it.
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                let out = self.out.clone();
                runtime.spawn(async move {
                    let _ = out.send(close).await;
                });
            }
        }
    }
}

/// Receiving half of an incoming stream.
#[derive(Debug)]
pub struct StreamReader {
    id: StreamId,
    data: mpsc::Receiver<Vec<u8>>,
    closed_by_sender: Arc<AtomicBool>,
}

impl StreamReader {
    pub(crate) fn new(
        id: StreamId,
        data: mpsc::Receiver<Vec<u8>>,
        closed_by_sender: Arc<AtomicBool>,
    ) -> Self {
        Self {
            id,
            data,
            closed_by_sender,
        }
    }

    pub fn id(&self) -> StreamId {
        self.id
    }

    /// Next chunk; `Ok(None)` after the sender closed the stream, an error if the
    /// connection was lost first.
    pub async fn recv(&mut self) -> Result<Option<Vec<u8>>, RpcError> {
        match self.data.recv().await {
            Some(chunk) => Ok(Some(chunk)),
            None if self.closed_by_sender.load(Ordering::Acquire) => Ok(None),
            None => Err(RpcError::ConnectionClosed),
        }
    }
}
