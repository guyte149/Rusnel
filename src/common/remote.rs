use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use anyhow::Result;
use crate::common::utils::SerdeHelper;



#[derive(Serialize, Deserialize, Debug)]
pub enum Protocol {
    Tcp,
    Udp,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RemoteRequest {
    pub local_host: String,
    pub local_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
    pub reversed: bool,
    pub protocol: Protocol,
}

impl RemoteRequest {
    pub fn new(
        local_host: String,
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
    pub fn from_str(remote_str: String) -> Result<RemoteRequest>{
        // remote_str is R/<local-interface>:<local-port>:<remote-host>:<remote-port>/<protocol>
        let mut reversed = false;
        let mut protocol = Protocol::Tcp;
        let splits: Vec<&str> = remote_str.split("/").collect();
        let mut inner_remote_str = splits[0];
        if splits[0] == "R" {
            reversed = true;
            inner_remote_str = splits[1];
        }
        else if splits.last().unwrap() == &"tcp" || splits.last().unwrap() == &"udp" {
            if splits.last().unwrap() == &"udp" {
                protocol = Protocol::Udp;
            }
        }
        let splits: Vec<&str> = inner_remote_str.split(":").collect();
        //todo continue the parsing

        Ok(())




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
