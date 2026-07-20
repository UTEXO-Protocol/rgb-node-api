# rgb-node-api

Standalone HTTP service for managing **watch-only** RGB wallets. The service holds
no private keys and never signs transactions — it is designed to be a semi-public
API. Clients register watch-only wallets with their xPubs, read wallet state, and
use the begin/end flow to sign PSBTs client-side.

- **Node/SDK API** (`/wallet/`) -- register watch-only wallets, read state, and use
  begin/end endpoints for external signing (the client holds the keys)

## Running

```bash
cargo run -- -c config.toml
```

A local regtest stack (bitcoind + electrs + rgb-proxy) is available via `just up`; then `just run` starts the service with `local.uwallet.toml`.

### Configuration

```toml
[api]
listen_address = "127.0.0.1"
port = 34000

[wallet]
data_dir = "./rgb-data"
network = "regtest"
btc_rpc_address = "127.0.0.1:18443"
btc_rpc_user = "dev"
btc_rpc_password = "dev"
indexer_address = "tcp://127.0.0.1:50001"
proxy_address = ["rpc://127.0.0.1:3000/json-rpc"]
```

Wallets are not configured in the file — they are registered at runtime via
`POST /wallet/register` with the `xpub-van`, `xpub-col`, and `master-fingerprint`
headers.

## API Reference

Swagger UI is available at `/_swagger` when the server is running.

### System

| Method | Path | Description |
|--------|------|-------------|
| GET | `/healthcheck` | Service liveness check |
| GET | `/version` | App version info |

### Node/SDK API (`/wallet/`)

Watch-only, external signing. Auth via three headers: `xpub-van`, `xpub-col`, `master-fingerprint`.

The begin/end pattern allows clients to sign PSBTs externally:
1. Call `*begin` to get an unsigned PSBT
2. Sign the PSBT client-side
3. Call `*end` with the signed PSBT to broadcast

| Method | Path | Description |
|--------|------|-------------|
| POST | `/register` | Register a read-only wallet |
| POST | `/address` | Get receive address |
| POST | `/btcbalance` | Get BTC balance |
| POST | `/listassets` | List all assets |
| POST | `/assetbalance` | Get balance for an asset |
| POST | `/listunspents` | List unspent outputs |
| POST | `/listtransactions` | List BTC transactions |
| POST | `/listtransfers` | List RGB transfers for an asset |
| POST | `/blindreceive` | Generate RGB invoice |
| POST | `/issueassetnia` | Issue new NIA token |
| POST | `/createutxosbegin` | Create UTXOs (unsigned PSBT) |
| POST | `/createutxosend` | Finalize UTXO creation (signed PSBT) |
| POST | `/sendbegin` | Send RGB token (unsigned PSBT) |
| POST | `/sendend` | Finalize token send (signed PSBT) |
| POST | `/failtransfers` | Mark transfers as failed |
| POST | `/refresh` | Force wallet sync |
| POST | `/drop` | Remove wallet from memory |

A root-level `POST /blindreceive` is also registered as a legacy alias for
`POST /wallet/blindreceive`. It is marked for removal in the source and should
not be used — integrators should call `/wallet/blindreceive`.
