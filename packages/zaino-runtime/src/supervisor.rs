//! Supervising a component: observe its health, act on its lifecycle.
//!
//! The minimal supervision policy — restart a component that has gone
//! [`Health::Critical`] — expressed over the `zaino-component` ports. The
//! component owns *how* it restarts ([`Managed`]); the supervisor owns *when* (a
//! health it will not tolerate). Observing is [`StatusSource`]; acting is
//! [`Managed`].
//!
//! Intentionally small: DAG-ordered bringup and the validator readiness gate
//! layer on top of this later.

use zaino_component::{Health, Managed, StatusSource};

/// What a supervision step did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisionOutcome {
    /// Health was acceptable; nothing was done.
    Observed,
    /// Health was [`Health::Critical`]; the component was restarted.
    Restarted,
}

/// One supervision step over `component`: observe its health, and restart it if
/// it has gone [`Health::Critical`]. Returns what was done, or the component's
/// own restart error.
///
/// Restart-on-critical only: `Recoverable` is left to recover on its own,
/// `Offline` is a deliberate or awaited state the supervisor does not force out
/// of, and `Healthy` needs nothing.
pub async fn supervise_step<C>(component: &C) -> Result<SupervisionOutcome, C::Error>
where
    C: StatusSource + Managed,
{
    match component.status().health {
        Health::Critical => {
            component.restart().await?;
            Ok(SupervisionOutcome::Restarted)
        }
        Health::Healthy | Health::Recoverable | Health::Offline => Ok(SupervisionOutcome::Observed),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use zaino_component::{
        ComponentName, ComponentStatus, Health, Lifecycle, Managed, StatusSource,
    };

    use super::{supervise_step, SupervisionOutcome};

    const NAME: ComponentName = ComponentName("mock");

    /// A component the test can put in a chosen health; restarting heals it and
    /// counts the restart.
    struct Mock {
        health: Mutex<Health>,
        lifecycle: Mutex<Lifecycle>,
        restarts: AtomicUsize,
    }

    impl Mock {
        fn in_health(health: Health) -> Self {
            Self {
                health: Mutex::new(health),
                lifecycle: Mutex::new(Lifecycle::Ready),
                restarts: AtomicUsize::new(0),
            }
        }

        fn restarts(&self) -> usize {
            self.restarts.load(Ordering::SeqCst)
        }
    }

    impl StatusSource for Mock {
        fn status(&self) -> ComponentStatus {
            ComponentStatus::new(
                NAME,
                *self.lifecycle.lock().unwrap(),
                *self.health.lock().unwrap(),
            )
        }
    }

    impl Managed for Mock {
        type Error = std::convert::Infallible;

        async fn spawn(&self) -> Result<(), Self::Error> {
            Ok(())
        }

        async fn restart(&self) -> Result<(), Self::Error> {
            *self.health.lock().unwrap() = Health::Healthy;
            *self.lifecycle.lock().unwrap() = Lifecycle::Ready;
            self.restarts.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn stop(&self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn restarts_a_critical_component() {
        let mock = Mock::in_health(Health::Critical);
        assert_eq!(
            supervise_step(&mock).await.unwrap(),
            SupervisionOutcome::Restarted
        );
        assert_eq!(mock.status().health, Health::Healthy);
        assert_eq!(mock.restarts(), 1);
    }

    #[tokio::test]
    async fn leaves_a_healthy_component_alone() {
        let mock = Mock::in_health(Health::Healthy);
        assert_eq!(
            supervise_step(&mock).await.unwrap(),
            SupervisionOutcome::Observed
        );
        assert_eq!(mock.restarts(), 0);
    }

    #[tokio::test]
    async fn does_not_restart_a_recoverable_component() {
        let mock = Mock::in_health(Health::Recoverable);
        assert_eq!(
            supervise_step(&mock).await.unwrap(),
            SupervisionOutcome::Observed
        );
        assert_eq!(mock.restarts(), 0);
    }
}
