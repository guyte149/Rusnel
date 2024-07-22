# Rusnel

## Description
Rusnel is a high-performance tunneling tool built using Rust and leveraging the QUIC protocol. It is designed for secure and efficient data transmission with minimal latency. This tool provides robust features like automatic encryption, connection migration, and congestion control, making it ideal for various networking applications.

## Key Features
- **QUIC Protocol Integration**: Utilizes the latest advancements in QUIC for fast and secure communication.
- **Encryption**: Ensures all data transmitted through the tunnel is securely encrypted.
- **Connection Resilience**: Supports connection migration to maintain connectivity even when network changes occur.

## Requirements
- Rust 1.58.0 or newer
- OpenSSL for cryptographic functions

## Installation
Clone the repository and build the project:
```bash
git clone https://github.com/yourusername/rusnel.git
cd rusnel
cargo build --release
```

## Usage
```bash
./target/release/rusnel [OPTIONS]
```

## TODO
- [ ] better error handling
- [ ] improve logging by adding the connection that the log is reffered to
- [ ] add sock5 tunneling support
- [ ] add reverse tunneling support
- [ ] add proxy support for client (client connects to server through a proxy)
- [ ] add password authentication
- [ ] add server support for real certificate and client support for custom CA
- [ ] add tls private key authentication?
- [ ] add fake-beckend feature to server

## Contributing
Contributions are welcome! Please read CONTRIBUTING.md for details on our code of conduct, and the process for submitting pull requests to us.
Let me know if you'd like to add or modify any specific sections, or if there are particular details you want included that reflect your project's functionality and setup.