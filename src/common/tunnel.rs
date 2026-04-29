use anyhow::{anyhow, Result};
use quinn::{RecvStream, SendStream};
use tracing::debug;

use crate::common::remote::RemoteResponse;
use crate::common::utils::SerdeHelper;

use super::remote::RemoteRequest;

pub async fn client_send_remote_request(
    remote: &RemoteRequest,
    send_channel: &mut SendStream,
    recv_channel: &mut RecvStream,
) -> Result<()> {
    debug!("sending remote request");
    let serialized = remote.to_json()?;
    send_channel.write_all(serialized.as_bytes()).await?;

    let mut buffer = [0u8; 1024];
    let n = recv_channel
        .read(&mut buffer)
        .await?
        .ok_or_else(|| anyhow!("Connection closed before receiving response"))?;
    let response = RemoteResponse::from_bytes(Vec::from(&buffer[..n]))?;

    match response {
        RemoteResponse::RemoteFailed(err) => return Err(anyhow!("Remote tunnel error: {}", err)),
        _ => debug!("remote accepted"),
    }

    Ok(())
}

pub async fn server_receive_remote_request(
    send_channel: &mut SendStream,
    recv_channel: &mut RecvStream,
    allow_reverse: bool,
) -> Result<RemoteRequest> {
    let mut buffer = [0; 1024];
    let n = recv_channel
        .read(&mut buffer)
        .await?
        .ok_or_else(|| anyhow!("Connection closed before receiving request"))?;
    let request = RemoteRequest::from_bytes(Vec::from(&buffer[..n]))?;

    debug!("received remote request: {}", request);

    if request.reversed && !allow_reverse {
        let response =
            RemoteResponse::RemoteFailed(String::from("Reverse remotes are not allowed"));
        send_channel
            .write_all(response.to_json()?.as_bytes())
            .await?;
        return Err(anyhow!("Reverse remotes are not allowed"));
    }

    let response = RemoteResponse::RemoteOk;
    send_channel
        .write_all(response.to_json()?.as_bytes())
        .await?;
    Ok(request)
}

pub async fn client_send_remote_start(
    send_channel: &mut SendStream,
    _remote: RemoteRequest,
) -> Result<()> {
    debug!("sending remote start");
    send_channel.write_all(b"remote_start").await?;
    Ok(())
}

pub async fn server_receive_remote_start(recv_channel: &mut RecvStream) -> Result<()> {
    let mut buffer = [0u8; 1024];
    recv_channel
        .read(&mut buffer)
        .await?
        .ok_or_else(|| anyhow!("Connection closed before receiving remote start"))?;
    debug!("received remote start");
    Ok(())
}
