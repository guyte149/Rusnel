//! Length-prefixed control plane on top of QUIC bi-streams.
//!
//! QUIC streams are byte-oriented, so any control message exchanged on a
//! stream must carry its own framing. We use a tiny `u32` little-endian
//! length prefix followed by a MessagePack body (see [`SerdeHelper`]).
//!
//! Two control flows live on top of this framing:
//!
//! * **Session hello.** The first bi-stream of every QUIC connection
//!   carries one [`SessionHello`] (client → server) and one
//!   [`SessionHelloResponse`] (server → client). The server uses the
//!   hello to validate every requested remote against `--allow-reverse`
//!   / `--allow-socks` in one shot, assigns each declaration a stable
//!   `tunnel_id`, and either accepts the whole batch or rejects the
//!   session entirely. There is no half-accept.
//!
//! * **Conn opener.** Every subsequent data-plane bi-stream (in either
//!   direction) opens with one [`OpenConn`] frame keyed by a previously
//!   negotiated `tunnel_id`. The receiving side replies with one
//!   [`OpenConnResponse`] and, on `Ok`, the bi-stream is handed off to
//!   the data-plane handler — no further control framing.

use anyhow::{anyhow, Context, Result};
use quinn::{RecvStream, SendStream};
use tokio::io::AsyncWriteExt;
use tracing::debug;

use crate::common::remote::{OpenConn, OpenConnResponse, SessionHello, SessionHelloResponse};
use crate::common::utils::SerdeHelper;

/// Hard cap on a single control message body. Generous compared to
/// anything the protocol actually sends today (the largest realistic
/// payload is a [`SessionHello`] with a few dozen `RemoteRequest`s),
/// but small enough that a malicious peer can't make us allocate
/// gigabytes by lying about the length.
const MAX_CONTROL_MSG: usize = 64 * 1024;

async fn write_framed<T: SerdeHelper>(send: &mut SendStream, msg: &T) -> Result<()> {
    let body = msg.to_bytes()?;
    let len = u32::try_from(body.len()).context("control message exceeds u32::MAX")?;
    send.write_all(&len.to_le_bytes()).await?;
    send.write_all(&body).await?;
    Ok(())
}

async fn read_framed<T: SerdeHelper>(recv: &mut RecvStream) -> Result<T> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .map_err(|e| anyhow!("failed to read control message length: {e}"))?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_CONTROL_MSG {
        return Err(anyhow!(
            "control message length {len} exceeds cap of {MAX_CONTROL_MSG}"
        ));
    }
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body)
        .await
        .map_err(|e| anyhow!("failed to read control message body: {e}"))?;
    T::from_bytes(body)
}

// ---------------------------------------------------------------------------
// Session-level hello
// ---------------------------------------------------------------------------

/// Client side of the hello: send the full tunnel-declaration batch
/// and wait for the server's verdict. On success, returns the list of
/// server-assigned `tunnel_id`s in the same order as `hello.remotes`.
pub async fn client_send_session_hello(
    hello: &SessionHello,
    send: &mut SendStream,
    recv: &mut RecvStream,
) -> Result<Vec<u64>> {
    debug!(remotes = hello.remotes.len(), "sending session hello");
    write_framed(send, hello).await?;
    send.shutdown().await?;
    match read_framed::<SessionHelloResponse>(recv).await? {
        SessionHelloResponse::Ok { tunnel_ids } => {
            if tunnel_ids.len() != hello.remotes.len() {
                return Err(anyhow!(
                    "server returned {} tunnel ids for {} remotes",
                    tunnel_ids.len(),
                    hello.remotes.len()
                ));
            }
            debug!(
                "session accepted, {} tunnel(s) registered",
                tunnel_ids.len()
            );
            Ok(tunnel_ids)
        }
        SessionHelloResponse::Failed(reason) => Err(anyhow!("server rejected session: {reason}")),
    }
}

/// Server side of the hello: receive the batch. The caller is
/// responsible for validating each remote (against `--allow-reverse` /
/// `--allow-socks`), assigning ids, and replying with
/// [`server_reply_session_hello`].
pub async fn server_receive_session_hello(recv: &mut RecvStream) -> Result<SessionHello> {
    let hello: SessionHello = read_framed(recv).await?;
    debug!(remotes = hello.remotes.len(), "received session hello");
    Ok(hello)
}

pub async fn server_reply_session_hello(
    send: &mut SendStream,
    response: &SessionHelloResponse,
) -> Result<()> {
    write_framed(send, response).await?;
    send.shutdown().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Per-conn opener
// ---------------------------------------------------------------------------

/// Send an [`OpenConn`] on a freshly opened bi-stream and wait for the
/// peer's verdict. On `Ok`, the stream is yours to use as a data-plane
/// channel.
pub async fn send_open_conn(
    open: &OpenConn,
    send: &mut SendStream,
    recv: &mut RecvStream,
) -> Result<()> {
    write_framed(send, open).await?;
    match read_framed::<OpenConnResponse>(recv).await? {
        OpenConnResponse::Ok => Ok(()),
        OpenConnResponse::Failed(reason) => Err(anyhow!("conn open rejected: {reason}")),
    }
}

pub async fn receive_open_conn(recv: &mut RecvStream) -> Result<OpenConn> {
    read_framed(recv).await
}

pub async fn reply_open_conn(send: &mut SendStream, response: &OpenConnResponse) -> Result<()> {
    write_framed(send, response).await
}
