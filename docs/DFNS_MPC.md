# External two-role P2TR API, version 1

2026-09-24. This clean branch starts from `95cb8c5715fa7f7f0fc86ce01e9d3bdcaceb33f0` (`main`). It contains the registry and external-signing capability required by Dfns; no provider SDK, passkey, Dynamic or Vault adapter runs in this server. Provider authorization belongs in Gateway. NIA is enabled by default. The existing explicit BFA configuration is preserved; BFAMOCK remains mock-backed.

## Authentication and registration

All routes below are under `/internal/mpc`. Require `Authorization: Bearer <service token>`, `X-Tenant-Id`, and `X-User-Id`. Configure the token with `RGB_MPC_SERVICE_TOKEN_FILE` (owner-only permissions). The browser never receives this credential. Other owners receive no wallet or operation data. HTTP responses use `Cache-Control: no-store`.

`POST /wallets` accepts an immutable registration:

```json
{
  "wallet_id": "<UUID>",
  "provider": "dfns",
  "provider_environment": "<deployment identity>",
  "provider_wallet_ref": "<stable provider binding>",
  "bitcoin_network": "signet",
  "genesis_hash": "<64 hex characters>",
  "addresses": [
    {"role":"rgb","script_type":"p2tr","address":"<bech32m>","public_key":"<tweaked x-only output key>","internal_key":"<untweaked x-only key>","provider_wallet_id":"<rgb wallet>","signing_key_id":"<rgb key>"},
    {"role":"fee","script_type":"p2tr","address":"<bech32m>","public_key":"<tweaked x-only output key>","internal_key":"<untweaked x-only key>","provider_wallet_id":"<fee wallet>","signing_key_id":"<fee key>"}
  ]
}
```

Exactly two distinct wallets, keys and scripts are required. The API verifies network/genesis, address/script and BIP341 tweak relationships. Gateway must independently authenticate provider ownership/delegation before registration. No xpub or private key is fabricated. The advertised profile is `p2tr_two_role_blind_v1`; registration advertises external signing, never internal signing.

## Wallet and send contract

Paths are relative to `/wallets/{wallet_id}`:

| Method / path | Body or result |
|---|---|
| GET `/` | Immutable registration and capabilities |
| GET `/assets` | Native RGB asset, six colored/vanilla BTC balance fields, and nullable `has_receive_utxo` UI hint |
| GET `/transfers` | Wallet transfers |
| POST `/refresh` | Synchronize and reconcile |
| POST `/blind-invoices` | `{request_id, asset_id?, amount?, expiration_timestamp}`; amount is decimal base units, expiry is Unix seconds |
| POST `/utxos/prepare` | `{request_id, num?:5, size?:1000, up_to?:false}` |
| GET `/utxos/{request_id}` | Reconcile saved pool preparation |
| POST `/utxos/{request_id}/finish` | `{signed_psbt}`; verified Base64 PSBT |
| POST `/utxos/{request_id}/cancel` | Cancel before submission |
| POST `/sends/prepare` | `{request_id, asset_id, invoice, amount}`; UUID intent, enabled-schema blind recipient only |
| GET `/sends/{request_id}` | Reconcile the saved operation |
| POST `/sends/{request_id}/finish` | `{signed_psbt}`; same complete transaction, Base64 PSBT |
| POST `/sends/{request_id}/cancel` | Cancel only an unsubmitted operation |

A send response contains `version:1`, `wallet_id`, `request_id`, `asset_id`, `invoice`, decimal-string `amount`, `state`, optional `message`, `expires_at`, `txid`, `fee_sat`, Base64 `psbt`, `key_groups:[{signing_key_id,input_indexes}]` and `change:[{role,vout,amount_sat}]`. Group indexes cover every input exactly once. New sends retain at most one Internal change output, carrying BTC and any RGB remainder together. Internal RGB inputs are checked against saved allocations and excluded from plain BTC funding. The `fee` role names the registered Internal key, not an assurance that its outputs are RGB-free. Previously saved split transactions retain their original validation policy for recovery. Fee rate, fee ceiling, maximum amount/inputs, confirmations and preparation expiry margin come from `[wallet.mpc_send]`.

Blind receive supports the first unknown contract by persisting a generic receive and binding the public invoice to the immutable requested contract. Fund Internal once, prepare External UTXOs through `/utxos/prepare` → delegated signing → `/finish`, and refresh before creating the first invoice. Invoice creation syncs the funded UTXO; an empty wallet cannot blind-receive. An invoice alone never moves funds. Repeating the same request returns/reconstructs the same receive, even after a lost response; a different payload with the same ID is rejected.

Pool preparation retains the library defaults (five 1000-sat outputs; fewer may fit the available budget). `num` is 1–251 and `size` at least 330 sat; `up_to` uses existing library allocation-pool semantics. Only unallocated, unreserved Internal inputs may fund it. Review returns every output as `{role,vout,amount_sat}`, plus `num`, `size`, `up_to`, `created`, transaction ID, fee and key groups. Fee/input ceilings use `[wallet.mpc_send]`. All External outputs reuse the registered External script; optional change is one Internal output.

The exact dry-run preview and prior reservations are persisted before committing `create_utxos_begin`. Restart exposes it for signing only if the matching reservation exists. A changed coin set between preview and commit enters `NEEDS_REVIEW` and preserves reservations; it never returns an unverified replacement PSBT. No committed reservation means `FAILED`. This conservative edge case needs operator reconciliation, not journal deletion. Finish saves verified signatures before broadcast; `SUBMITTING` recovery replays only that signed transaction. `COMPLETED` means broadcast/import completed, not Bitcoin confirmation. Cancellation and its lost response are idempotent; unknown broadcast outcomes cannot be cancelled. `has_receive_utxo` is only a UI hint; `blind_receive` checks current allocation eligibility authoritatively.

Prepare saves the original PSBT, transfer identity, exact node-verified prevouts and RGB allocations. A known rejection before a transfer exists is saved as `FAILED`, with no PSBT, so a new corrected intent can proceed. Unknown outcomes retain their existing journal. Prepared states progress through `AWAITING_SIGNATURE`, `SUBMITTING`, `WAITING_COUNTERPARTY`, `WAITING_CONFIRMATIONS`, `SETTLED`; `PREPARING` is reconciled from saved artifacts. Cancellation/known failure are terminal.

Finish accepts valid key-path Schnorr signatures, including finalized one-item witnesses. It checks original input order/outpoints, outputs, fees, Taproot output keys and supported sighash; RGB proprietary metadata comes from the saved PSBT. All signatures must be present. The fully verified final PSBT is persisted before submission. Restart replays only that exact transaction when the original library batch is still Initiated. Duplicate finish/status never constructs a replacement transfer.

Do not delete wallet databases, JSON journals, consignments or prepared-input records. The process lock prevents competing servers; per-wallet serialization prevents simultaneous access. Unknown outcomes are not an invitation to reset state.

## Build and checks

API and `wallet-cli` share exact `rgb-lib` revision `7d2dcfa286c360489614db9ed7be079a2f50d245` in `[workspace.dependencies]`. Delivery must not use `.cargo/config.toml` or a sibling path override. The upstream library requires read access to `rgb-consensus-s-bfa`, `rgb-ops-s-bfa` and `rgb-schemas-s-bfa` even for NIA. CI expects the existing `BFA_MIRRORS_TOKEN` secret with read-only access to those mirrors (and the library if private). No token is stored in Git URLs/config. Hosted CI cannot pass without that organization/repository secret.

```sh
cargo fmt --all -- --check
cargo test --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets -- -D warnings
# Optional container build: token stays in a BuildKit secret mount.
docker build --secret id=bfa_mirrors_token,src=/protected/git-read.token .
```

The ordinary test suite does not contact Dfns. The integration fixture below creates fresh local native/MPC wallets and test keys, signs locally, and mines only disposable regtest coins:

```sh
docker compose -p dfns-isolated -f tests/dfns/compose.yaml up -d
RGB_DFNS_REGTEST=1 cargo test --locked --test dfns_regtest -- --ignored --nocapture
# After tests, stop only this project; no existing POC services are involved.
docker compose -p dfns-isolated -f tests/dfns/compose.yaml down
```

The fixture uses fresh keys each run, tests one Internal deposit → pool preparation → blind receive 25 → sends 10 and 5 → remainder 10, pool cancel/lost-prepare/lost-submit recovery, lost-invoice-response recovery, two independent signing groups on the first send and one on the second, repeated Internal RGB change, unrelated allocations and exhaustion, protection against plain BTC spending of RGB change, spending a separately funded Internal UTXO, prepare/submit crash recovery, bad signatures and duplicate finish. Registry tests cover immutable bindings, restart and owner isolation. This is not live Dfns acceptance.
