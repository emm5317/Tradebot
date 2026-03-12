# Contributing to Tradebot

Thanks for your interest in contributing! This guide covers setup, code style, and the pull request process.

## Prerequisites

- **Rust** 1.75+ (stable)
- **Python** 3.12+
- **Docker** and Docker Compose v2
- **just** task runner (`cargo install just`)
- **sqlx-cli** for migrations (`cargo install sqlx-cli`)

## Setup

```bash
# 1. Clone the repo
git clone https://github.com/emm5317/Tradebot.git
cd Tradebot

# 2. Start infrastructure
just db-up                     # PostgreSQL, Redis, NATS

# 3. Configure environment
cp config/.env.example .env    # Fill in Kalshi API key + credentials

# 4. Run migrations
just migrate

# 5. Verify everything works
just test-all                  # 590 tests (187 Rust + 403 Python)
```

## Code Style

### Rust

- Format with `cargo fmt` and lint with `cargo clippy -- -D warnings`
- Tests are **inline** in each module (`#[cfg(test)] mod tests`)
- Extract data from lock guards before `.await` (see TapeSnapshot pattern in `orderbook_feed.rs`)
- Use `tracing` for structured logging (not `println!`)
- Prefer `Arc<std::sync::RwLock>` for low-write shared state, `DashMap` for high-cardinality maps

### Python

- **structlog** for all logging (no print statements)
- **pydantic** for configuration and data schemas
- Tests in `python/tests/` using pytest
- New model parameters should default to `None` for backward compatibility
- Use type hints throughout

## Making Changes

1. **Create a branch** from `main`
2. **Write tests** for new functionality
3. **Run the full suite** before opening a PR:
   ```bash
   just test-all      # Rust + Python tests
   just fmt-check     # Rust formatting
   just clippy        # Rust lints
   ```
4. **Keep commits focused** — one logical change per commit
5. **Update docs** if you add new files, environment variables, or change architecture

## Pull Request Guidelines

- Describe **what** changed and **why**
- Include test coverage for new behavior
- Reference related issues if applicable
- Keep PRs focused — prefer multiple small PRs over one large one

## Architecture Context

- **[CLAUDE.md](CLAUDE.md)** — Project conventions, key files, common pitfalls
- **[docs/trading-models.md](docs/trading-models.md)** — Fair-value model documentation
- **[docs/build-plans/](docs/build-plans/)** — Detailed implementation plans for each phase
- **[docs/configuration.md](docs/configuration.md)** — Environment variable reference

## Code of Conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md). Please review it before participating.

## Questions?

Open an issue for discussion or check the existing [docs/](docs/) for context.
