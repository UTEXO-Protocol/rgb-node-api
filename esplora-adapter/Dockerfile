# syntax=docker/dockerfile:1
# Build from the repo root:
#   docker buildx build --load --secret id=github_token,env=ORG_REPO_READ_TOKEN \
#     -f esplora-adapter/Dockerfile -t esplora-adapter:local .
FROM rust:1.96 AS builder

WORKDIR /opt/utexo

COPY . .

RUN apt-get update && apt-get install -y build-essential \
        cmake \
        git \
        pkgconf

# The workspace resolves private git dependencies (used by the other members)
# even when only this crate is built, so the same BuildKit secret is needed.
RUN --mount=type=secret,id=github_token,required=true \
    test -s /run/secrets/github_token && \
    git config --global url."https://github.com/".insteadOf "ssh://git@github.com/" && \
    GIT_CONFIG_COUNT=3 \
    GIT_CONFIG_KEY_0=credential.helper GIT_CONFIG_VALUE_0= \
    GIT_CONFIG_KEY_1=credential.https://github.com.helper \
    GIT_CONFIG_VALUE_1='!sh /opt/utexo/scripts/docker-git-credential.sh' \
    GIT_CONFIG_KEY_2=credential.https://github.com.useHttpPath GIT_CONFIG_VALUE_2=true \
    GIT_TERMINAL_PROMPT=0 \
    CARGO_NET_GIT_FETCH_WITH_CLI=true \
    cargo build --locked --release -p esplora-adapter \
    && mkdir bins \
    && cp ./target/release/esplora-adapter ./bins/

FROM debian:stable-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/* \
    && useradd --system --no-create-home esplora

WORKDIR /opt/utexo

# Configure with ESPLORA_* env vars (see esplora-adapter/README.md). Required:
#   ESPLORA_ELECTRUM_ADDRESS, ESPLORA_NETWORK
ENV RUST_LOG=info \
    ESPLORA_LISTEN_ADDRESS=0.0.0.0 \
    ESPLORA_PORT=3002

COPY --from=builder /opt/utexo/bins/ /opt/utexo/

USER esplora
EXPOSE 3002

ENTRYPOINT ["./esplora-adapter"]
