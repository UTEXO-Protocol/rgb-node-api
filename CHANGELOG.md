# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased] — local Fireblocks/RGB POC

### API compatibility

- `/wallet/sendbegin` requires `expiration_timestamp`, a future absolute Unix
  timestamp copied from the recipient invoice (earliest expiry for a batch),
  representable as a signed 64-bit value. Clients that previously omitted it
  must supply it; no arbitrary default is inferred.
- `/wallet/blindreceive` uses a one-hour expiry when `duration_seconds` is omitted
  or `null`. Neither means an unlimited invoice. Explicit durations are converted
  to absolute timestamps. These behaviors accompany the current pinned RGB API;
  see Swagger for request fields.

### Fixed

- External-wallet send limits are configurable under `wallet.mpc_send`; invoice and
  address checks use the configured Bitcoin network, including mainnet.
  Send journals retain their original policy across restarts/configuration
  changes; old journals retain the previous defaults.
- Removed the MPC restriction to test networks. Configuration supports mainnet,
  testnet (Testnet3), testnet4, signet and regtest; `testnet3` is an alias for
  `testnet`. Registration network names are normalized without changing the
  legacy `testnet` wire value. Invalid network names still fail closed.
- MPC state is pinned to its network/genesis under the existing process lock.
  Legacy state is adopted only after checking existing registration chains;
  starting the same state directory with another network is rejected.
- MPC wallet capabilities report configured schemas, including BFA when enabled,
  instead of always advertising NIA only.
- Provider identifiers are extensible without adding enum cases or HTTP routes;
  existing `dynamic_embedded` and `fireblocks_vault` wire values are preserved.
  Send eligibility is derived from the registered wallet shape, not the provider
  name. Additive `external_send` capabilities report `p2wpkh_blind` or `null`.
- Regression coverage exercises fresh wallets under three provider identifiers
  (Dynamic, Vault and a newly generated adapter name) with a new asset,
  cancellation, interrupted preparation recovery, external signatures and an
  ordinary native wallet receive on isolated regtest.
- Internal MPC wallets now run under independent locks with a bounded open-wallet
  cache. Broken registration records no longer stop healthy wallet refreshes.
- New interrupted invoice/send intents can recover the same saved operation when
  its identity is unambiguous. External-send cancellation is restricted to
  verified unsubmitted batches; unknown submissions remain blocked.
- Outgoing MPC history reports the recipient amount instead of wallet change;
  incoming history continues to report validated allocations.
- The legacy Vault helper requires the exact Iris transport endpoint, while
  preserving per-invoice nonce parameters.

The sibling `rgb-lib` Cargo path patch is intentionally retained for this POC.

## [0.2.0] - 2026-08-03

### Breaking

- Endpoints that answered with a bare string, number, boolean or `null` now
  return JSON objects:

  | endpoint                   | before          | after                |
  |----------------------------|-----------------|----------------------|
  | `/wallet/createutxosbegin` | `"cHNidP8B..."` | `{"psbt": "..."}`    |
  | `/wallet/sendbegin`        | `"cHNidP8B..."` | `{"psbt": "..."}`    |
  | `/wallet/createutxosend`   | `3`             | `{"created": 3}`     |
  | `/wallet/address`          | `"bcrt1q..."`   | `{"address": "..."}` |
  | `/wallet/failtransfers`    | `true`          | `{"changed": true}`  |
  | `/wallet/refresh`          | `null`          | `{}`                 |
  | `/wallet/drop`             | `null`          | `{}`                 |

- Failures no longer come back as `500 internal_error` by default. Invalid
  request data is a 4xx with a specific code — clients branching on the status
  or code must be updated.
- A missing wallet key header returns the standard JSON error envelope with
  code 401 `access_denied` instead of a `text/plain` body.

### Added

- Error codes for the cases the API can actually report: 1005
  `not_enough_assets`, 1006 `invalid_fee_rate`, 1007 `invalid_psbt`, 1008
  `invalid_recipient`, 1009 `conflict`, 1010 `unsupported`.
- Error responses carry `details.kind`, the underlying rgb-lib error variant
  (e.g. `InvalidFeeRate`), as a precise discriminator for clients.
- Swagger documents the error envelope, the full code table and the statuses
  each endpoint can return.

### Changed

- Every rgb-lib error is classified into an HTTP status and code: validation
  errors are 4xx with the rgb-lib message, unreachable dependencies (indexer,
  proxy, network) are 503, and only genuine server-side faults stay an opaque
  500.
- Client-caused failures are logged at `warn` instead of `error`, so operator
  logs are no longer full of other people's bad requests.
- `/wallet/blindreceive` treats `amount: 0` the same as an omitted amount — no
  amount restriction — instead of building an invoice demanding zero units.

### Fixed

- `/wallet/blindreceive` failed with `InvalidExpiration` on every request:
  `duration_seconds` was passed to rgb-lib as an absolute expiration timestamp,
  placing every invoice's expiry in 1970. It is now converted to
  `now + duration_seconds`.

### Notes

- This release also adds `wallet-cli`, a local signing helper that keeps keys
  off the node; the API itself remains watch-only.
