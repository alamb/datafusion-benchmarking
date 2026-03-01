FROM rust:1-bookworm

# Skip rust-toolchain.toml files — use the image's pre-installed stable
# toolchain to avoid rustup conflicts when switching minor versions.
ENV RUSTUP_TOOLCHAIN=stable

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

# Install critcmp for criterion benchmark comparisons, and
# tpchgen-cli for generating TPC-H test data
RUN cargo install critcmp tpchgen-cli sccache

# Install uv (Python package manager) — needed by bench.sh for comparison
RUN curl -LsSf https://astral.sh/uv/install.sh | sh
ENV PATH="/root/.local/bin:${PATH}"

# TPC-H expected answer files (extracted in GHA before build)
COPY tpch-answers/ /data/tpch-answers/

WORKDIR /workspace

# Build bench-cache binary (standalone, not part of workspace)
COPY Cargo.toml /build/bench-cache/Cargo.toml
COPY src/ /build/bench-cache/src/
RUN cd /build/bench-cache && cargo build --release \
    && cp /build/bench-cache/target/release/bench-cache /usr/local/bin/bench-cache \
    && rm -rf /build

COPY entrypoint.sh run_bench_sh.sh run_criterion.sh run_arrow_criterion.sh cache_data.sh /scripts/
RUN chmod +x /scripts/*.sh

ENTRYPOINT ["/scripts/entrypoint.sh"]
