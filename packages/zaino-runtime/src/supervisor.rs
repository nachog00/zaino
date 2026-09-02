//! Supervising a component: observe its health, act on its lifecycle.
//!
//! The minimal supervision policy — restart a component that has gone
//! [`Health::Critical`] — expressed over the `zaino-component` ports. The
//! component owns *how* it restarts ([`Managed`]); the supervisor owns *when*.
//!
//! Two entry points: [`supervise_step`] does one observe→act pass (for a caller
//! that drives its own cadence); [`supervise`] runs a **reactive** loop that
//! sleeps between transitions, waking on each status change via
//! [`StatusWatch`] — no polling.
//!
//! Intentionally small: DAG-ordered bringup and the validator readiness gate
//! layer on top of this later.

use zaino_component::{Health, Managed, StatusSource, StatusWatch};

/// What a supervision step did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisionOutcome {
    /// Health was acceptable; nothing was done.
    Observed,
    /// Health was [`Health::Critical`]; the component was restarted.
    Restarted,
}

/// The supervisor's policy: which health it will not tolerate.
///
/// Restart-on-critical only: `Recoverable` recovers on its own, `Offline` is a
/// deliberate or awaited state, `Healthy` needs nothing.
fn needs_restart(health: Health) -> bool {
    matches!(health, Health::Critical)
}

/// One supervision step over `component`: observe its health and restart it if
/// it has gone [`Health::Critical`], otherwise leave it. Returns what was done,
/// or the component's own restart error.
pub async fn supervise_step<C>(component: &C) -> Result<SupervisionOutcome, C::Error>
where
    C: StatusSource + Managed,
{
    if needs_restart(component.status().health) {
        component.restart().await?;
        Ok(SupervisionOutcome::Restarted)
    } else {
        Ok(SupervisionOutcome::Observed)
    }
}

/// Supervise `component` reactively: wake on each status change and restart it
/// if it has gone [`Health::Critical`]. Sleeps between transitions — no polling.
///
/// Returns once every sender is dropped (the component is gone). A caller that
/// wants to stop supervising a live component runs this on a cancellable
/// [`Task`](zaino_component::Task) and aborts it.
pub async fn supervise<C>(component: &C) -> Result<(), C::Error>
where
    C: StatusWatch + Managed,
{
    let mut status = component.subscribe();
    loop {
        // Read (and mark seen) the current state, then act. The borrow is
        // dropped before the await, so nothing is held across the restart.
        if needs_restart(status.borrow_and_update().health) {
            component.restart().await?;
        }
        // Sleep until the next transition; `Err` means the component is gone.
        if status.changed().await.is_err() {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::sync::watch;
    use zaino_component::{
        ComponentName, ComponentStatus, Health, Lifecycle, Managed, StatusSource, StatusWatch,
    };

    use super::{supervise, supervise_step, SupervisionOutcome};

    const NAME: ComponentName = ComponentName("mock");

    /// A watch-backed component: its status lives in the `watch`, so it is both
    /// observable ([`StatusSource`]) and subscribable ([`StatusWatch`]).
    /// Restarting heals it to Healthy/Ready and counts the restart.
    #[derive(Clone)]
    struct Mock {
        status: watch::Sender<ComponentStatus>,
        restarts: Arc<AtomicUsize>,
    }

    impl Mock {
        fn in_health(health: Health) -> Self {
            let (status, _) = watch::channel(ComponentStatus::new(NAME, Lifecycle::Ready, health));
            Self {
                status,
                restarts: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn set_health(&self, health: Health) {
            self.status.send_modify(|s| s.health = health);
        }

        fn restarts(&self) -> usize {
            self.restarts.load(Ordering::SeqCst)
        }
    }

    impl StatusSource for Mock {
        fn status(&self) -> ComponentStatus {
            *self.status.borrow()
        }
    }

    impl StatusWatch for Mock {
        fn subscribe(&self) -> watch::Receiver<ComponentStatus> {
            self.status.subscribe()
        }
    }

    impl Managed for Mock {
        type Error = std::convert::Infallible;

        async fn spawn(&self) -> Result<(), Self::Error> {
            Ok(())
        }

        async fn restart(&self) -> Result<(), Self::Error> {
            self.status.send_modify(|s| {
                s.health = Health::Healthy;
                s.lifecycle = Lifecycle::Ready;
            });
            self.restarts.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn stop(&self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    /// Poll `cond` until true, or fail — the reactive loop runs concurrently.
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
    async fn step_restarts_a_critical_component() {
        let mock = Mock::in_health(Health::Critical);
        assert_eq!(
            supervise_step(&mock).await.unwrap(),
            SupervisionOutcome::Restarted
        );
        assert_eq!(mock.status().health, Health::Healthy);
        assert_eq!(mock.restarts(), 1);
    }

    #[tokio::test]
    async fn step_leaves_healthy_and_recoverable_alone() {
        let healthy = Mock::in_health(Health::Healthy);
        assert_eq!(
            supervise_step(&healthy).await.unwrap(),
            SupervisionOutcome::Observed
        );
        let recoverable = Mock::in_health(Health::Recoverable);
        assert_eq!(
            supervise_step(&recoverable).await.unwrap(),
            SupervisionOutcome::Observed
        );
        assert_eq!(healthy.restarts() + recoverable.restarts(), 0);
    }

    #[tokio::test]
    async fn supervise_reacts_to_a_critical_transition() {
        let mock = Mock::in_health(Health::Healthy);
        let watched = mock.clone();
        let handle = tokio::spawn(async move {
            let _ = supervise(&watched).await;
        });

        // Drive it Critical; the reactive loop should wake and restart it.
        mock.set_health(Health::Critical);
        wait_until(|| mock.restarts() >= 1).await;
        assert_eq!(mock.status().health, Health::Healthy);

        handle.abort();
    }
}
