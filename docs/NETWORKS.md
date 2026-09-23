# Bitcoin network deployment

One API instance uses one configured network; all share the same API/library engine.

| `wallet.network` | Registration/API value |
|---|---|
| `mainnet` | `mainnet` |
| `testnet` or `testnet3` (Testnet3) | `testnet` |
| `testnet4` | `testnet4` |
| `signet` (default challenge) | `signet` |
| `regtest` | `regtest` |

Names are case-insensitive. Missing/unknown names are rejected; custom signet
challenges are unsupported.

## Configuration

Start with the [README configuration](../README.md#configuration), then set:

- `wallet.network` and a separate persistent `wallet.data_dir` per chain.
- `wallet.indexer_address` to a compatible Electrum/Esplora service for that
  chain; the resolver checks its network. Any Bitcoin RPC must match too.
- `wallet.proxy_address` to allowed RGB transports; no Iris endpoint is assumed.
- `wallet.mpc_send` for amount, fee, input, confirmation and expiry limits.
  Amounts are base units; all values must be positive and `max_inputs < 253`.
- Separate listen ports for concurrent instances.

Run `rgb-node-api -c <config-file>` with the protected `RGB_MPC_SERVICE_TOKEN`
environment variable to enable MPC. Mainnet needs no separate enable flag.
BFA additionally requires `bfa_enabled` and a trusted `eth_rpc_url`.

## State and validation

Registration validates network, genesis and public key/address. Testnet3,
Testnet4 and signet share `tb1…` prefixes, so addresses alone cannot identify the
chain. Invoices must match the configured network.

`<data_dir>/mpc/network.json`, protected by `service.lock`, prevents reopening
state on another chain. Legacy state is adopted only after registration-chain
checks; ambiguous/corrupt state is rejected. Keep the marker, databases and
journals together. Use new directories for other networks, never delete the
marker to migrate a wallet. Send policy and signature checks remain unchanged.

## Adapter limits and verification

Gateway/UI use env configuration; see the workspace's
`wallet-gateway/docs/NETWORK_CONFIGURATION.md`. The installed Dynamic SDK supports
mainnet/Testnet3/signet and rejects Testnet4/regtest. Faucet and legacy Vault
operators remain Testnet3-only.

Offline and unfunded HTTP tests cover all five networks, including wrong-chain
rejection. Funded tests use isolated regtest with local keys. Live provider,
endpoint and settlement acceptance is required for each deployment.
