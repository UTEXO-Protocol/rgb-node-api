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

CI builds two images from the same Dockerfile:

| Image tag | Build | Use |
|-----------|-------|-----|
| `ghcr.io/utexo-protocol/rgb-node-api:<tag>` | read-only (default) | watch-only wallets, clients sign |
| `ghcr.io/utexo-protocol/rgb-node-api:<tag>-full` | `--features server-signing` | full wallets, the node signs (private deployments only) |

For a local full-wallet image, add `--build-arg FEATURES=server-signing`.

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
# Optional, PRIVATE DEPLOYMENTS ONLY. Needs a build with
# `cargo build --features server-signing` (not in the default build).
# The node can then keep a wallet's keys in memory (POST /wallet/init) and sign
# for it: /send, /burn, /inflate, /sendbtc and /createutxos. The seed phrase
# reaches the server, so keep the API on a private network (or behind TLS) and
# never expose it publicly. Callers must send the token in `x-signing-token`.
# allow_server_signing = true
# server_signing_token = "at-least-16-random-characters"
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
`/_swagger/swagger.yaml`. The API and signing CLI use rgb-lib `bfa.8`.

### System

| Method | Path | Description |
|--------|------|-------------|
| GET | `/healthcheck` | Service liveness check |
| GET | `/version` | App version info |
| GET | `/_swagger` | Swagger UI |
| GET | `/_swagger/swagger.yaml` | OpenAPI specification |

### Node/SDK API (`/wallet/`)

Watch-only, external signing. Auth via three headers: `xpub-van`, `xpub-col`, `master-fingerprint`.

The begin/end pattern allows clients to sign PSBTs externally:
1. Call `*begin` to get an unsigned PSBT
2. Sign the PSBT client-side
3. Call `*end` with the signed PSBT to broadcast

| Method | Path | Description |
|--------|------|-------------|
| POST | `/register` | Register a read-only wallet |
| POST | `/decodeinvoice` | Decode an RGB invoice (no wallet headers required) |
| POST | `/getreceivedmint` | Get a received mint checked by the wallet |
| POST | `/getassetmetadata` | Get asset metadata |
| POST | `/inspectpsbt` | Inspect PSBT inputs, outputs, fee and partial signatures |
| POST | `/address` | Get receive address |
| POST | `/btcbalance` | Get BTC balance |
| POST | `/listassets` | List all assets |
| POST | `/assetbalance` | Get balance for an asset |
| POST | `/listunspents` | List unspent outputs |
| POST | `/listtransactions` | List BTC transactions |
| POST | `/listtransfers` | List RGB transfers; optional `asset_id` and `batch_transfer_idx` filters |
| POST | `/blindreceive` | Generate RGB invoice (blinded UTXO) |
| POST | `/witnessreceive` | Generate RGB invoice paid to a new output of the sender's transaction |
| POST | `/issueassetnia` | Issue new NIA token |
| POST | `/issueassetifa` | Issue IFA tokens and reserve inflation rights |
| POST | `/issueassetbfa` | Issue a BFA with mint rights and an optional link right |
| POST | `/mintbegin` | Prepare a BFA mint (unsigned PSBT) |
| POST | `/mintend` | Finalize a BFA mint after the EVM lock is finalized |
| POST | `/linkbegin` | Prepare a link to another asset contract (unsigned PSBT) |
| POST | `/linkend` | Finalize an asset contract link (signed PSBT) |
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
| POST | `/getconsignment` | Get send/burn/link consignment by asset ID and TXID |
| POST | `/failtransfers` | Mark transfers as failed |
| POST | `/refresh` | Force wallet sync |
| POST | `/drop` | Remove wallet from memory |

### Full-wallet build (`--features server-signing`)

Private deployments only. The node keeps the keys of a wallet in memory and
signs for it, so a client (for example an exchange) does not run the begin/end
round trip. The three wallet headers stay the same. These routes need the
`x-signing-token` header, are off unless `allow_server_signing = true`, and
return 404 in a normal build.

The seed is **not stored**. After a restart, call `/init` again with the same
mnemonic. `/init` replaces a watch-only wallet with the same fingerprint.

| Method | Path | Description |
|--------|------|-------------|
| POST | `/init` | Make a full wallet (`{mnemonic?}`): restores it, or makes new keys. Returns the keys and the header values |
| POST | `/send` | Send RGB in one call (body of `/sendbegin`) |
| POST | `/burn` | Burn in one call (body of `/burnbegin`) |
| POST | `/inflate` | Inflate an IFA asset in one call (body of `/inflatebegin`) |
| POST | `/sendbtc` | Send BTC in one call (body of `/sendbtcbegin`) |
| POST | `/createutxos` | Create UTXOs in one call (body of `/createutxosbegin`) |

All routes are under `/wallet/`. On a watch-only wallet they return an error;
use the begin/end routes there.

A root-level `POST /blindreceive` is also registered as a legacy alias for
`POST /wallet/blindreceive`. It is marked for removal in the source and should
not be used — integrators should call `/wallet/blindreceive`.
