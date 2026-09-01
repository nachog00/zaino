//! The event → state → observe → act loop, on a mock component.
//!
//! A component is spawned; its task fails and flips its health to `Critical`
//! (event → state); a minimal supervisor reads that through [`StatusSource`]
//! (observe) and restarts it through [`Managed`] (act); health returns to
//! `Healthy`. No runtime, no dev crates — just the abstraction.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::{ComponentStatus, Health, Lifecycle, Managed, StatusSource, Task, TaskName};

/// A component whose spawned task marks it `Critical` while `fail` is set, and
/// `Healthy` once it is cleared — a stand-in for a subsystem whose work loop
/// dies and then recovers on restart.
#[derive(Clone)]
struct Flaky {
    health: Arc<Mutex<Health>>,
    lifecycle: Arc<Mutex<Lifecycle>>,
    fail: Arc<AtomicBool>,
    task: Arc<Mutex<Option<Task>>>,
}

impl Flaky {
    fn new() -> Self {
        Self {
            health: Arc::new(Mutex::new(Health::Offline)),
            lifecycle: Arc::new(Mutex::new(Lifecycle::Closing)),
            fail: Arc::new(AtomicBool::new(true)),
            task: Arc::new(Mutex::new(None)),
        }
    }

    fn start_task(&self) {
        let health = Arc::clone(&self.health);
        let fail = Arc::clone(&self.fail);
        let task = Task::spawn(TaskName("flaky-work"), move |cancel| async move {
            *health.lock().unwrap() = if fail.load(Ordering::SeqCst) {
                Health::Critical
            } else {
                Health::Healthy
            };
            // Then idle, as a real work loop would, until told to stop.
            cancel.cancelled().await;
        });
        *self.task.lock().unwrap() = Some(task);
    }
}

impl StatusSource for Flaky {
    fn status(&self) -> ComponentStatus {
        ComponentStatus {
            lifecycle: *self.lifecycle.lock().unwrap(),
            health: *self.health.lock().unwrap(),
        }
    }
}

impl Managed for Flaky {
    type Error = std::convert::Infallible;

    async fn spawn(&self) -> Result<(), Self::Error> {
        *self.lifecycle.lock().unwrap() = Lifecycle::Spawning;
        self.start_task();
        *self.lifecycle.lock().unwrap() = Lifecycle::Ready;
        Ok(())
    }

    async fn restart(&self) -> Result<(), Self::Error> {
        self.stop().await?;
        self.spawn().await
    }

    async fn stop(&self) -> Result<(), Self::Error> {
        *self.lifecycle.lock().unwrap() = Lifecycle::Closing;
        if let Some(task) = self.task.lock().unwrap().take() {
            task.abort();
        }
        *self.health.lock().unwrap() = Health::Offline;
        Ok(())
    }
}

/// Poll `cond` until true, or fail — the task sets health asynchronously.
async fn wait_until(mut cond: impl FnMut() -> bool) {
    for _ in 0..200 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("condition not met in time");
}

#[tokio::test]
async fn task_failure_flips_health_and_supervisor_restarts() {
    let component = Flaky::new();
    assert_eq!(component.status().health, Health::Offline, "not started");

    // Spawn → the task runs and fails: event → state.
    component.spawn().await.unwrap();
    wait_until(|| component.status().health == Health::Critical).await;
    assert_eq!(
        component.status().lifecycle,
        Lifecycle::Ready,
        "spawned regardless of health"
    );

    // A minimal supervisor: observe (StatusSource) then act (Managed).
    component.fail.store(false, Ordering::SeqCst);
    if component.status().health == Health::Critical {
        component.restart().await.unwrap();
    }
    wait_until(|| component.status().health == Health::Healthy).await;
    assert_eq!(component.status().lifecycle, Lifecycle::Ready);
}

#[test]
fn health_and_lifecycle_are_independent_axes() {
    // A component can be Ready-but-Critical or Closing-but-Healthy: one axis
    // moving must not imply anything about the other.
    let ready_but_broken = ComponentStatus {
        lifecycle: Lifecycle::Ready,
        health: Health::Critical,
    };
    let closing_but_fine = ComponentStatus {
        lifecycle: Lifecycle::Closing,
        health: Health::Healthy,
    };
    assert_ne!(ready_but_broken.health, Health::Healthy);
    assert_eq!(ready_but_broken.lifecycle, Lifecycle::Ready);
    assert_eq!(closing_but_fine.health, Health::Healthy);
    assert_ne!(closing_but_fine.lifecycle, Lifecycle::Ready);
}
