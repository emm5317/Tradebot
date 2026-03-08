# ── Build stage ──────────────────────────────────────────────
FROM rust:1-bookworm AS builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Cache dependency build
COPY rust/Cargo.toml rust/Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && cargo build --release 2>/dev/null; rm -rf src

# Build real source
COPY rust/src/ src/
RUN touch src/main.rs && cargo build --release

# ── Runtime stage ────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/tradebot /usr/local/bin/tradebot
COPY config/ /etc/tradebot/

EXPOSE 3030

CMD ["tradebot"]
