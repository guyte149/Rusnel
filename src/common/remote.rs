use std::net::IpAddr;
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, Debug)]
pub enum Protocol {
    Tcp,
    Udp,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Remote {
    local_host: IpAddr,
    local_port: u16,
    remote_host: IpAddr,
    remote_port: u16,
    reversed: bool,
    socks: bool,
    protocol: Protocol,
}

impl Remote {
    pub fn new(
        local_host: IpAddr,
        local_port: u16,
        remote_host: IpAddr,
        remote_port: u16,
        reversed: bool,
        socks: bool,
        protocol: Protocol,
    ) -> Remote {
        Remote {
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
