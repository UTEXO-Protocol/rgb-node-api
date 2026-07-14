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
    cargo check --all-targets
    cargo clippy --fix --all-targets -- -D warnings

# Non-mutating lint used by CI (see .github/workflows/ci.yaml).
ci-lint:
    cargo fmt --all --check
    cargo check --all-targets
    cargo clippy --all-targets -- -D warnings

fmt-cargo:
    taplo fmt -c .taplo.conf

fix: fmt-cargo
    cargo fmt --all
    cargo clippy --fix --allow-staged --allow-dirty --all-targets -- -D warnings
    cargo check --all-targets

### Local regtest stack: bitcoind + electrs + rgb-proxy (see docker-compose.yaml)

miner-address := "bcrt1pjke58jesf3z0yct82nk3aqck2euywk759dpqvtmqu8q7s8qur7jqdfmwk3"
btc-cli := "docker compose exec -T bitcoind bitcoin-cli -regtest -rpcuser=dev -rpcpassword=dev"

# Boot the local stack and pre-mine 101 blocks.
up:
    docker compose up -d
    @echo "Waiting for bitcoind RPC..."
    @until {{btc-cli}} getblockchaininfo > /dev/null 2>&1; do sleep 1; done
    @blocks=$({{btc-cli}} getblockcount); \
        if [ "$blocks" -lt 101 ]; then \
            echo "Pre-mining $((101 - blocks)) blocks to {{miner-address}}..."; \
            {{btc-cli}} generatetoaddress $((101 - blocks)) {{miner-address}} > /dev/null; \
        else \
            echo "Chain already at height $blocks, skipping pre-mine."; \
        fi
    @echo "Local stack ready. Run: just run"

down:
    docker compose down

# Full wipe — removes volumes (chain state).
reset:
    docker compose down -v

logs SERVICE='':
    docker compose logs -f {{SERVICE}}

gen-blocks N='1':
    {{btc-cli}} generatetoaddress {{N}} {{miner-address}}

### Run the service

run:
    cargo run -- -c ./local.uwallet.toml

new-keys:
    cargo run --bin wallet-cli -- new
