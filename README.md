# fortune-402

A Lightning-powered fortune cookie server. Pay 1 sat, get a fortune.

Implements the [L402 protocol](https://docs.lightning.engineering/the-lightning-network/l402) — HTTP 402 Payment Required with Lightning invoices.

## How it works

1. `GET /fortune` → server returns HTTP 402 with a Lightning invoice
2. Pay the invoice, get the preimage
3. `GET /fortune` with `Authorization: L402 <token>:<preimage>` → receive your fortune

```bash
# 1. Request a fortune — get back a 402 with an invoice and token
$ curl -si http://localhost:3402/fortune
HTTP/1.1 402 Payment Required
www-authenticate: L402 token="abc123...", invoice="lnbc10n1p..."

{"payment_request":"lnbc10n1p...","amount_sats":1}

# 2. Extract the token from the www-authenticate header
#    and pay the invoice to get the preimage

# 3. Send both back to get your fortune
$ curl -s -H "Authorization: L402 abc123...:deadbeef..." http://localhost:3402/fortune | jq
{
  "fortune": "The cypherpunk writes code."
}
```

## Configuration

| Variable | Default | Description |
|---|---|---|
| `LND_ADDRESS` | `https://127.0.0.1:10009` | LND gRPC endpoint |
| `LND_CERT_PATH` | *required* | Path to LND TLS cert |
| `LND_MACAROON_PATH` | *required* | Path to LND admin macaroon |
| `LISTEN_ADDR` | `0.0.0.0:3402` | HTTP listen address |
| `INVOICE_AMOUNT_SATS` | `1` | Price per fortune |
| `INVOICE_MEMO` | `Fortune cookie` | Invoice description |
| `INVOICE_EXPIRY_SECS` | `300` | Invoice expiry |
| `L402_ROOT_KEY` | *random* | 32-byte hex key for token signing |
| `RUST_LOG` | `fortune_402=info` | Log level |

## Running

```bash
cp .env.example .env
# edit .env with your LND credentials
cargo run
```

## systemd

The server speaks the `sd_notify` protocol, so the unit can use `Type=notify`.
`systemctl start` then returns only once the server is connected to LND and
listening, and units ordered after it wait for that. `systemctl stop` sends
`SIGTERM`, which starts a graceful shutdown. If `WatchdogSec=` is set, the
server pings the watchdog and systemd restarts it when the process stops
responding.

Create a dedicated system user (e.g. `useradd --system fortune`) and make the
macaroon and cert readable by it.

```ini
[Unit]
Description=fortune-402 L402 fortune cookie server
After=network-online.target lnd.service
Wants=network-online.target

[Service]
Type=notify
User=fortune
ExecStart=/usr/local/bin/fortune-402
Environment=LND_ADDRESS=https://127.0.0.1:10009
Environment=LND_CERT_PATH=/etc/fortune-402/tls.cert
Environment=LND_MACAROON_PATH=/etc/fortune-402/admin.macaroon
Environment=LISTEN_ADDR=127.0.0.1:3402
# Secrets such as L402_ROOT_KEY go here, not in the unit file
EnvironmentFile=-/etc/fortune-402/env
WatchdogSec=30
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

Outside systemd the notifications are skipped, so the same binary runs
unchanged by hand or in Docker.

## Docker

```bash
docker build -t fortune-402 .
docker run -p 3402:3402 \
  -v /path/to/lnd:/lnd:ro \
  -e LND_ADDRESS=https://your-lnd:10009 \
  -e LND_CERT_PATH=/lnd/tls.cert \
  -e LND_MACAROON_PATH=/lnd/admin.macaroon \
  fortune-402
```

Pre-built images are available from GitHub Container Registry:

```bash
docker pull ghcr.io/benthecarman/fortune-402:main
```
