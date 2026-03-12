# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| `main` branch (latest) | Yes |
| Older commits / tags | No |

Only the current `main` branch receives security fixes. There are no versioned releases at this time.

## Reporting a Vulnerability

Tradebot is a financial trading system that interacts with real exchanges and handles API credentials, order execution, and risk controls. Security reports are taken seriously and will be prioritized accordingly.

**To report a vulnerability**, please contact the maintainer directly via GitHub:

- GitHub: [@emm5317](https://github.com/emm5317)

Please include:

- A description of the vulnerability and its potential impact
- Steps to reproduce or a proof of concept
- The affected component(s) (e.g., auth, execution, risk controls)

**Do not open a public GitHub issue for security vulnerabilities.**

## Responsible Disclosure Timeline

- **Acknowledgement**: Within 3 business days of receiving your report.
- **Assessment**: An initial severity assessment will be provided within 7 days.
- **Fix timeline**: Critical issues will be patched as quickly as possible, targeting 30 days. Non-critical issues may take up to 90 days.
- **Disclosure**: After a fix is deployed, coordinated disclosure is welcome. Please allow up to 90 days before public disclosure.

## Scope

### In scope

- Authentication and API key handling (Kalshi RSA-PSS signing, exchange credentials)
- Order execution logic and risk controls (kill switches, position limits, edge guards)
- Secrets management and credential storage (`.env` files, environment variables)
- WebSocket feed authentication and data integrity
- Database access controls and SQL injection vectors
- NATS / Redis message integrity
- Dependency vulnerabilities with a viable exploit path

### Out of scope

- Issues that only affect demo or paper trading mode (`PAPER_MODE=true`)
- Denial-of-service against local development infrastructure (Docker Compose services)
- Issues requiring physical access to the host machine
- Social engineering attacks
- Bugs in third-party exchange APIs (Kalshi, Coinbase, Binance, Deribit)

## Security Best Practices for Contributors

- Never commit `.env` files, API keys, or private keys
- Always use `PAPER_MODE=true` unless explicitly authorized for live trading
- Review the `CLAUDE.md` trading mode section before making configuration changes
