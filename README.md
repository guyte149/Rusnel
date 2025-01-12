# Rusnel

## Description
Rusnel is a fast TCP/UDP tunnel, transported over and encrypted using QUIC protocol. Single executable including both client and server. Written in Rust.


## Features
-   Easy to use
-   Single executable including both client and server.
-   Uses QUIC protocol for fast and multiplexed communication.
-   Encrypted connections using the QUIC protocol (Tls1.3)
-   Clients can create multiple tunnel endpoints over one TCP connection
-   Reverse port forwarding (Connections go through the server and out the client)
-   Server allows SOCKS5 connections
-   Clients allow SOCKS5 connections from a reversed port forward


## Install
```bash
cargo install rusnel
```

### or

Clone the repository and build the project:
```bash
git clone https://github.com/guyte149/Rusnel.git
cd rusnel
cargo build --release
```

## Usage
```bash
$ rusnel --help
A fast tcp/udp tunnel

Usage: rusnel <COMMAND>

Commands:
  server  run Rusnel in server mode
  client  run Rusnel in client mode
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

```bash
$ rusnel server --help
run Rusnel in server mode

Usage: rusnel server [OPTIONS]

Options:
      --host <HOST>    defines Rusnel listening host (the network interface) [default: 0.0.0.0]
  -p, --port <PORT>    defines Rusnel listening port [default: 8080]
      --allow-reverse  Allow clients to specify reverse port forwarding remotes
  -v, --verbose        enable verbose logging
      --debug          enable debug logging
  -h, --help           Print help
```

```bash
$ rusnel client --help
run Rusnel in client mode

Usage: rusnel client [OPTIONS] <SERVER> <remote>...

Arguments:
  <SERVER>     defines the Rusnel server address (in form of host:port)
  <remote>...
               <remote>s are remote connections tunneled through the server, each which come in the form:

                   <local-host>:<local-port>:<remote-host>:<remote-port>/<protocol>

                   ■ local-host defaults to 0.0.0.0 (all interfaces).
                   ■ local-port defaults to remote-port.
                   ■ remote-port is required*.
                   ■ remote-host defaults to 0.0.0.0 (server localhost).
                   ■ protocol defaults to tcp.

               which shares <remote-host>:<remote-port> from the server to the client as <local-host>:<local-port>, or:

                   R:<local-host>:<local-port>:<remote-host>:<remote-port>/<protocol>

               which does reverse port forwarding,
               sharing <remote-host>:<remote-port> from the client to the server\'s <local-host>:<local-port>.

                   example remotes

                       1337
                       example.com:1337
                       1337:google.com:80
                       192.168.1.14:5000:google.com:80
                       socks
                       5000:socks
                       R:2222:localhost:22
                       R:socks
                       R:5000:socks
                       1.1.1.1:53/udp

                   When the Rusnel server has --allow-reverse enabled, remotes can be prefixed with R to denote that they are reversed.

                   Remotes can specify "socks" in place of remote-host and remote-port.
                   The default local host and port for a "socks" remote is 127.0.0.1:1080.


Options:
  -v, --verbose  enable verbose logging
      --debug    enable debug logging
  -h, --help     Print help
```

## TODO
- [ ] write tests for tcp, udp, reverse and socks - better convert to python tests
- [ ] improve logging by adding the connection and stream that the log is reffered to
- [ ] client reconnect
- [ ] add proxy support for client (client connects to server through a proxy)
- [ ] add server tls certificate verificatin
- [ ] add mutual tls verification
- [ ] add fake-beckend http/3 feature to server
- [ ] close QUIC connection when receiving ^C

