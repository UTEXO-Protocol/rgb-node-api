# syntax=docker/dockerfile:1
FROM rust:1.96 AS builder

WORKDIR /opt/utexo

COPY . .

RUN apt-get update && apt-get install -y build-essential \
        cmake \
        git \
        pkgconf

# Git revisions work without a sibling checkout. The token exists only in a
# BuildKit secret mount, never in an image layer, remote URL or Git config.
RUN --mount=type=secret,id=bfa_mirrors_token,required=true \
    git config --global url."https://github.com/".insteadOf "ssh://git@github.com/" && \
    GIT_ASKPASS=/opt/utexo/.github/scripts/git-askpass.sh \
    BFA_MIRRORS_TOKEN_FILE=/run/secrets/bfa_mirrors_token \
    GIT_TERMINAL_PROMPT=0 CARGO_NET_GIT_FETCH_WITH_CLI=true \
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
