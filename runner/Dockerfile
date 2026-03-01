# ── Stage 1: install external CLI tools ──
FROM rust:1-bookworm AS tools

RUN curl -L --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh | bash \
    && cargo binstall -y --no-symlinks critcmp tpchgen-cli sccache

# ── Stage 2: build bench-cache binary ──
FROM rust:1-bookworm AS bench-cache

COPY runner/Cargo.toml /build/Cargo.toml
COPY runner/src/ /build/src/
RUN cd /build && cargo build --release

# ── Stage 3: build runner binary ──
FROM rust:1-bookworm AS runner-builder

WORKDIR /build

# Copy workspace root manifests for dependency caching
COPY Cargo.toml Cargo.lock ./
COPY controller/Cargo.toml controller/Cargo.toml
COPY runner/Cargo.toml runner/Cargo.toml
RUN mkdir -p controller/src/bin && echo 'fn main() {}' > controller/src/bin/controller.rs \
    && echo 'fn main() {}' > controller/src/bin/runner.rs \
    && mkdir -p controller/src && echo '' > controller/src/lib.rs \
    && mkdir -p runner/src && echo 'fn main() {}' > runner/src/main.rs
RUN cargo build --release --bin runner

# Copy real source and rebuild
COPY controller/src/ controller/src/
COPY controller/migrations/ controller/migrations/
RUN touch controller/src/lib.rs && cargo build --release --bin runner

# ── Stage 4: final runner image ──
FROM rust:1-bookworm

# Skip rust-toolchain.toml files — use the image's pre-installed stable
# toolchain to avoid rustup conflicts when switching minor versions.
ENV RUSTUP_TOOLCHAIN=stable

# Pull in the latest stable toolchain at image build time so runner scripts
# are less likely to trigger a rustup update (which races when two cargo
# processes start in parallel).
RUN rustup update stable --no-self-update

RUN apt-get update && apt-get install -y \
    git \
    cmake \
    build-essential \
    curl \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Install gh CLI
RUN curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
    | dd of=/usr/share/keyrings/githubcli-archive-keyring.gpg \
    && echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
    | tee /etc/apt/sources.list.d/github-cli.list > /dev/null \
    && apt-get update && apt-get install -y gh && rm -rf /var/lib/apt/lists/*

# Install uv (Python package manager) — needed by bench.sh for comparison
RUN curl -LsSf https://astral.sh/uv/install.sh | sh
ENV PATH="/root/.local/bin:${PATH}"

# Copy pre-built CLI tools from builder stages
COPY --from=tools /usr/local/cargo/bin/critcmp /usr/local/cargo/bin/critcmp
COPY --from=tools /usr/local/cargo/bin/tpchgen-cli /usr/local/cargo/bin/tpchgen-cli
COPY --from=tools /usr/local/cargo/bin/sccache /usr/local/cargo/bin/sccache
COPY --from=bench-cache /build/target/release/bench-cache /usr/local/bin/bench-cache
COPY --from=runner-builder /build/target/release/runner /usr/local/bin/runner

# TPC-H expected answer files (extracted in GHA before build)
COPY runner/tpch-answers/ /data/tpch-answers/

# Keep cache_data.sh — called by the Rust runner for data generation
COPY runner/cache_data.sh /scripts/
RUN chmod +x /scripts/*.sh

WORKDIR /workspace

ENTRYPOINT ["/usr/local/bin/runner"]
