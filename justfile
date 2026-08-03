default: list

list:
    just --list

install_dev_tools:
    cargo install cargo-outdated --locked --force
    cargo install taplo-cli --locked --force
    cargo install cargo-machete

check_deps:
    cargo machete
    cargo outdated -R

check:
    cargo fmt --all
    cargo check --workspace --all-targets
    cargo clippy --fix --workspace --all-targets -- -D warnings

# Non-mutating lint used by CI (see .github/workflows/ci.yaml).
ci-lint:
    cargo fmt --all --check
    cargo check --workspace --all-targets
    cargo clippy --workspace --all-targets -- -D warnings

fmt-cargo:
    taplo fmt -c .taplo.conf

fix: fmt-cargo
    cargo fmt --all
    cargo clippy --fix --allow-staged --allow-dirty --workspace --all-targets -- -D warnings
    cargo check --workspace --all-targets

### Local regtest stack: bitcoind + electrs + rgb-proxy (see docker-compose.yaml)

miner-wallet-name := "miner"
btc-cli := "docker compose exec -T bitcoind bitcoin-cli -regtest -rpcuser=dev -rpcpassword=dev"
btc-wallet-cli := btc-cli + " -rpcwallet=" + miner-wallet-name

# Create or load the miner wallet (idempotent).
miner-wallet:
    @{{btc-cli}} createwallet {{miner-wallet-name}} > /dev/null 2>&1 \
        || {{btc-cli}} loadwallet {{miner-wallet-name}} > /dev/null 2>&1 || true

# Print a fresh address owned by the miner wallet.
miner-address: miner-wallet
    @{{btc-wallet-cli}} getnewaddress

# Boot the local stack and pre-mine 101 blocks to the miner wallet.
up:
    docker compose up -d
    @echo "Waiting for bitcoind RPC..."
    @until {{btc-cli}} getblockchaininfo > /dev/null 2>&1; do sleep 1; done
    @just miner-wallet
    @blocks=$({{btc-cli}} getblockcount); \
        if [ "$blocks" -lt 101 ]; then \
            addr=$({{btc-wallet-cli}} getnewaddress); \
            echo "Pre-mining $((101 - blocks)) blocks to $addr..."; \
            {{btc-wallet-cli}} generatetoaddress $((101 - blocks)) "$addr" > /dev/null; \
        else \
            echo "Chain already at height $blocks, skipping pre-mine."; \
        fi
    @echo "Miner balance: $({{btc-wallet-cli}} getbalance) BTC"
    @echo "Local stack ready. Run: just run"

down:
    docker compose down

# Full wipe — removes volumes (chain state).
reset:
    docker compose down -v

logs SERVICE='':
    docker compose logs -f {{SERVICE}}

gen-blocks N='1': miner-wallet
    @{{btc-wallet-cli}} generatetoaddress {{N}} "$({{btc-wallet-cli}} getnewaddress)"

### Run the service

run:
    cargo run -- -c ./local.uwallet.toml

new-keys:
    cargo run -p wallet-cli -- new

# Sign a PSBT from stdin: `just sign <<< "<base64-psbt>"` (needs RGB_MNEMONIC).
sign:
    cargo run -q -p wallet-cli -- sign

# Send BTC from the miner wallet to an address and confirm it.
send-btc ADDRESS AMOUNT='1.0': miner-wallet
    {{btc-wallet-cli}} sendtoaddress {{ADDRESS}} {{AMOUNT}}
    @just gen-blocks 1 > /dev/null
