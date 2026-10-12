use crate::ErrorCode;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use tokio::sync::watch;

const MAX_ACTIVE_REQUESTS: usize = 4;

#[derive(Default)]
pub(crate) struct Registry {
    active: Mutex<HashMap<String, watch::Sender<bool>>>,
}

pub(crate) fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(Registry::default)
}

pub(crate) struct Registration<'a> {
    registry: &'a Registry,
    request_id: String,
    pub cancelled: watch::Receiver<bool>,
}

impl Registry {
    pub fn register(&self, request_id: &str) -> Result<Registration<'_>, ErrorCode> {
        let mut active = self.active.lock().map_err(|_| ErrorCode::InternalError)?;
        if active.contains_key(request_id) {
            return Err(ErrorCode::DuplicateRequest);
        }
        if active.len() >= MAX_ACTIVE_REQUESTS {
            return Err(ErrorCode::Busy);
        }
        let (sender, cancelled) = watch::channel(false);
        active.insert(request_id.to_string(), sender);
        Ok(Registration {
            registry: self,
            request_id: request_id.to_string(),
            cancelled,
        })
    }

    pub fn cancel(&self, request_id: &str) {
        if let Ok(active) = self.active.lock() {
            if let Some(sender) = active.get(request_id) {
                let _ = sender.send(true);
            }
        }
    }
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.registry.active.lock() {
            active.remove(&self.request_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_duplicate_until_original_worker_retires_even_after_cancel() {
        let registry = Registry::default();
        let registration = registry.register("synthetic-request").unwrap();
        assert!(matches!(
            registry.register("synthetic-request"),
            Err(ErrorCode::DuplicateRequest)
        ));
        registry.cancel("synthetic-request");
        assert!(*registration.cancelled.borrow());
        assert!(matches!(
            registry.register("synthetic-request"),
            Err(ErrorCode::DuplicateRequest)
        ));
        drop(registration);
        let replacement = registry.register("synthetic-request").unwrap();
        assert!(!*replacement.cancelled.borrow());
    }

    #[test]
    fn bounds_workers_and_isolates_cancellation() {
        let registry = Registry::default();
        let registrations: Vec<_> = (0..MAX_ACTIVE_REQUESTS)
            .map(|index| registry.register(&format!("synthetic-{index}")).unwrap())
            .collect();
        assert!(matches!(
            registry.register("synthetic-overflow"),
            Err(ErrorCode::Busy)
        ));
        registry.cancel("synthetic-missing");
        registry.cancel("synthetic-1");
        for (index, registration) in registrations.iter().enumerate() {
            assert_eq!(*registration.cancelled.borrow(), index == 1);
        }
        drop(registrations);
        assert!(registry.register("synthetic-new").is_ok());
    }

    #[test]
    fn another_thread_can_cancel_the_registered_worker() {
        let registry = Registry::default();
        let registration = registry.register("synthetic-worker").unwrap();
        std::thread::scope(|scope| {
            scope
                .spawn(|| registry.cancel("synthetic-worker"))
                .join()
                .unwrap();
        });
        assert!(*registration.cancelled.borrow());
    }
}
