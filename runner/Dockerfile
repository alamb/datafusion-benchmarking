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

# Install critcmp for criterion benchmark comparisons
RUN cargo install critcmp

WORKDIR /workspace

COPY entrypoint.sh run_bench_sh.sh run_criterion.sh run_arrow_criterion.sh /scripts/
RUN chmod +x /scripts/*.sh

ENTRYPOINT ["/scripts/entrypoint.sh"]
