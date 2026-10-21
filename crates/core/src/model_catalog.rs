//! Static presentation metadata, not runtime readiness or provider admission.
//! Regenerate consumers with `python3 scripts/generate-model-catalog.py`.
use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelDescriptor {
    pub id: &'static str,
    pub label: String,
    /// Native routing labels can differ from the browser's descriptive fallback.
    pub native_label: String,
    pub agent: bool,
    pub preview: bool,
    pub family_id: &'static str,
    pub family_label: String,
    pub short_family_label: &'static str,
    pub effort: &'static str,
    /// Explicit historical Android short labels; absent means use the full label.
    pub android_label: Option<String>,
    /// Optional descriptors do not expand InstantModelChoice/provider support.
    pub android_group: Option<&'static str>,
}

#[derive(Serialize)]
pub struct ModelDefaults {
    pub instant: &'static str,
    pub deep: &'static str,
    pub review: &'static str,
}

pub const DEFAULT_MODEL_CHOICES: ModelDefaults = ModelDefaults {
    instant: "local-fast",
    deep: "opencode-sol-high",
    review: "opencode-sol-high",
};

pub fn model_catalog() -> Vec<ModelDescriptor> {
    let mut models = Vec::with_capacity(44);
    for (id, name, short, preview, group) in [
        (
            "local-fast",
            "Local: Llama 3.2 3B",
            "Llama 3.2 3B",
            true,
            None,
        ),
        ("local-quality", "Local: Quality", "Quality", false, None),
        (
            "gemini-nano",
            "Local: Gemini Nano",
            "Gemini Nano",
            false,
            Some("optional"),
        ),
    ] {
        models.push(ModelDescriptor {
            id,
            label: name.into(),
            native_label: if id == "local-fast" {
                "Local: Fast"
            } else {
                name
            }
            .into(),
            agent: false,
            preview,
            family_id: id,
            family_label: name.into(),
            short_family_label: short,
            effort: "none",
            android_label: None,
            android_group: group,
        });
    }
    for (family, provider, name, group, variants) in [
        (
            "custom-sol",
            "Custom",
            "GPT-6.1 Sol",
            Some("custom"),
            &[
                ("custom-sol-low", "low"),
                ("custom-sol-medium", "medium"),
                ("custom-sol-high", "high"),
                ("custom-sol-xhigh", "xhigh"),
                ("custom-sol-max", "max"),
            ][..],
        ),
        (
            "custom-luna6",
            "Custom",
            "GPT-6 Luna",
            Some("custom"),
            &[
                // Persisted historical IDs intentionally differ from the family ID.
                ("custom-luna-none", "none"),
                ("custom-luna6-low", "low"),
                ("custom-luna6-medium", "medium"),
                ("custom-luna6-high", "high"),
                ("custom-luna6-xhigh", "xhigh"),
                ("custom-luna6-max", "max"),
            ][..],
        ),
        (
            "custom-luna56",
            "Custom",
            "GPT-5.6 Luna",
            Some("custom"),
            &[
                ("custom-luna56-none", "none"),
                ("custom-luna-low", "low"),
                ("custom-luna56-medium", "medium"),
                ("custom-luna56-high", "high"),
                ("custom-luna56-xhigh", "xhigh"),
                ("custom-luna56-max", "max"),
            ][..],
        ),
        (
            "opencode-sol",
            "OpenCode",
            "GPT-6.1 Sol",
            None,
            &[
                ("opencode-sol-low", "low"),
                ("opencode-sol-medium", "medium"),
                ("opencode-sol-high", "high"),
                ("opencode-sol-xhigh", "xhigh"),
                ("opencode-sol-max", "max"),
            ][..],
        ),
        (
            "opencode-luna",
            "OpenCode",
            "GPT-5.6 Luna",
            None,
            &[
                ("opencode-luna-none", "none"),
                ("opencode-luna-low", "low"),
                ("opencode-luna-medium", "medium"),
                ("opencode-luna-high", "high"),
                ("opencode-luna-xhigh", "xhigh"),
                ("opencode-luna-max", "max"),
            ][..],
        ),
        (
            "agy-argon",
            "Agy",
            "Gemini 4 Argon",
            Some("optional"),
            &[
                ("agy-argon-low", "low"),
                ("agy-argon-medium", "medium"),
                ("agy-argon-high", "high"),
            ][..],
        ),
        (
            "agy-flash",
            "Agy",
            "Gemini 3.8 Flash",
            Some("optional"),
            &[
                ("agy-flash-none", "none"),
                ("agy-flash-low", "low"),
                ("agy-flash-medium", "medium"),
                ("agy-flash-high", "high"),
            ][..],
        ),
        (
            "agy-opus",
            "Agy",
            "Claude Opus 5.5",
            Some("optional"),
            &[
                ("agy-opus-low", "low"),
                ("agy-opus-medium", "medium"),
                ("agy-opus-high", "high"),
            ][..],
        ),
        (
            "agy-sonnet",
            "Agy",
            "Claude Sonnet 5.5",
            Some("optional"),
            &[
                ("agy-sonnet-low", "low"),
                ("agy-sonnet-medium", "medium"),
                ("agy-sonnet-high", "high"),
            ][..],
        ),
    ] {
        for &(id, effort) in variants {
            let suffix = match effort {
                "none" if provider != "Agy" => "No Reasoning",
                "none" => "None",
                "low" => "Low",
                "medium" => "Medium",
                "high" => "High",
                "xhigh" => "X-High",
                "max" => "Max",
                _ => unreachable!(),
            };
            let android_label = if group == Some("custom") {
                let short = match id {
                    "custom-luna-none" | "custom-luna-low" => "Luna",
                    _ if family == "custom-sol" => "Sol",
                    _ => name,
                };
                Some(format!(
                    "{short} {}",
                    if effort == "none" { "None" } else { suffix }
                ))
            } else {
                None
            };
            models.push(ModelDescriptor {
                id,
                label: format!("{provider}: {name} {suffix}"),
                native_label: format!(
                    "{provider}: {name} {}",
                    if effort == "none" { "None" } else { suffix }
                ),
                agent: effort != "none",
                preview: false,
                family_id: family,
                family_label: format!("{provider}: {name}"),
                short_family_label: name,
                effort,
                android_label,
                android_group: group,
            });
        }
    }
    models
}
