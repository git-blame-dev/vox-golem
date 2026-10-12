use std::future::Future;
use tokio::time::Instant;
use voxgolem_core::search::Failure;

#[derive(Clone)]
pub struct Cancellation {
    local: tokio::sync::watch::Sender<bool>,
    parent: Option<tokio::sync::watch::Receiver<bool>>,
}

impl Default for Cancellation {
    fn default() -> Self {
        Self {
            local: tokio::sync::watch::channel(false).0,
            parent: None,
        }
    }
}

impl Cancellation {
    pub fn from_receiver(parent: tokio::sync::watch::Receiver<bool>) -> Self {
        Self {
            parent: Some(parent),
            ..Default::default()
        }
    }
    pub fn cancel(&self) {
        self.local.send_replace(true);
    }
    pub fn is_cancelled(&self) -> bool {
        *self.local.borrow()
            || self
                .parent
                .as_ref()
                .is_some_and(|parent| *parent.borrow() || parent.has_changed().is_err())
    }
    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let mut local = self.local.subscribe();
        let mut parent = self.parent.clone();
        tokio::select! {
            _ = async { let _ = local.wait_for(|value|*value).await; } => {},
            _ = async {
                match &mut parent {
                    Some(parent) => { let _ = parent.wait_for(|value|*value).await; },
                    None => std::future::pending::<()>().await,
                }
            } => {},
        }
    }
    pub async fn guard<T>(
        &self,
        deadline: Instant,
        future: impl Future<Output = T>,
    ) -> Result<T, Failure> {
        tokio::select! {
            biased;
            _ = self.cancelled() => Err(Failure::Cancelled),
            _ = tokio::time::sleep_until(deadline) => Err(Failure::Deadline),
            result = future => if self.is_cancelled() { Err(Failure::Cancelled) } else { Ok(result) },
        }
    }
}
