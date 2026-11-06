# rgb-node-api

Standalone HTTP service for managing **watch-only** RGB wallets. The service holds
no private keys and never signs transactions — it is designed to be a self-hosted semi-public
API. Clients register watch-only wallets with their xPubs, read wallet state, and
use the begin/end flow to sign PSBTs client-side.

- **Node/SDK API** (`/wallet/`) -- register watch-only wallets, read state, and use
  begin/end endpoints for external signing (the client holds the keys)

## Running

```bash
cargo run -- -c config.toml
```

A local regtest stack (bitcoind + electrs + rgb-proxy) is available via `just up`; then `just run` starts the service with `local.uwallet.toml`.

### Building the Docker image

Set the GitHub Actions secret `ORG_REPO_READ_TOKEN` with Contents read access
to `UTEXO-Protocol/rgb-lib-bfa`. Its RGB dependencies use public `rgb-protocol` repositories.
CI passes it to Docker as a BuildKit secret.

For a local build, export `ORG_REPO_READ_TOKEN` in your shell and run:

```bash
docker buildx build --load \
  --secret id=github_token,env=ORG_REPO_READ_TOKEN \
  -t rgb-node-api:local .
```

The token is available only during the Cargo build and is not stored in the image.

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
# Optional BFA (non-mainnet): EVM RPC must serve finalized bridge state.
# bfa_enabled = true
# eth_rpc_url = "http://127.0.0.1:8545"
# eth_chain_id = 11155111
# eth_min_mint_lock_blocks = 2
# eth_max_mint_lock_blocks = 1000
```

For BFA, all four `eth_*` fields are required. Choose the mint lock bounds for
your EVM chain's block time. The maximum bounds how long a received mint can
reserve a UTXO, plus finalization time; it must be at least the minimum.
A minimum below 2 acts as 2; a maximum below 2 rejects all mints.

Wallets are not configured in the file — they are registered at runtime via
`POST /wallet/register` with the `xpub-van`, `xpub-col`, and `master-fingerprint`
headers.

## API Reference

Swagger UI is available at `/_swagger`; the OpenAPI specification is at
`/_swagger/swagger.yaml`. The API and signing CLI use rgb-lib `bfa.6`.

### System

| Method | Path | Description |
|--------|------|-------------|
| GET | `/healthcheck` | Service liveness check |
| GET | `/version` | App version info |

### Node/SDK API (`/wallet/`)

Wallet operations use three headers: `xpub-van`, `xpub-col`, `master-fingerprint`.
`/wallet/decodeinvoice` requires no wallet headers or registration.

The begin/end pattern allows clients to sign PSBTs externally:
1. Call `*begin` to get an unsigned PSBT
2. Sign the PSBT client-side
3. Call `*end` with the signed PSBT to broadcast

| Method | Path | Description |
|--------|------|-------------|
| POST | `/register` | Register a read-only wallet |
| POST | `/decodeinvoice` | Decode an RGB invoice |
| POST | `/address` | Get receive address |
| POST | `/btcbalance` | Get BTC balance |
| POST | `/listassets` | List all assets |
| POST | `/assetbalance` | Get balance for an asset |
| POST | `/listunspents` | List unspent outputs |
| POST | `/listtransactions` | List BTC transactions |
| POST | `/listtransfers` | List RGB transfers for an asset |
| POST | `/blindreceive` | Generate RGB invoice (blinded UTXO) |
| POST | `/witnessreceive` | Generate RGB invoice paid to a new output of the sender's transaction |
| POST | `/issueassetnia` | Issue new NIA token |
| POST | `/issueassetifa` | Issue IFA tokens and reserve inflation rights |
| POST | `/inflatebegin` | Create additional IFA tokens (unsigned PSBT) |
| POST | `/inflateend` | Finalize IFA inflation (signed PSBT) |
| POST | `/createutxosbegin` | Create UTXOs (unsigned PSBT) |
| POST | `/createutxosend` | Finalize UTXO creation (signed PSBT) |
| POST | `/sendbtcbegin` | Send BTC (unsigned PSBT) |
| POST | `/sendbtcend` | Finalize BTC send (signed PSBT) |
| POST | `/sendbegin` | Send RGB token (unsigned PSBT) |
| POST | `/sendend` | Finalize token send (signed PSBT) |
| POST | `/burnbegin` | Burn BFA or IFA tokens (unsigned PSBT) |
| POST | `/burnend` | Finalize token burn (signed PSBT) |
| POST | `/getconsignment` | Get send/burn consignment by asset ID and TXID |
| POST | `/failtransfers` | Mark transfers as failed |
| POST | `/refresh` | Force wallet sync |
| POST | `/drop` | Remove wallet from memory |

A root-level `POST /blindreceive` is also registered as a legacy alias for
`POST /wallet/blindreceive`. It is marked for removal in the source and should
not be used — integrators should call `/wallet/blindreceive`.

### Decode an invoice

```bash
curl -X POST http://127.0.0.1:34000/wallet/decodeinvoice \
  -H 'Content-Type: application/json' \
  -d '{"invoice":"rgb:~/~/~/bc:utxob:4vm1CX2Z-K8hMo59-e7dgGBS-Jka7mYn-Xe~yP85-yUiHHxr-aVlYa"}'
```

```json
{
  "recipient_id": "bc:utxob:4vm1CX2Z-K8hMo59-e7dgGBS-Jka7mYn-Xe~yP85-yUiHHxr-aVlYa",
  "asset_schema": null,
  "asset_id": null,
  "assignment": "Any",
  "assignment_name": null,
  "network": "Mainnet",
  "expiration_timestamp": null,
  "transport_endpoints": [],
  "public_key": null,
  "encryption": "Required",
  "unknown_query_params": {}
}
```

Decoding parses the invoice; it does not check whether it has expired or been paid.
Malformed invoice strings return HTTP 400 with `error.code = 1008`. Missing or malformed
JSON fields return HTTP 422.
