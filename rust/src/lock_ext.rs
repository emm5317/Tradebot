//! Extension traits for lock recovery on poisoned `std::sync::RwLock`.
//!
//! Phase 12.0a: All production lock sites use these helpers instead of `.unwrap()`.
//! If a thread panics while holding a write lock, the data may be partially
//! updated but is structurally valid (all fields are independent numeric values).
//! Recovering from poison is better than crashing the entire trading bot.

use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use tracing::error;

/// Extension trait for `std::sync::RwLock` that recovers from poisoned locks
/// instead of panicking.
pub trait RwLockExt<T> {
    /// Acquire a read lock, recovering from poison if necessary.
    fn read_or_recover(&self) -> RwLockReadGuard<'_, T>;

    /// Acquire a write lock, recovering from poison if necessary.
    fn write_or_recover(&self) -> RwLockWriteGuard<'_, T>;
}

impl<T> RwLockExt<T> for RwLock<T> {
    fn read_or_recover(&self) -> RwLockReadGuard<'_, T> {
        self.read().unwrap_or_else(|poisoned| {
            error!(
                "RwLock read poisoned — recovering (a thread panicked while holding write lock)"
            );
            poisoned.into_inner()
        })
    }

    fn write_or_recover(&self) -> RwLockWriteGuard<'_, T> {
        self.write().unwrap_or_else(|poisoned| {
            error!(
                "RwLock write poisoned — recovering (a thread panicked while holding write lock)"
            );
            poisoned.into_inner()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_read_or_recover_normal() {
        let lock = RwLock::new(42);
        let guard = lock.read_or_recover();
        assert_eq!(*guard, 42);
    }

    #[test]
    fn test_write_or_recover_normal() {
        let lock = RwLock::new(42);
        {
            let mut guard = lock.write_or_recover();
            *guard = 99;
        }
        let guard = lock.read_or_recover();
        assert_eq!(*guard, 99);
    }

    #[test]
    fn test_read_recovers_from_poison() {
        let lock = Arc::new(RwLock::new(42));
        let lock2 = Arc::clone(&lock);

        // Poison the lock by panicking while holding a write guard
        let _ = std::thread::spawn(move || {
            let _guard = lock2.write().unwrap();
            panic!("intentional panic to poison lock");
        })
        .join();

        // Lock is now poisoned — read_or_recover should still work
        assert!(lock.read().is_err(), "lock should be poisoned");
        let guard = lock.read_or_recover();
        assert_eq!(*guard, 42);
    }

    #[test]
    fn test_write_recovers_from_poison() {
        let lock = Arc::new(RwLock::new(42));
        let lock2 = Arc::clone(&lock);

        let _ = std::thread::spawn(move || {
            let _guard = lock2.write().unwrap();
            panic!("intentional panic to poison lock");
        })
        .join();

        assert!(lock.write().is_err(), "lock should be poisoned");
        let mut guard = lock.write_or_recover();
        *guard = 100;
        drop(guard);

        let guard = lock.read_or_recover();
        assert_eq!(*guard, 100);
    }
}
