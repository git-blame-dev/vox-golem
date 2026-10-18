# Retrieval opportunities v1

## Annotation rubric

This synthetic development/qualification corpus labels an **opportunity before the first selected-model round**, not whether the eventual answer should use the web.

- `early_lookup`: one clear, bounded external lookup has enough context to form a useful query immediately. Current weather, a named project's current version, and a single official historical fact can qualify. A small formatting or explanation step afterward does not remove the opportunity.
- `model_led`: take the normal selected-model path first. Examples include supplied-text transformations, reasoning, stable explanations, conversational closure, deescalation, missing required context, and explicitly requested planning or questions before research. This label does **not** prohibit later research or imply that research is unnecessary.

Do not label by topic words alone. Quoted research instructions are not operative instructions; an explicit request to verify a quoted claim is. Complex research is not automatically negative: the negative research cases here explicitly require planning first or lack essential requirements. Use only `prompt` and `prev`; never assume an unseen assistant answer, location, date, or project identity.

## Schema and composition

The JSON is a plain array of 60 objects, balanced at 30 `early_lookup` and 30 `model_led`. Each object contains only:

- `id`: unique lower-ASCII identifier.
- `family`: lower-ASCII grouping identifier.
- `expected`: one of the two labels above.
- `prompt`: current user text, at most 400 characters.
- `prev`: self-contained prior context, at most 180 characters, or `null`.
- `reason`: short human annotation rationale, not a retrieval query.

There are no `exp` or `py_norm` fields; those belong to a distinct gold task. Short, simple English is intended to fit the existing 256-token reference context; character bounds alone do not prove tokenizer-specific non-truncation.

| Family | Cases | Early / model | Purpose |
| --- | ---: | ---: | --- |
| `weather` | 5 | 2 / 3 | Current conditions versus explanation, missing location, and supplied forecast |
| `software_versions` | 5 | 3 / 2 | Current releases versus concepts and scope gathering |
| `official_rules` | 5 | 3 / 2 | Bounded authoritative rules versus fictional supplied logic and general explanation |
| `event_results` | 5 | 3 / 2 | Bounded historical records versus supplied arithmetic and concepts |
| `anchored_followups` | 5 | 3 / 2 | Resolved place/date/project references versus incomplete or supplied context |
| `quoted_text` | 5 | 2 / 3 | Operative verification versus quoted research wording |
| `mixed_requests` | 5 | 3 / 2 | Lookup plus lightweight output versus editing and requirements first |
| `closing_and_format` | 5 | 2 / 3 | Explicit verification before formatting versus closure and format-only work |
| `planning_first` | 5 | 2 / 3 | Single actionable facts within plans versus explicit planning gates |
| `missing_requirements` | 5 | 2 / 3 | Context-resolved followups versus essential missing identifiers |
| `explanation_and_reassurance` | 5 | 2 / 3 | Fresh-fact-dependent explanations versus stable explanations and deescalation |
| `paraphrase_controls` | 5 | 3 / 2 | Polite phrasing, uppercase variants, and quoted instruction controls |

## Context, mixed requests, and privacy

All prompts are synthetic. Public cities, projects, institutions, and awards are query subjects, not user identities or personal history. Invented venues and match data are explicitly marked. No live answers, executed web queries, private data, commands, device actions, or system-setting requests are included. Date-anchored followups place the needed date in `prev`; relative dates elsewhere are interpreted at evaluation time, not against a hidden fixture creation date.

Mixed requests qualify for early lookup only when the immediate lookup is already bounded and useful; later presentation is not a prerequisite. When the user asks for a plan or missing requirements first, honor that ordering instead of opportunistically searching.

## Interpretation and limits

Annotations are frozen for review before any fixture predictions. No predictions, accuracy results, actual encoder execution, or measured latency results are included. Reviewers should adjudicate the opportunity labels before running the corpus; do not train or tune on subsequent predictions and represent the result as independent evaluation.

This is a small English development/qualification set, **not a blind holdout** or a training corpus. Topic repetition is deliberate for crossing action and context with domain vocabulary; it limits independence and coverage. Family-level results and individual errors matter alongside aggregate accuracy. The set does not establish multilingual robustness, adversarial resistance, provider reliability, or production routing quality. Static historical questions intentionally test bounded verification even when a model might recall the answer.

An expected opportunity is **not actual speedup evidence**. A useful query may fail, return stale information, incur more latency than it saves, or require model-guided refinement. Routing accuracy alone proves neither answer correctness nor faster end-to-end completion. Those claims require separately authorized runtime measurements with equivalent conditions, retrieval outcomes, selected-model behavior, and complete-path timing.
