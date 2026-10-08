# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Breaking

- `/wallet/sendbegin` requires `expiration_timestamp`: the recipient invoice's
  absolute Unix deadline (the earliest one for a batch). A missing, past or
  out-of-range value is rejected with `InvalidExpiration`.
- `/wallet/receive` invoices always expire: `duration_seconds: null` now means
  the default one hour instead of "never expires".

### Added

- Opt-in BFA support: set `bfa_enabled = true` and `eth_rpc_url` in `[wallet]`.
  Wallets then accept the BFA schema and validate BFA consignments against the
  EVM RPC. BFA stays disabled without the opt-in.
- IFA support on every network except mainnet, where rgb-lib does not allow
  IFA. No configuration is needed.
- `/wallet/issueassetifa` creates IFA tokens and reserves inflation rights.
- `/wallet/inflatebegin` and `/wallet/inflateend` create additional IFA tokens
  using the wallet's inflation rights and a PSBT signed by the client.
- `/wallet/burnbegin` and `/wallet/burnend` support burning BFA and IFA assets.
  BFA burns require a 32-byte `burn_recipient`, encoded as hex.
- `/wallet/getconsignment` returns the hex consignment of a send or burn by
  `asset_id` and `txid`.

### Changed

- Responses follow the pinned rgb-lib models: `listassets` has a `bfa` group,
  transfers can be `WaitingBroadcast`, allocations can be `BridgeRight` and
  UTXOs carry `derivation_index`.
- rgb-lib pinned to tag `v0.3.0-beta.43-bfa`, with its BFA RGB crate patches
  mirrored in `[patch.crates-io]`.

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
