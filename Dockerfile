# syntax=docker/dockerfile:1
FROM rust:1.96 AS builder

WORKDIR /opt/utexo

COPY . .

RUN apt-get update && apt-get install -y build-essential \
        cmake \
        git \
        pkgconf

# Private Cargo dependencies need credentials inside the build container too.
# The helper reads a BuildKit secret; no token is written to Git config or layers.
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
    cargo build --locked --release --bin rgb-node-api \
    && mkdir bins \
    && cp ./target/release/rgb-node-api ./bins/

FROM debian:stable-slim
RUN apt-get update && apt-get install -y ca-certificates bash && rm -rf /var/lib/apt/lists/*

WORKDIR /opt/utexo

ENV RUST_LOG=info

COPY --from=builder /opt/utexo/bins/ /opt/utexo/

EXPOSE 34000

ENTRYPOINT ["./rgb-node-api"]
