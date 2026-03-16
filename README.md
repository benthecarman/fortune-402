# fortune-402

A Lightning-powered fortune cookie server. Pay 1 sat, get a fortune.

Implements the [L402 protocol](https://docs.lightning.engineering/the-lightning-network/l402) — HTTP 402 Payment Required with Lightning invoices.

## How it works

1. `GET /fortune` → server returns HTTP 402 with a `www-authenticate` header containing a macaroon token and a Lightning invoice:
   ```
   www-authenticate: L402 token="<macaroon>", invoice="<bolt11>"
   ```
2. Pay the invoice, get the preimage
3. `GET /fortune` with `Authorization: L402 <macaroon>:<preimage>` → receive your fortune

```bash
# Step 1: Request a fortune, get the L402 challenge
$ curl -si http://localhost:3402/fortune
HTTP/1.1 402 Payment Required
www-authenticate: L402 token="<macaroon>", invoice="lnbc10n1p..."

# Step 2: Pay the invoice from the header and note the preimage

# Step 3: Use the macaroon (from the token field) + preimage to authenticate
$ curl -s -H "Authorization: L402 <macaroon>:<preimage>" http://localhost:3402/fortune | jq
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
