//! Length-prefixed control plane on top of QUIC bi-streams.
//!
//! QUIC streams are byte-oriented, so any control message exchanged on a
//! stream must carry its own framing. We use a tiny `u32` little-endian
//! length prefix followed by a JSON body (kept as JSON for now to limit the
//! blast radius of this change — see issue #21 for a binary codec).

use anyhow::{anyhow, Context, Result};
use quinn::{RecvStream, SendStream};
use tracing::debug;

use crate::common::remote::RemoteResponse;
use crate::common::utils::SerdeHelper;

use super::remote::RemoteRequest;

/// Hard cap on a single control message body. Generous compared to anything
/// the protocol actually sends today, but small enough that a malicious peer
/// can't make us allocate gigabytes by lying about the length.
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

pub async fn client_send_remote_request(
    remote: &RemoteRequest,
    send_channel: &mut SendStream,
    recv_channel: &mut RecvStream,
) -> Result<()> {
    debug!("sending remote request");
    write_framed(send_channel, remote).await?;

    let response: RemoteResponse = read_framed(recv_channel).await?;
    match response {
        RemoteResponse::RemoteFailed(err) => Err(anyhow!("Remote tunnel error: {}", err)),
        RemoteResponse::RemoteOk => {
            debug!("remote accepted");
            Ok(())
        }
    }
}

pub async fn server_receive_remote_request(
    send_channel: &mut SendStream,
    recv_channel: &mut RecvStream,
    allow_reverse: bool,
) -> Result<RemoteRequest> {
    let request: RemoteRequest = read_framed(recv_channel).await?;
    debug!("received remote request: {}", request);

    if request.is_reversed() && !allow_reverse {
        let response =
            RemoteResponse::RemoteFailed(String::from("Reverse remotes are not allowed"));
        write_framed(send_channel, &response).await?;
        return Err(anyhow!("Reverse remotes are not allowed"));
    }

    write_framed(send_channel, &RemoteResponse::RemoteOk).await?;
    Ok(request)
}
