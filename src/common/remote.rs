use serde::{Deserialize, Serialize};
use std::{net::IpAddr, str::FromStr};
use anyhow::{anyhow, Result};
use crate::common::utils::SerdeHelper;



#[derive(Serialize, Deserialize, Debug)]
pub enum Protocol {
    Tcp,
    Udp,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RemoteRequest {
    pub local_host: IpAddr,
    pub local_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
    pub reversed: bool,
    pub protocol: Protocol,
}

impl RemoteRequest {
    pub fn new(
        local_host: IpAddr,
        local_port: u16,
        remote_host: String,
        remote_port: u16,
        reversed: bool,
        protocol: Protocol,
    ) -> RemoteRequest {
        RemoteRequest {
            local_host,
            local_port,
            remote_host,
            remote_port,
            reversed,
            protocol,
        }
    }
}

impl RemoteRequest {
    pub fn from_str(remote_str: String) -> Result<RemoteRequest> {
        // remote_str can be in various formats, including:
        // <local-host>:<local-port>:<remote-host>:<remote-port>/<protocol>
        // <remote-host>:<remote-port>
        // <local-port>:<remote-host>:<remote-port>
        // R:<local-interface>:<local-port>:<remote-host>:<remote-port>/<protocol>

        let mut reversed = false;
        let mut protocol = Protocol::Tcp;

        let parts: Vec<&str> = remote_str.split('/').collect();
        if parts.is_empty() {
            return Err(anyhow!("Invalid format: Missing parts"));
        }

        let mut inner_remote_str = parts[0];
        if parts[0].starts_with("R:") {
            reversed = true;
            inner_remote_str = &parts[0][2..];
        } else if parts[0] == "R" {
            reversed = true;
            if parts.len() < 2 {
                return Err(anyhow!("Invalid format: Missing details after R"));
            }
            inner_remote_str = parts[1];
        }

        if parts.len() > 1 {
            match parts.last().unwrap() {
                &"tcp" => protocol = Protocol::Tcp,
                &"udp" => protocol = Protocol::Udp,
                _ => return Err(anyhow!("Invalid protocol: Must be 'tcp' or 'udp'")),
            }
        }

        let address_parts: Vec<&str> = inner_remote_str.split(':').collect();

        // Parse address parts and apply defaults based on the format
        let (local_host, local_port, remote_host, remote_port) = match address_parts.len() {
            1 => {
                let remote_port = address_parts[0].parse::<u16>().map_err(|_| anyhow!("Invalid remote port"))?;
                ("0.0.0.0".parse::<IpAddr>().unwrap(), remote_port, "0.0.0.0".to_string(), remote_port)
            }
            2 => {
                let remote_host = address_parts[0].to_string();
                let remote_port = address_parts[1].parse::<u16>().map_err(|_| anyhow!("Invalid remote port"))?;
                ("0.0.0.0".parse::<IpAddr>().unwrap(), remote_port, remote_host, remote_port)
            }
            3 => {
                let local_port = address_parts[0].parse::<u16>().map_err(|_| anyhow!("Invalid local port"))?;
                let remote_host = address_parts[1].to_string();
                let remote_port = address_parts[2].parse::<u16>().map_err(|_| anyhow!("Invalid remote port"))?;
                ("0.0.0.0".parse::<IpAddr>().unwrap(), local_port, remote_host, remote_port)
            }
            4 => {
                let local_host = address_parts[0].parse::<IpAddr>().map_err(|_| anyhow!("Invalid local host"))?;
                let local_port = address_parts[1].parse::<u16>().map_err(|_| anyhow!("Invalid local port"))?;
                let remote_host = address_parts[2].to_string();
                let remote_port = address_parts[3].parse::<u16>().map_err(|_| anyhow!("Invalid remote port"))?;
                (local_host, local_port, remote_host, remote_port)
            }
            _ => return Err(anyhow!("Invalid format: Unexpected number of address parts")),
        };

        Ok(RemoteRequest {
            local_host,
            local_port,
            remote_host,
            remote_port,
            reversed,
            protocol,
        })
    }
}

impl SerdeHelper for RemoteRequest {}

#[derive(Serialize, Deserialize, Debug)]
pub enum RemoteResponse {
    RemoteOk,
    RemoteFailed(String),
}

impl SerdeHelper for RemoteResponse {}


#[derive(Serialize, Deserialize, Debug)]
pub struct RemoteStart {
    remote_start: bool
}

impl SerdeHelper for RemoteStart {}
