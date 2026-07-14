FROM rust:1.96 AS builder

WORKDIR /opt/utexo

COPY . .

RUN apt-get update && apt-get install -y build-essential \
        cmake \
        git \
        pkgconf

RUN cargo build --release --bin rgb-node-api \
    && mkdir bins \
    && cp ./target/release/rgb-node-api ./bins/

FROM debian:stable-slim
RUN apt-get update && apt-get install -y ca-certificates bash && rm -rf /var/lib/apt/lists/*

WORKDIR /opt/utexo

ENV RUST_LOG=info

COPY --from=builder /opt/utexo/bins/ /opt/utexo/

EXPOSE 34000

ENTRYPOINT ["./rgb-node-api"]
