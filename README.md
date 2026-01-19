# Slipstream (Rust)

Slipstream is a high-performance DNS tunnel that carries QUIC packets over DNS queries and responses.
This repository hosts the Rust rewrite of the [original C implementation](https://github.com/EndPositive/slipstream).

## What is here

- slipstream-client and slipstream-server CLI binaries.
- A DNS codec crate with vector-based tests.
- Pure-Rust QUIC transport using [tquic](https://github.com/tencent/tquic).
- DNS packet fragmentation for large QUIC handshakes.
- Certificate pinning with self-signed cert support.
- Multipath QUIC for path diversity.
- Fully async with tokio.

## Platform Compatibility

Pre-built binaries are available for the following platforms:

| Artifact | Platform | Notes |
|----------|----------|-------|
| `slipstream-linux-x86_64` | Linux x86_64 | Standard Linux distributions |
| `slipstream-linux-aarch64` | Linux ARM64 | ARM64 servers, Raspberry Pi OS |
| `slipstream-macos-aarch64` | macOS ARM64 | Apple Silicon Macs |

> **Android/Termux users**: The Linux binaries require glibc which Termux doesn't have. 
> Use [proot-distro](https://github.com/termux/proot-distro) to run a full Linux environment:
> ```bash
> pkg install proot-distro
> proot-distro install debian
> proot-distro login debian
> # Now you can run the aarch64 binary
> ```

## Quick start (local dev)

Prereqs:

- Rust toolchain (stable)

Build the Rust binaries:

```
cargo build -p slipstream-client -p slipstream-server
```

Generate a self-signed TLS cert:

```
openssl req -x509 -newkey rsa:2048 -nodes \
  -keyout key.pem -out cert.pem -days 365 \
  -subj "/CN=slipstream"
```

Run the server:

```
cargo run -p slipstream-server -- \
  --dns-listen-port 8853 \
  --target-address 127.0.0.1:5201 \
  --domain example.com \
  --cert ./cert.pem \
  --key ./key.pem
```

Run the client:

```
cargo run -p slipstream-client -- \
  --tcp-listen-port 7000 \
  --resolver 127.0.0.1:8853 \
  --domain example.com
```

### Certificate Pinning

For production use, pin the server certificate on the client:

```
cargo run -p slipstream-client -- \
  --tcp-listen-port 7000 \
  --resolver 127.0.0.1:8853 \
  --domain example.com \
  --cert ./cert.pem   # Pin server's certificate
```

Self-signed certificates are supported by default. The pinned cert is used as the trusted CA.

### Multipath QUIC

Use multiple resolvers for path diversity:

```
cargo run -p slipstream-client -- \
  --tcp-listen-port 7000 \
  --resolver 1.1.1.1:53/recursive \
  --resolver 8.8.8.8:53/recursive \
  --domain example.com
```

## Benchmarks (local snapshot)

All results below are end-to-end completion times in seconds (lower is better),
averaged over 5 runs on local loopback. Payload: 10 MiB in each direction.

See `scripts/bench` for scripts used for obtaining these results.

| Variant                              | Exfil avg (s) | Download avg (s) |
|--------------------------------------| ---: | ---: |
| dnstt                                | 16.207 | 2.492 |
| slipstream (C)                       | 5.332 | 1.096 |
| slipstream-rust                      | 3.249 | 0.978 |
| slipstream-rust (Authoritative mode) | 1.602 | 0.407 |

![Throughput bar chart](.github/throughput.png)

## Documentation

- docs/README.md for the doc index
- docs/build.md for build prerequisites
- docs/usage.md for CLI usage
- docs/protocol.md for DNS encapsulation notes
- docs/dns-codec.md for codec behavior and vectors
- docs/interop.md for local harnesses and interop
- docs/benchmarks.md for benchmarking harnesses
- docs/benchmarks-results.md for benchmark results
- docs/profiling.md for profiling notes
- docs/design.md for architecture notes
- CLAUDE.md for internal development notes

## Repo layout

- crates/      Rust workspace crates
- docs/        Public docs and internal design notes
- fixtures/    Test certificates and DNS vectors
- scripts/     Interop and benchmark harnesses
- tools/       Vector generator and helpers

## License

Apache-2.0. See LICENSE.

