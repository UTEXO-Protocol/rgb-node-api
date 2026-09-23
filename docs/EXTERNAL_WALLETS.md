# External wallet integration contract

Gateway/frontend adapters verify identity/ownership, register public metadata and
obtain signatures. `rgb-lib` handles RGB state and transaction construction through
`MpcWalletProvider`; compatible providers share the API and send engine.

## Identity and capabilities

Registration `provider` matches `[a-z][a-z0-9_-]{0,63}`. Existing
`dynamic_embedded` and `fireblocks_vault` values remain valid. New Rust adapters
use `Provider::try_from(String)`.
Wallet owner, provider, key and network bindings remain immutable; renaming a
provider cannot bypass duplicate-address checks or establish customer approval.

Capabilities follow registration and server configuration:

| Field | Meaning |
|---|---|
| `witness_receive` | Public-address witness receiving is supported. |
| `supported_schemas` | Server-enabled RGB schemas. |
| `signing: false` | No server-side signer. |
| `external_send: "p2wpkh_blind"` | One RGB P2WPKH address, no separate fee address, blind recipient and owned change. |
| `external_send: null` | Sending is unsupported for this wallet shape; receiving may remain available. |

Clients must tolerate new fields/provider identifiers. Capabilities do
not guarantee funding or valid signatures; P2TR receiving does not imply sending.

## Send flow

All routes are under `/internal/mpc`:

1. Register verified metadata with `POST /wallets` through the authenticated Gateway.
2. Call `/wallets/{id}/sends/prepare` with a stable `request_id`, invoice and
   integer-string amount in asset base units.
3. Sign the returned Base64 PSBT/input indexes using `SIGHASH_ALL`. Return a PSBT
   with partial ECDSA signatures or finalized P2WPKH witnesses; preserve the transaction.
   Raw-signature SDKs need an adapter that assembles the signed PSBT.
4. Submit `signed_psbt` to `/wallets/{id}/sends/{request_id}/finish`. The API checks
   transaction, inputs, signatures, fees and outputs, then restores saved RGB metadata.
5. Recover using the same request. Cancel only unsubmitted preparations; a timeout
   must not create a replacement or release reserved inputs.

`wallet.mpc_send` policy is saved per operation; legacy journals retain previous
defaults. Network, transport and owner checks apply to every provider; see
[network configuration](NETWORKS.md).

Compatible P2WPKH providers need only Gateway/auth/signing adapters. P2TR send,
multisig, multiple fee addresses or witness destinations need a new shared core
capability. Keep shared engines when
removing a provider. Local-key fixtures do not replace live provider acceptance;
the existing Vault deployment still uses operator helpers.
