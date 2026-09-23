# fortune-402

A Lightning-powered fortune cookie server. Pay 1 sat, get a fortune.

Implements the [L402 protocol](https://docs.lightning.engineering/the-lightning-network/l402) — HTTP 402 Payment Required with Lightning invoices —
on `/fortune`, and [x402](https://github.com/x402-foundation/x402) with the
Lightning `exact` scheme on `/x402`.

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

## x402

`/x402` sells the same fortune over x402 v2, using the
[`exact` scheme on Lightning](https://github.com/x402-foundation/x402/blob/main/specs/schemes/exact/scheme_exact_lnbtc.md).
It is separate from `/fortune`: an L402 token does not work on `/x402`, and an
x402 payment does not work on `/fortune`.

1. `GET /x402` → server returns HTTP 402 with a `PAYMENT-REQUIRED` header
2. Pay the invoice in `accepts[0].extra.invoice`, get the preimage
3. `GET /x402` again with a `PAYMENT-SIGNATURE` header → receive your fortune

The invoice commits to the request (method, URL including the query, and body)
through its description hash, so a payment only buys the request it was made
for. Each payment is good for one fortune.

```bash
# 1. Request a fortune. The body is the same JSON as the base64
#    PAYMENT-REQUIRED header.
$ curl -s https://fortune.example.com/x402 > challenge.json
$ jq -r '.accepts[0].extra.invoice' challenge.json
lnbc10n1p...

# 2. Pay the invoice and get the preimage, then build the payment payload
$ PAYMENT=$(jq -c --arg preimage "$PREIMAGE" \
    '{x402Version: 2, accepted: .accepts[0], payload: {preimage: $preimage}}' \
    challenge.json | base64 -w0)

# 3. Send the same request with the payment
$ curl -s -H "PAYMENT-SIGNATURE: $PAYMENT" https://fortune.example.com/x402 | jq
{
  "fortune": "The cypherpunk writes code."
}
```

The response has a `PAYMENT-RESPONSE` header with the settlement result. If a
payment is rejected, the server returns 402 with a fresh challenge, and
`PAYMENT-RESPONSE` holds the reason in `errorReason`.

x402 is enabled when `PUBLIC_URL` is set. Requirements:

- `PUBLIC_URL` must be the URL clients use to reach the server, because each
  payment is bound to the full request URL. Behind a reverse proxy, use the
  public URL, not the listen address.
- LND must be on mainnet, testnet3 or signet. The scheme has no identifier for
  testnet4 or regtest, so on those networks `/x402` is disabled with a warning.
- The server must be the only party that can create invoices on the LND node.
  Anyone else who can create invoices on it could pay their own invoice and use
  the proof here.
- Used payments are recorded in a SQLite database at `REPLAY_DB_PATH`. It must
  persist across restarts, otherwise a payment could be used twice.

## Configuration

| Variable | Default | Description |
|---|---|---|
| `LND_ADDRESS` | `https://127.0.0.1:10009` | LND gRPC endpoint |
| `LND_CERT_PATH` | *required* | Path to LND TLS cert |
| `LND_MACAROON_PATH` | *required* | Path to LND admin macaroon |
| `LISTEN_ADDR` | `0.0.0.0:3402` | HTTP listen address |
| `INVOICE_AMOUNT_SATS` | `1` | Price per fortune, on both routes |
| `INVOICE_MEMO` | `Fortune cookie` | Invoice description (L402 only) |
| `INVOICE_EXPIRY_SECS` | `300` | Invoice expiry, on both routes |
| `L402_ROOT_KEY` | *random* | 32-byte hex key for token signing |
| `PUBLIC_URL` | *unset* | Public URL of the server, e.g. `https://fortune.example.com`. Enables `/x402` |
| `REPLAY_DB_PATH` | `fortune-402.db` | SQLite database of used x402 payments |
| `X402_CLOCK_SKEW_SECS` | `60` | Allowed clock difference for x402 invoice times |
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
Environment=PUBLIC_URL=https://fortune.example.com
StateDirectory=fortune-402
Environment=REPLAY_DB_PATH=/var/lib/fortune-402/replay.db
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
  -e PUBLIC_URL=https://fortune.example.com \
  -v fortune-402-data:/data \
  fortune-402
```

In the image, `REPLAY_DB_PATH` is `/data/fortune-402.db`. Mount a volume on
`/data` so used x402 payments are kept when the container is replaced.

Pre-built images are available from GitHub Container Registry:

```bash
docker pull ghcr.io/benthecarman/fortune-402:main
```

The image has a `HEALTHCHECK` that runs `fortune-402 health-check` every 30
seconds. It probes `/health` on the port from `LISTEN_ADDR` (default `3402`)
over loopback and exits non-zero if the server does not answer. The probe is a
separate process that reads the same environment variables as the server, so a
custom `-e LISTEN_ADDR` applies to both. The same command works outside Docker,
e.g. `LISTEN_ADDR=127.0.0.1:8080 fortune-402 health-check`.
