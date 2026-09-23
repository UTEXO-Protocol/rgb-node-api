# rgb-node-api

Standalone HTTP service for managing **watch-only** RGB wallets. The service holds
no private keys and never signs transactions — it is designed to be a self-hosted semi-public
API. Clients register watch-only wallets with their xPubs, read wallet state, and
use the begin/end flow to sign PSBTs client-side.

- **Node/SDK API** (`/wallet/`) -- register watch-only wallets, read state, and use
  begin/end endpoints for external signing (the client holds the keys)
- **Internal MPC API** (`/internal/mpc/`) -- Gateway-authenticated public-key registration,
  witness invoices, NIA/BFA receiving (BFA opt-in) and capability-based external sends.

`GET /internal/mpc/wallets/{id}/assets` returns nullable `btc_balance`: `colored`
and `vanilla`, each with exact satoshi strings for `settled`, `future`, `spendable`.
These preserve rgb-lib accounting (currently equal UTXO totals per keychain).
Unavailable BTC is `null`; no confirmed/pending classification is added.

The MPC POC currently uses the sibling `../rgb-lib` checkout via an explicit Cargo
patch. Keep both repositories together; the baseline git pin remains in Cargo.toml.
CI checks out the library commit pinned by `RGB_LIB_REV` in its workflow and requires
the `BFA_MIRRORS_TOKEN` Actions secret with read access to the three private BFA
mirrors. Replace the local Cargo override before a standalone Docker release.

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
network = "regtest" # See docs/NETWORKS.md for supported networks
btc_rpc_address = "127.0.0.1:18443"
btc_rpc_user = "dev"
btc_rpc_password = "dev"
indexer_address = "tcp://127.0.0.1:50001"
proxy_address = ["rpc://127.0.0.1:3000/json-rpc"]
# Optional BFA support:
# bfa_enabled = true
# eth_rpc_url = "<trusted EVM RPC URL>"

# Optional external-send policy (defaults):
[wallet.mpc_send]
max_amount = 25
fee_rate_sat_vb = 2
max_fee_sat = 2000
max_inputs = 10
min_confirmations = 1
min_invoice_validity_secs = 120
```

Wallets are not configured in the file — they are registered at runtime via
`POST /wallet/register` with the `xpub-van`, `xpub-col`, and `master-fingerprint`
headers.

Send journals retain their policy across configuration changes; older journals
use the defaults above. Reconcile pending sends before changing proxy settings.
Use separate instances/state directories per [Bitcoin network](docs/NETWORKS.md).

External sends support one RGB P2WPKH address and blind recipients, with commitment
and owned-change outputs. Wallet `external_send` reports `"p2wpkh_blind"` or `null`;
`signing: false` means no server signer. See the
[provider contract and limits](docs/EXTERNAL_WALLETS.md).

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
| POST | `/issueassetbfa` | Issue BFA genesis with bridge rights and zero token supply (opt-in) |
| POST | `/bridgebegin` | Prepare BFA mint PSBT and operation ID for EVM evidence |
| POST | `/bridgeend` | Broadcast the externally signed saved mint PSBT |
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

BFA requires `wallet.bfa_enabled = true` and `wallet.eth_rpc_url` for xpub/MPC
wallets, including ordinary incoming transfers. Recover uncertain operations from
their saved transactions. BFAMOCK uses synthetic evidence without ERC-20 backing;
see the workspace's `wallet-gateway/docs/BFA_MOCK.md`.
