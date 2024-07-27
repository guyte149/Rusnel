use serde::{Deserialize, Serialize};
use std::net::IpAddr;

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
    pub remote_host: IpAddr,
    pub remote_port: u16,
    pub reversed: bool,
    pub socks: bool,
    pub protocol: Protocol,
}

impl RemoteRequest {
    pub fn new(
        local_host: IpAddr,
        local_port: u16,
        remote_host: IpAddr,
        remote_port: u16,
        reversed: bool,
        socks: bool,
        protocol: Protocol,
    ) -> RemoteRequest {
        RemoteRequest {
            local_host,
            local_port,
            remote_host,
            remote_port,
            reversed,
            socks,
            protocol,
        }
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
