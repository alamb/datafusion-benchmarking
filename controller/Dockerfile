FROM rust:1.83-bookworm AS builder

WORKDIR /build
COPY Cargo.toml Cargo.toml
COPY src/ src/
COPY migrations/ migrations/

RUN cargo build --release

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/benchmark-controller /usr/local/bin/benchmark-controller

ENTRYPOINT ["benchmark-controller"]
