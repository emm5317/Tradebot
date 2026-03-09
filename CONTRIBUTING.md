# Contributing to Tradebot

Thanks for your interest in contributing! This guide covers the basics for getting started.

## Prerequisites

- **Rust** 1.75+ (stable)
- **Python** 3.11+
- **Docker** and Docker Compose
- **just** task runner (`cargo install just`)
- **sqlx-cli** for migrations (`cargo install sqlx-cli`)

## Setup

```bash
# 1. Clone the repo
git clone https://github.com/your-org/tradebot.git
cd tradebot

# 2. Start infrastructure
just db-up                     # PostgreSQL, Redis, NATS

# 3. Configure environment
cp config/.env.example .env    # Fill in credentials

# 4. Verify everything works
just test-all                  # 354 tests (112 Rust + 242 Python)
```

## Code Style

### Rust
- Tests are **inline** in each module (`#[cfg(test)] mod tests`)
- Use `cargo fmt` and `cargo clippy -- -D warnings` before committing
- Async code: extract data from lock guards before `.await` (see TapeSnapshot pattern)

### Python
- **structlog** for all logging (no print statements)
- **pydantic** for configuration and data schemas
- Tests in `python/tests/` using pytest
- New model parameters should default to `None` for backward compatibility

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

## Pull Request Guidelines

- Describe **what** changed and **why**
- Include test coverage for new behavior
- Update `CLAUDE.md` if you add new files or change architecture
- Reference related issues if applicable

## Architecture Context

- **`CLAUDE.md`** — project conventions, key files, common pitfalls
- **`docs/build-plans/`** — detailed specs for each implementation phase
- **`docs/trading-models.md`** — full model documentation

## Questions?

Open an issue for discussion or check the existing `docs/` for context.
