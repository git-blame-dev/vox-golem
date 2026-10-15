use std::sync::Arc;
use tokio::sync::{futures::Notified, Notify, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

#[derive(Default)]
pub(super) struct UpdateActivity {
    gate: Arc<RwLock<()>>,
    changed: Notify,
}

impl UpdateActivity {
    pub(super) fn operation(self: &Arc<Self>) -> Result<UpdateOperationLease, String> {
        let guard = Arc::clone(&self.gate)
            .try_read_owned()
            .map_err(|_| String::from("an update installation is starting"))?;
        Ok(UpdateOperationLease {
            guard: Some(guard),
            activity: Arc::clone(self),
        })
    }

    pub(super) fn installation(self: &Arc<Self>) -> Result<OwnedRwLockWriteGuard<()>, String> {
        Arc::clone(&self.gate)
            .try_write_owned()
            .map_err(|_| String::from("Update installation requires VoxGolem to be idle."))
    }

    pub(super) fn notified(&self) -> Notified<'_> {
        self.changed.notified()
    }

    pub(super) fn signal_change(&self) {
        self.changed.notify_waiters();
    }

    #[cfg(test)]
    pub(super) fn try_write(
        &self,
    ) -> Result<tokio::sync::RwLockWriteGuard<'_, ()>, tokio::sync::TryLockError> {
        self.gate.try_write()
    }
}

pub(super) struct UpdateOperationLease {
    guard: Option<OwnedRwLockReadGuard<()>>,
    activity: Arc<UpdateActivity>,
}

impl Drop for UpdateOperationLease {
    fn drop(&mut self) {
        // A notified installer must observe retirement, not a still-held read lock.
        drop(self.guard.take());
        self.activity.signal_change();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn operation_lease_blocks_installation_until_retired() {
        let activity = Arc::new(UpdateActivity::default());
        let lease = activity.operation().expect("operation starts while idle");
        assert!(activity.try_write().is_err());
        assert!(activity.installation().is_err());
        drop(lease);
        assert!(activity.installation().is_ok());
    }

    #[tokio::test]
    async fn retirement_notifies_with_writer_available() {
        let activity = Arc::new(UpdateActivity::default());
        let lease = activity.operation().expect("operation starts while idle");
        let notified = activity.notified();
        tokio::pin!(notified);
        assert!(futures_util::poll!(notified.as_mut()).is_pending());

        drop(lease);

        tokio::time::timeout(Duration::from_secs(5), notified)
            .await
            .expect("retirement wakes the registered waiter");
        let _installation = activity
            .installation()
            .expect("notification observes an actually released gate");
    }

    #[tokio::test]
    async fn cancelling_wait_does_not_retire_operation() {
        let activity = Arc::new(UpdateActivity::default());
        let lease = activity.operation().expect("operation starts while idle");
        {
            let notified = activity.notified();
            tokio::pin!(notified);
            assert!(futures_util::poll!(notified.as_mut()).is_pending());
        }
        assert!(activity.installation().is_err());

        let notified = activity.notified();
        tokio::pin!(notified);
        assert!(futures_util::poll!(notified.as_mut()).is_pending());
        drop(lease);
        tokio::time::timeout(Duration::from_secs(5), notified)
            .await
            .expect("a replacement waiter observes actual retirement");
        assert!(activity.installation().is_ok());
    }

    #[test]
    fn installation_blocks_new_operations() {
        let activity = Arc::new(UpdateActivity::default());
        let installation = activity.installation().expect("installation starts idle");
        assert!(activity.operation().is_err());
        drop(installation);
        assert!(activity.operation().is_ok());
    }
}
