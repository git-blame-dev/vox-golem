#![cfg(feature = "web-search")]

use serde_json::json;
use voxgolem_core::{
    agent_pipeline::{validate_review_input, ReviewInput, SourceEvidence, StageStatus},
    assistant::Content,
    search::Provider,
};
use voxgolem_platform::web::{decode_result, Operation};

#[test]
fn blank_and_missing_provider_titles_survive_real_review_input_validation() {
    for provider in [Provider::Parallel, Provider::Exa] {
        for title in [None, Some(""), Some("  \t")] {
            let value = match provider {
                Provider::Parallel => {
                    let mut item =
                        json!({"url":"https://example.com","excerpts":["Synthetic evidence"]});
                    if let Some(title) = title {
                        item["title"] = json!(title);
                    }
                    json!({"structuredContent":{"results":[item]}})
                }
                Provider::Exa => {
                    let heading = title
                        .map(|title| format!("Title: {title}\n"))
                        .unwrap_or_default();
                    json!({"content":[{"type":"text","text":format!("{heading}URL: https://example.com\nHighlights:\nSynthetic evidence")}]})
                }
            };
            let result = decode_result(
                provider,
                &Operation::Search {
                    objective: "Synthetic".into(),
                    queries: vec!["Synthetic".into()],
                    limit: 3,
                },
                &value,
                "synthetic-run",
                &mut 0,
            )
            .unwrap();
            let source = &result.sources[0];
            assert_eq!(source.title, source.url);
            validate_review_input(&ReviewInput {
                original_request: "Synthetic question".into(),
                canonical_history: vec![],
                instant: StageStatus::Success(Content::Text("Synthetic answer".into())),
                deep: StageStatus::Failure("Disabled".into()),
                materiality_policy: "Material facts only".into(),
                sources: vec![SourceEvidence {
                    url: source.url.clone(),
                    title: source.title.clone(),
                }],
            })
            .unwrap();
        }
    }
}
