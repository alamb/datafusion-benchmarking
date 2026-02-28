FROM rust:1-bookworm AS builder

WORKDIR /build

# Copy workspace root manifests for dependency caching
COPY Cargo.toml Cargo.lock ./
COPY controller/Cargo.toml controller/Cargo.toml
RUN mkdir -p controller/src && echo 'fn main() {}' > controller/src/main.rs
RUN cargo build --release -p benchmark-controller

# Copy real source and rebuild
COPY controller/src/ controller/src/
COPY controller/migrations/ controller/migrations/
RUN touch controller/src/main.rs && cargo build --release -p benchmark-controller

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/benchmark-controller /usr/local/bin/benchmark-controller

EXPOSE 8080
ENTRYPOINT ["benchmark-controller"]
