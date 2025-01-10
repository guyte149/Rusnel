# Rusnel

## Description
Rusnel is a fast TCP/UDP tunnel, transported over and encrypted using QUIC protocol. Single executable including both client and server. Written in Rust!.
Rusnel is mainly useful for passing through firewalls, though it can also be used to provide a secure endpoint into your network.

## Features

-   Easy to use
-   Utilizes the latest advancements in QUIC for fast and secure communication.
-   Encrypted connections using the QUIC protocol
-   Clients can create multiple tunnel endpoints over one TCP connection
-   Reverse port forwarding (Connections go through the server and out the client)
-   Server optionally allows SOCKS5 connections (See guide below)
-   Clients allow SOCKS5 connections from a reversed port forward


## Installation
Clone the repository and build the project:
```bash
git clone https://github.com/guyte149/Rusnel.git
cd rusnel
cargo build --release
```

## Usage
```bash
./target/release/rusnel [OPTIONS]
```

## TODO
- [v] Rusnel never shuts down connection with remote (check)
- [v] write better --help
- [ ] write tests for tcp, udp, reverse and socks - better convert to python tests
- [v] add reverse sock5 tunneling support
- [v] add server --allow-reverse flag
- [ ] improve logging by adding the connection and stream that the log is reffered to
- [ ] client reconnect
- [v] add sock5 tunneling support
- [v] add reverse tunneling support
- [v] support multiple connections through a single tunnel
- [v] support multiple connections through UDP tunnel
- [ ] add proxy support for client (client connects to server through a proxy)
- [ ] generate a hardcoded key and CA every compliation
- [ ] add mtls authentication
- [ ] add fake-beckend http/3 feature to server

