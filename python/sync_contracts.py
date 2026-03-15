"""Sync active contracts from Kalshi into the database.

Pulls all open/active weather and crypto contracts so the evaluator
and contract discovery can find contracts to trade.

Usage:
    python -m sync_contracts              # pull active + settled (last 3 months)
    python -m sync_contracts --active     # active only
    python -m sync_contracts --settled    # settled only (for backtesting)
    python -m sync_contracts --loop 300   # re-sync every 5 minutes
"""

import argparse
import asyncio

import structlog

from data.kalshi_history import pull_active_contracts, pull_settlement_history, settle_stale_contracts

logger = structlog.get_logger()


async def main() -> None:
    parser = argparse.ArgumentParser(description="Sync Kalshi contracts into DB")
    parser.add_argument("--active", action="store_true", help="Pull active contracts only")
    parser.add_argument("--settled", action="store_true", help="Pull settled contracts only")
    parser.add_argument("--months", type=int, default=3, help="Months of settled history")
    parser.add_argument("--loop", type=int, default=0, help="Re-sync interval in seconds (0=once)")
    args = parser.parse_args()

    # Default: pull both
    pull_both = not args.active and not args.settled

    while True:
        if args.active or pull_both:
            n = await pull_active_contracts()
            print(f"Synced {n} active contracts")

        if args.settled or pull_both:
            n = await pull_settlement_history(months=args.months)
            print(f"Synced {n} settled contracts")

        # Always settle stale contracts (orders past settlement with no result)
        n = await settle_stale_contracts()
        if n > 0:
            print(f"Settled {n} stale contracts + updated order outcomes")

        if args.loop <= 0:
            break

        logger.info("sync_sleeping", seconds=args.loop)
        await asyncio.sleep(args.loop)


if __name__ == "__main__":
    asyncio.run(main())
