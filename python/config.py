from pydantic_settings import BaseSettings


class Settings(BaseSettings):
    database_url: str = "postgres://tradebot:tradebot_dev@localhost:5432/tradebot"
    kalshi_api_key: str = ""
    kalshi_private_key_path: str = ""
    kalshi_base_url: str = "https://demo-api.kalshi.co"
    binance_ws_url: str = "wss://stream.binance.com:9443/ws/btcusdt@trade"
    mesonet_base_url: str = "https://mesonet.agron.iastate.edu"

    # Stations to collect ASOS observations from
    asos_stations: list[str] = ["KORD", "KJFK", "KDEN", "KLAX", "KIAH"]

    # Collection intervals
    collection_interval_seconds: int = 60

    # Evaluation loop
    evaluation_interval_seconds: int = 10
    nats_url: str = "nats://localhost:4222"
    redis_url: str = "redis://localhost:6379"

    # Dashboard
    dashboard_host: str = "0.0.0.0"
    dashboard_port: int = 8050

    # Notifications
    discord_webhook_url: str = ""

    # Concurrent snapshot fetching
    max_concurrent_snapshots: int = 8

    model_config = {"env_prefix": "", "env_file": "../config/.env", "extra": "ignore"}


def get_settings() -> Settings:
    return Settings()
