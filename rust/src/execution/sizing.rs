/// Computes order size using Kelly criterion or fixed-fraction.
pub fn kelly_size(edge: f64, odds: f64, bankroll: f64, fraction: f64) -> f64 {
    let kelly_fraction = (edge * odds - (1.0 - edge)) / odds;
    let clamped = kelly_fraction.max(0.0).min(fraction);
    (bankroll * clamped).floor()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kelly_zero_edge() {
        assert_eq!(kelly_size(0.5, 1.0, 1000.0, 0.25), 0.0);
    }
}
