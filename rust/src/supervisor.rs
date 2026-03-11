//! Task supervision — detects silent task deaths and triggers shutdown.
//!
//! Phase 12.0c: Wraps `tokio::task::JoinSet` to classify tasks as Critical
//! or NonCritical. Critical task death activates the kill switch and initiates
//! graceful shutdown. Non-critical task deaths are logged as warnings.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::kill_switch::KillSwitchState;

/// How critical a task is to the trading system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskCriticality {
    /// Task death triggers kill switch + shutdown.
    Critical,
    /// Task death is logged; process continues.
    NonCritical,
}

/// Metadata for a supervised task.
struct TaskMeta {
    criticality: TaskCriticality,
    started_at: Instant,
}

/// Supervises spawned tasks and reacts to unexpected exits.
pub struct TaskSupervisor {
    join_set: JoinSet<&'static str>,
    meta: HashMap<&'static str, TaskMeta>,
    id_to_name: HashMap<tokio::task::Id, &'static str>,
    cancel: CancellationToken,
    kill_switch: Arc<KillSwitchState>,
}

impl TaskSupervisor {
    pub fn new(cancel: CancellationToken, kill_switch: Arc<KillSwitchState>) -> Self {
        Self {
            join_set: JoinSet::new(),
            meta: HashMap::new(),
            id_to_name: HashMap::new(),
            cancel,
            kill_switch,
        }
    }

    /// Spawn a named task with a criticality classification.
    pub fn spawn(
        &mut self,
        name: &'static str,
        criticality: TaskCriticality,
        fut: impl std::future::Future<Output = ()> + Send + 'static,
    ) {
        self.meta.insert(name, TaskMeta {
            criticality,
            started_at: Instant::now(),
        });
        let abort_handle = self.join_set.spawn(async move {
            fut.await;
            name
        });
        self.id_to_name.insert(abort_handle.id(), name);
        info!(task = name, criticality = ?criticality, "task spawned under supervision");
    }

    /// Run the supervision loop. Returns when shutdown is triggered
    /// (either externally via cancel or internally due to critical task death).
    pub async fn run(&mut self) {
        loop {
            tokio::select! {
                result = self.join_set.join_next() => {
                    match result {
                        None => {
                            // All tasks gone
                            if !self.cancel.is_cancelled() {
                                error!("all supervised tasks exited unexpectedly");
                                self.cancel.cancel();
                            }
                            return;
                        }
                        Some(Ok(name)) => {
                            if self.cancel.is_cancelled() {
                                info!(task = name, "task exited during shutdown");
                            } else {
                                self.handle_task_exit(name, false);
                            }
                        }
                        Some(Err(join_err)) => {
                            if self.cancel.is_cancelled() {
                                continue;
                            }
                            let task_id = join_err.id();
                            let name = self.id_to_name.get(&task_id).copied().unwrap_or("unknown");
                            let is_panic = join_err.is_panic();
                            error!(task = name, panicked = is_panic, "task JoinError");
                            self.handle_task_exit(name, is_panic);
                        }
                    }
                }
                _ = self.cancel.cancelled() => {
                    info!("supervisor: shutdown initiated, aborting remaining tasks");
                    self.join_set.abort_all();
                    while self.join_set.join_next().await.is_some() {}
                    return;
                }
            }
        }
    }

    fn handle_task_exit(&self, name: &str, panicked: bool) {
        let criticality = self.meta.get(name)
            .map(|m| m.criticality)
            .unwrap_or(TaskCriticality::Critical);

        match criticality {
            TaskCriticality::Critical => {
                error!(
                    task = name,
                    panicked,
                    "CRITICAL task died — activating kill switch and shutting down"
                );
                self.kill_switch.kill_all.store(true, Ordering::Relaxed);
                self.cancel.cancel();
            }
            TaskCriticality::NonCritical => {
                warn!(
                    task = name,
                    panicked,
                    "non-critical task died — trading continues"
                );
            }
        }
    }

    /// Number of tasks still running.
    pub fn active_count(&self) -> usize {
        self.join_set.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_critical_task_death_triggers_cancel() {
        let cancel = CancellationToken::new();
        let ks = Arc::new(KillSwitchState::new(false, false, false));
        let mut sup = TaskSupervisor::new(cancel.clone(), Arc::clone(&ks));

        sup.spawn("critical_task", TaskCriticality::Critical, async {
            // Exit immediately
        });

        // Also spawn a long-running task so supervisor doesn't exit from empty JoinSet
        sup.spawn("long_task", TaskCriticality::NonCritical, async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        });

        sup.run().await;

        assert!(cancel.is_cancelled(), "cancel should be triggered by critical task death");
        assert!(ks.kill_all.load(Ordering::Relaxed), "kill_all should be activated");
    }

    #[tokio::test]
    async fn test_non_critical_task_death_continues() {
        let cancel = CancellationToken::new();
        let ks = Arc::new(KillSwitchState::new(false, false, false));
        let mut sup = TaskSupervisor::new(cancel.clone(), Arc::clone(&ks));

        sup.spawn("optional_task", TaskCriticality::NonCritical, async {
            // Exit immediately
        });

        // Spawn another non-critical that also exits, so supervisor sees empty JoinSet
        // and returns. The key check: cancel should NOT be triggered before empty.

        // We need a task that stays alive briefly, then we cancel externally
        let cancel_c = cancel.clone();
        sup.spawn("watchdog", TaskCriticality::NonCritical, async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            cancel_c.cancel(); // simulate external shutdown
        });

        sup.run().await;

        assert!(!ks.kill_all.load(Ordering::Relaxed), "kill_all should NOT be activated for non-critical");
    }

    #[tokio::test]
    async fn test_external_cancel_drains_all() {
        let cancel = CancellationToken::new();
        let ks = Arc::new(KillSwitchState::new(false, false, false));
        let mut sup = TaskSupervisor::new(cancel.clone(), Arc::clone(&ks));

        sup.spawn("task_a", TaskCriticality::Critical, async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        });
        sup.spawn("task_b", TaskCriticality::NonCritical, async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        });

        // Cancel from outside after a short delay
        let cancel_c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            cancel_c.cancel();
        });

        sup.run().await;

        assert!(cancel.is_cancelled());
        assert!(!ks.kill_all.load(Ordering::Relaxed), "kill_all should NOT be set for external cancel");
        assert_eq!(sup.active_count(), 0, "all tasks should be drained");
    }

    #[tokio::test]
    async fn test_critical_panic_triggers_cancel() {
        let cancel = CancellationToken::new();
        let ks = Arc::new(KillSwitchState::new(false, false, false));
        let mut sup = TaskSupervisor::new(cancel.clone(), Arc::clone(&ks));

        sup.spawn("panicker", TaskCriticality::Critical, async {
            panic!("intentional test panic");
        });

        sup.spawn("long_task", TaskCriticality::NonCritical, async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        });

        sup.run().await;

        assert!(cancel.is_cancelled());
        assert!(ks.kill_all.load(Ordering::Relaxed));
    }
}
