# Infrastructure
db-up:
    docker compose -f docker/docker-compose.yml up -d

db-down:
    docker compose -f docker/docker-compose.yml down

db-reset:
    docker compose -f docker/docker-compose.yml down -v
    docker compose -f docker/docker-compose.yml up -d

# Migrations (requires sqlx-cli: cargo install sqlx-cli)
migrate:
    cd rust && sqlx migrate run --source ../migrations/

# Development
dev:
    cd rust && cargo run

build:
    cd rust && cargo build

# Testing
test:
    cd rust && cargo test

# Diagnostics
health:
    curl -s localhost:3000/api/health | jq .
