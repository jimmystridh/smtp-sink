# smtp-sink

A minimal SMTP sink for local development and testing. Receives emails via SMTP and exposes them via HTTP for inspection.

## Features

- SMTP server on configurable port (default: 1025)
- HTTP REST API for email retrieval (default: 1080)
- WebSocket for real-time updates
- Web UI for visual inspection
- Ring buffer with configurable max emails
- Sender whitelist filtering
- TLS/SMTPS support with self-signed or custom certificates

## Installation

```bash
cargo install --path .
```

## Usage

```bash
# Basic usage
smtp-sink

# Custom ports
smtp-sink --smtp-port 2525 --http-port 8080

# With sender whitelist
smtp-sink --whitelist alice@example.com,bob@example.com

# With TLS (self-signed)
smtp-sink --tls --tls-self-signed

# With custom certificates
smtp-sink --tls --tls-key ./key.pem --tls-cert ./cert.pem
```

## Options

```
-s, --smtp-port <PORT>     SMTP port [default: 1025]
-p, --http-port <PORT>     HTTP port [default: 1080]
-w, --whitelist <ADDRS>    Comma-separated allowed senders
-m, --max <N>              Max emails to keep [default: 10]
    --tls                  Enable SMTPS
    --tls-key <PATH>       TLS private key (PEM)
    --tls-cert <PATH>      TLS certificate (PEM)
    --tls-self-signed      Generate self-signed certificate
```

## API

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/` | Web UI |
| GET | `/emails` | List all emails (JSON) |
| DELETE | `/emails` | Clear all emails |
| DELETE | `/emails/:id` | Delete single email |
| GET | `/ws` | WebSocket for real-time updates |

## Example

```bash
# Start the sink
smtp-sink

# Send a test email
echo -e "HELO localhost\nMAIL FROM:<test@example.com>\nRCPT TO:<user@example.com>\nDATA\nSubject: Hello\n\nTest message\n.\nQUIT" | nc localhost 1025

# View emails
curl http://localhost:1080/emails
```

## License

MIT
