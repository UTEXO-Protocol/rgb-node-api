#!/usr/bin/env bash
# Boots esplora-adapter + rgb-node-api (pointed at the adapter) and runs e2e.py.
# Prereq: `just up` (bitcoind + electrs + rgb-proxy on regtest).
set -euo pipefail
cd "$(dirname "$0")/../.."
export E2E_DIR="$(mktemp -d)"
pids=()
cleanup() { for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done; rm -rf "$E2E_DIR"; }
trap cleanup EXIT

cargo build -q -p esplora-adapter -p rgb-node-api -p wallet-cli
cat > "$E2E_DIR/adapter.toml" <<T
electrum_address = "tcp://127.0.0.1:50001"
network = "regtest"
port = 3002
T
cat > "$E2E_DIR/node.toml" <<T
[api]
port = 34000
[wallet]
data_dir = "$E2E_DIR/rgb-data"
network = "Regtest"
btc_rpc_address = "127.0.0.1:18443"
btc_rpc_user = "dev"
btc_rpc_password = "dev"
indexer_address = "http://127.0.0.1:3002"
proxy_address = ["rpc://127.0.0.1:3000/json-rpc"]
T
RUST_LOG=info ./target/debug/esplora-adapter -c "$E2E_DIR/adapter.toml" > "$E2E_DIR/adapter.log" 2>&1 & pids+=($!)
RUST_LOG=info ./target/debug/rgb-node-api -c "$E2E_DIR/node.toml" > "$E2E_DIR/node.log" 2>&1 & pids+=($!)
until curl -sf localhost:3002/health >/dev/null && curl -sf localhost:34000/healthcheck >/dev/null; do sleep 1; done

# make sure the miner wallet exists and has mature coins
btc="docker compose exec -T bitcoind bitcoin-cli -regtest -rpcuser=dev -rpcpassword=dev"
$btc createwallet miner >/dev/null 2>&1 || $btc loadwallet miner >/dev/null 2>&1 || true
if [ "$($btc getblockcount)" -lt 101 ]; then
  $btc -rpcwallet=miner generatetoaddress 101 "$($btc -rpcwallet=miner getnewaddress)" >/dev/null
  sleep 5
fi

python3 esplora-adapter/tests/e2e.py || { echo "--- adapter log tail"; tail -20 "$E2E_DIR/adapter.log"; exit 1; }
