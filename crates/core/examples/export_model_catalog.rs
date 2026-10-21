use voxgolem_core::model_catalog;

fn main() {
    println!(
        "{}",
        serde_json::json!({
            "models": model_catalog::model_catalog(),
            "defaults": model_catalog::DEFAULT_MODEL_CHOICES,
        })
    );
}

#[cfg(test)]
mod tests {
    use super::model_catalog::*;
    use voxgolem_core::ipc::{AgentModelChoice, InstantModelChoice};

    #[test]
    fn metadata_preserves_canonical_membership_and_optional_boundary() {
        let catalog = model_catalog();
        let ids: std::collections::HashSet<_> = catalog.iter().map(|m| m.id).collect();
        assert_eq!(ids.len(), catalog.len());
        assert_eq!(catalog.len(), 44);
        for choice in InstantModelChoice::ALL {
            assert!(ids.contains(choice.as_str()));
            let descriptor = catalog
                .iter()
                .find(|model| model.id == choice.as_str())
                .unwrap();
            assert_eq!(descriptor.native_label, choice.label());
        }
        for model in &catalog {
            let encoded = serde_json::to_string(model.id).unwrap();
            let canonical = serde_json::from_str::<InstantModelChoice>(&encoded).is_ok();
            assert_eq!(canonical, model.android_group != Some("optional"));
            if canonical {
                assert_eq!(
                    model.agent,
                    serde_json::from_str::<AgentModelChoice>(&encoded).is_ok(),
                    "{}",
                    model.id
                );
            }
        }
        for id in [DEFAULT_MODEL_CHOICES.deep, DEFAULT_MODEL_CHOICES.review] {
            assert!(catalog.iter().any(|m| m.id == id && m.agent));
        }
        assert!(ids.contains(DEFAULT_MODEL_CHOICES.instant));
    }
}
