# Rusnel

## Description
Rusnel is a high-performance tunneling tool built using Rust and leveraging the QUIC protocol. It is designed for secure and efficient data transmission with minimal latency. This tool provides robust features like automatic encryption, connection migration, and congestion control, making it ideal for various networking applications.

## Key Features
- **QUIC Protocol Integration**: Utilizes the latest advancements in QUIC for fast and secure communication.
- **Encryption**: Ensures all data transmitted through the tunnel is securely encrypted.
- **Tunneling feagures:**
    - ***TCP tunneling***
    - ***UDP tunneling***
    - ***reverse tunneling***
    - ***socks tunneling***

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
- [ ] write better --help
- [ ] write tests for tcp, udp, reverse and socks
- [ ] add reverse sock5 tunneling support
- [ ] add server --allow-reverse flag
- [ ] Rusnel never shuts down connection with remote (check)
- [ ] improve logging by adding the connection and stream that the log is reffered to
- [ ] client reconnect
- [v] add sock5 tunneling support
- [v] add reverse tunneling support
- [ ] support multiple connections through a single tunnel
- [ ] add proxy support for client (client connects to server through a proxy)
- [ ] generate a hardcoded key and CA every compliation
- [ ] add mtls authentication
- [ ] add fake-beckend http/3 feature to server

