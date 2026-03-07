/// Database query helpers (sqlx).

pub async fn insert_order(
    _pool: &sqlx::PgPool,
    _ticker: &str,
    _side: &str,
    _price: f64,
    _qty: i32,
) -> Result<(), sqlx::Error> {
    // TODO: INSERT INTO orders …
    Ok(())
}

pub async fn get_daily_pnl(_pool: &sqlx::PgPool) -> Result<f64, sqlx::Error> {
    // TODO: SELECT sum(pnl) FROM orders WHERE date = today
    Ok(0.0)
}
