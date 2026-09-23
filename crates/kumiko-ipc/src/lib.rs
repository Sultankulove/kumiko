//! KPP transport: binary framing, RPC peer, streams, cancellation.
//!
//! [`Frame`] is the wire format of process plugins (spec, section 16). [`Peer`] runs a
//! symmetric JSON-RPC connection over any `AsyncRead`/`AsyncWrite` pair (pipes of a child
//! process, an in-memory duplex in tests), with cancellation, timeouts, bounded queues and
//! binary streams. [`Router`] dispatches incoming messages to typed closures.

mod frame;
mod peer;
mod router;
mod stream;

pub use frame::{Frame, FrameError, MAX_FRAME_LEN, MAX_MESSAGE_LEN};
pub use peer::{Handler, HandlerFuture, Peer, PeerConfig, RpcError};
pub use router::Router;
pub use stream::{STREAM_CHUNK_LEN, StreamReader, StreamWriter};
