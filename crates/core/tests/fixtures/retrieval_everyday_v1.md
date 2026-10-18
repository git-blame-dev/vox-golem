# Everyday retrieval routing qualification v1

## Scope and provenance

These 80 requests are newly authored synthetic English examples for prospective qualification of a previously frozen routing head. They are held out from the existing fit, not a truly blind population sample. No previous datasets, predictions, head weights or results were consulted in authoring. Keep the older stress set separate: this fixture addresses everyday requests rather than replacing stress coverage. No accuracy or wild-traffic prevalence estimate follows from the deliberate balance here.

Data review must occur before predictions are inspected. Freeze annotations after that review and before prediction exposure; do not revise labels in response to predictions. Any later disagreement should be documented separately rather than silently relabeling this version.

## Annotation rubric

- `early_lookup`: a clear, bounded external query is sufficiently specified for evidence to be useful before the selected model round. This includes current weather, releases, hours, public schedules, prices, rules and actual source-verification requests. A stable fact qualifies when the user explicitly asks for verification, not merely because it is factual.
- `model_led`: the normal selected-model path, including reasoning from supplied inputs, stable explanations, text transformation, creative work, closing turns, missing essential query context, and explicit requests to establish preferences or a plan before searching. It does not mean the model may never retrieve later, nor does it depend on an explicit refusal of web use.
- Search complexity alone is not a negative criterion. Requirements-first comparisons are negative because the immediate request is to establish needs, not because comparisons are inherently unsuitable for retrieval.
- Context examples include the referents needed to interpret their follow-up in `prev`; no unseen assistant answer is required. Missing-context negatives intentionally leave an essential anchor unresolved.
- Mixed examples distinguish simple lookup plus presentation from transformations scoped solely to supplied text or explicit planning before search.

## Composition

| Stratum | early_lookup | model_led | Total |
| --- | ---: | ---: | ---: |
| ordinary | 24 | 24 | 48 |
| context | 12 | 12 | 24 |
| mixed | 4 | 4 | 8 |
| Total | 40 | 40 | 80 |

The JSON is an array of records containing `id`, `family`, `stratum`, `expected`, `prompt`, `prev` and `reason`. Labels are lowercase. Prompts are at most 400 characters; non-null previous turns are at most 180 characters. The existing parser may ignore `stratum` and `reason`; these fields preserve review context.

## Correlation and interpretation

Family identifiers deliberately group near-paraphrases, contextual variants and related payloads. Family members are correlated, not independent evidence. Families also span strata where applicable; record counts should not be interpreted as independent sampling units. Broader thematic overlap can remain even between different families. The set is not dominated by a single venue, product or syntax, but is still hand-constructed rather than representative traffic.

Interpret relative dates such as “tomorrow,” “this Friday,” “past week” and “next” against the evaluation request's date and relevant locale. Do not freeze a live factual answer into a routing label. Questions about whether a release has occurred remain useful external queries both before and after release. Current figures and policies can change without changing the intended routing decision.

The labels express whether an early query is useful, not whether an external service is reachable, a source has published an answer, or a snippet is sufficient evidence. A missing announcement, no matching recall, temporary outage or ambiguous search result is an outcome to handle downstream, not grounds to reverse the routing label. Weather and schedule horizons, seasonal beach zones, passport exceptions and changing fare categories may require careful downstream qualification; the supplied anchors still support useful initial evidence gathering. Public source and entity names are query anchors, not claims that a particular page exists or was consulted.

Review focus before prediction exposure: confirm that requirements-first cases genuinely gate lookup; that source-verification positives retain an explicit verification request; and that relative dates, venue policies and ticket categories remain sufficiently bounded for useful initial retrieval. No external factual answers or source availability were verified during fixture creation.
