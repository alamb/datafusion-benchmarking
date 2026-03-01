FROM rust:1-bookworm AS builder

WORKDIR /build

# Copy workspace root manifests for dependency caching
COPY Cargo.toml Cargo.lock ./
COPY controller/Cargo.toml controller/Cargo.toml
COPY runner/Cargo.toml runner/Cargo.toml
RUN mkdir -p controller/src/bin && echo 'fn main() {}' > controller/src/bin/controller.rs \
    && echo 'fn main() {}' > controller/src/bin/runner.rs \
    && mkdir -p controller/src && echo '' > controller/src/lib.rs \
    && mkdir -p runner/src && echo 'fn main() {}' > runner/src/main.rs
RUN cargo build --release --bin controller

# Copy real source and rebuild
COPY controller/src/ controller/src/
COPY controller/migrations/ controller/migrations/
RUN touch controller/src/lib.rs && cargo build --release --bin controller

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/controller /usr/local/bin/controller

EXPOSE 8080
ENTRYPOINT ["controller"]
