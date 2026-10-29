# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Breaking

- rgb-lib moved to `UTEXO-Protocol/rgb-lib-bfa` tag
  `v0.3.0-beta.7-upstream-dev-bfa.3`. Its wallet database history diverges
  from the previous `0.3.0-beta.46-bfa` tag: use a fresh `data_dir`, do not
  open wallets created by the previous build. It speaks proxy protocol v1,
  so the RGB proxy must be `rgb-proxy-server` 0.4.0 or later.
- BFA config needs `eth_chain_id` next to `eth_rpc_url`, and BFA is rejected on
  mainnet.
- `/wallet/sendbegin` recipients carry their invoice's `public_key`,
  `encryption` (default `Required`, as for an invoice without `enc`) and
  `expiration_timestamp`; rgb-lib needs the key and the expiration for proxy
  recipients. The transfer's `expiration_timestamp` is optional and defaults
  to the earliest invoice expiration, minus 10 minutes only for non-donations.
  An explicit one must be at least 5 minutes ahead and, for non-donation proxy
  transfers, at least 10 minutes before the earliest invoice expiration.
- Responses follow the new rgb-lib models: BFA uses mint rights
  (`MintRight` allocations, `Mint` transfers, `mint` in assignment totals)
  instead of bridge and link rights; transfers gain
  `sender_transport_endpoint` and lose `proxy_recipient_id`; receive data gains
  `public_key`; IFA link fields are gone.
- `/wallet/burnbegin` passes `burn_recipient` to rgb-lib as the burn reason
  (also accepted as `burn_reason`): 32 bytes, hex, optional `0x`.
- `/wallet/blindreceive` invoices always expire: omitted or null
  `duration_seconds` uses a two-hour default. This leaves time to start a BFA
  mint, which requires at least one hour remaining on the invoice. Explicit
  durations are preserved.

### Added

- `/wallet/sendbtcbegin` and `/wallet/sendbtcend` send BTC from the wallet's
  vanilla side with a PSBT signed by the client; the end call returns the
  broadcast `txid`.
- `/wallet/witnessreceive` creates an RGB invoice paid to a new output of the
  sender's transaction, so the wallet needs no colorable UTXO up front. It
  takes the same body as `/wallet/blindreceive`.
- Opt-in BFA support: set `bfa_enabled = true`, `eth_rpc_url` and `eth_chain_id`
  in `[wallet]`.
  Wallets then accept the BFA schema and validate BFA consignments against the
  EVM RPC. BFA stays disabled without the opt-in.
- IFA support on every network except mainnet, where rgb-lib does not allow
  IFA. No configuration is needed.
- `/wallet/issueassetifa` creates IFA tokens and reserves inflation rights.
- `/wallet/inflatebegin` and `/wallet/inflateend` create additional IFA tokens
  using the wallet's inflation rights and a PSBT signed by the client.
- `/wallet/burnbegin` and `/wallet/burnend` support burning BFA and IFA assets.
  An optional 32-byte hex burn reason is accepted as `burn_recipient` or
  `burn_reason`.
- `/wallet/getconsignment` returns the hex consignment of a send or burn by
  `asset_id` and `txid`.

### Changed

- Responses follow the pinned rgb-lib models: `listassets` has a `bfa` group,
  transfers can be `WaitingBroadcast`, allocations can be `MintRight` and
  UTXOs carry `derivation_index`.
- rgb-lib pinned to `rgb-lib-bfa` tag `v0.3.0-beta.7-upstream-dev-bfa.3`. BFA
  is in the published RGB crates, so `[patch.crates-io]` is gone.

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
