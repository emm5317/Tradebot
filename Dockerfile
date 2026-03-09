# ── Rust build stage ────────────────────────────────────────
FROM rust:1-bookworm AS rust-builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Cache dependency build
COPY rust/Cargo.toml rust/Cargo.lock* ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && cargo build --release 2>/dev/null; rm -rf src

# Build real source
COPY rust/src/ src/
RUN touch src/main.rs && cargo build --release

# ── Runtime stage (Rust + Python) ──────────────────────────
FROM python:3.12-slim-bookworm

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates libssl3 curl postgresql-client \
    && rm -rf /var/lib/apt/lists/*

# Install just (task runner)
RUN curl -fsSL https://just.systems/install.sh | bash -s -- --to /usr/local/bin

# Rust binary
COPY --from=rust-builder /app/target/release/tradebot /usr/local/bin/tradebot

# Python app
WORKDIR /app
COPY python/pyproject.toml python/
COPY python/ python/
RUN pip install --no-cache-dir ./python[dev]

# Migrations + config
COPY migrations/ migrations/
COPY config/ config/
COPY justfile .

# Python modules expect cwd=python/ for relative imports
WORKDIR /app/python

EXPOSE 3030 8050

# Default: run Rust binary. Override with command for other services.
CMD ["tradebot"]
