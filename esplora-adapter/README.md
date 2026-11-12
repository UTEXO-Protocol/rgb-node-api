# esplora-adapter

HTTP Esplora REST API in front of an Electrum (TCP) server such as electrs.
Browsers cannot open TCP sockets; this lets them use electrs through plain HTTP.

    just up && just esplora-adapter      # serves http://127.0.0.1:3002

Then set `indexer_address = "http://127.0.0.1:3002"` in rgb-node-api, or point any
Esplora client at that URL. CORS is on by default (`cors_domain` to restrict it).

Supported: `/blocks[/:h]`, `/blocks/tip/{height,hash}`, `/block-height/:h`, `/block/:hash/{header,status}`,
`/tx/:txid[/status|/hex|/raw|/merkle-proof|/outspend/:vout|/outspends]`, `POST /tx`,
`/address/:a/{txs,txs/mempool,txs/chain[/:last],utxo}`, `/scripthash/:h/...` (same), `/fee-estimates`, `/health`.

Tested end to end with rgb-node-api (rgb-lib bfa.8) on regtest: register, fund, create UTXOs, issue NIA,
blind send, witness send, BTC send, settle. All pass.

Limits (Electrum cannot answer these):
- Other `/block/:hash/*` routes return 501. Block info has `size` and `weight` = 0 (Electrum has no such data). `tx_count` is real, found by probing.
- Block-by-hash lookup scans back at most 5000 blocks from the tip.
- Address lists return 50 mempool + 25 confirmed txs per page; address/scripthash stats are not implemented.
- A confirmed tx's height is found via the history of one of its scripts (OP_RETURN is not indexed by electrs).

## Public deployment

Built-in safeguards (all configurable, see `esplora-adapter.toml.example`): per-IP rate limit (429 + `Retry-After`),
cap on requests in flight (503), request timeout (504), a pool of Electrum connections, and an in-memory tx cache
that cuts repeat lookups. Still put nginx or Cloudflare in front for TLS, response caching and DDoS cover; then set
`trust_forwarded_for = true` so limits use the real client IP, and set `cors_domain` to your app's origin.
One address with a huge history stays costly: `/tx/:txid/outspend` scans up to 200 history entries.

## Configuration from the environment

Each TOML setting has an env var: `ESPLORA_` plus the field name in capitals, e.g. `ESPLORA_ELECTRUM_ADDRESS`,
`ESPLORA_NETWORK`, `ESPLORA_RATE_LIMIT_PER_SEC`, `ESPLORA_CORS_DOMAIN`, `ESPLORA_REQUEST_TIMEOUT_SECS`.
Env wins over the file, and the file may be missing. Handy for Docker.
