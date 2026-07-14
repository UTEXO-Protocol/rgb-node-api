FROM rust:1.96 AS builder

WORKDIR /opt/utexo

COPY . .

RUN apt-get update && apt-get install -y build-essential \
        cmake \
        git \
        pkgconf

# rgb-lib is declared as an ssh:// git dependency but the repo is public,
# so rewrite to anonymous HTTPS to fetch it without any SSH key/secret.
RUN git config --global url."https://github.com/".insteadOf "ssh://git@github.com/" && \
    CARGO_NET_GIT_FETCH_WITH_CLI=true \
    cargo build --release --bin rgb-node-api \
    && mkdir bins \
    && cp ./target/release/rgb-node-api ./bins/

FROM debian:stable-slim
RUN apt-get update && apt-get install -y ca-certificates bash && rm -rf /var/lib/apt/lists/*

WORKDIR /opt/utexo

ENV RUST_LOG=info

COPY --from=builder /opt/utexo/bins/ /opt/utexo/

EXPOSE 34000

ENTRYPOINT ["./rgb-node-api"]
