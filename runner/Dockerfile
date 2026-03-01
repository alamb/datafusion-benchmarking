# ── Stage 1: install external CLI tools ──
FROM rust:1-bookworm AS tools

RUN curl -L --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh | bash \
    && cargo binstall -y --no-symlinks critcmp tpchgen-cli sccache

# ── Stage 2: build bench-cache binary ──
FROM rust:1-bookworm AS bench-cache

COPY Cargo.toml /build/Cargo.toml
COPY src/ /build/src/
RUN cd /build && cargo build --release

# ── Stage 3: final runner image ──
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

# TPC-H expected answer files (extracted in GHA before build)
COPY tpch-answers/ /data/tpch-answers/

WORKDIR /workspace

COPY entrypoint.sh run_bench_sh.sh run_criterion.sh run_arrow_criterion.sh cache_data.sh /scripts/
RUN chmod +x /scripts/*.sh

ENTRYPOINT ["/scripts/entrypoint.sh"]
