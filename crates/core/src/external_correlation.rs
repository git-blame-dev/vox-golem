use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::sync::OnceLock;

/// Randomized per-process projection: local identities never become provider metadata.
struct ExternalCorrelation {
    left: RandomState,
    right: RandomState,
}

impl ExternalCorrelation {
    fn new() -> Self {
        Self {
            left: RandomState::new(),
            right: RandomState::new(),
        }
    }

    fn id(&self, local_key: &str) -> String {
        format!(
            "session-{:016x}{:016x}",
            self.left.hash_one(local_key),
            self.right.hash_one(local_key)
        )
    }
}

pub fn external_id(local_key: &str) -> String {
    static CORRELATION: OnceLock<ExternalCorrelation> = OnceLock::new();
    CORRELATION
        .get_or_init(ExternalCorrelation::new)
        .id(local_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuity_is_process_scoped_and_does_not_expose_local_identity() {
        let process = ExternalCorrelation::new();
        let next_process = ExternalCorrelation::new();
        let key = "synthetic-replica:42";
        let id = process.id(key);
        assert_eq!(id, process.id(key));
        assert_ne!(id, process.id("synthetic-replica:43"));
        assert_ne!(id, next_process.id(key));
        assert_eq!(id.len(), 40);
        assert!(!id.contains("synthetic"));
        assert!(!id.contains(':'));
    }
}
