# Retrieval adaptation evaluation v1

## Provenance and scope

Synthetic English requests and synthetic conversational context. Public names serve only as query anchors; no factual answers, live web data, or personal records are included. This deliberately balanced collection is not a sample of wild traffic. The ten families contain related cases and must not be treated as independent population evidence. No holdout certification is claimed.

## Intended counts

- 120 cases: 60 `early_lookup`, 60 `model_led`.
- Ordinary: 72 cases, 36 per expected route.
- Context: 36 cases, 18 per expected route.
- Mixed: 12 cases, 6 per expected route.
- Ten families, each with 12 cases, six per expected route.
- Stratum is uniform within each family: six entire ordinary families, three entire context families, and one entire mixed family. Every family has six cases per expected route.
- Exactly 12 critical missing-anchor cases, all `model_led`: six ordinary, six context, none mixed.

## Rubric

`early_lookup` means a clear, bounded external query has enough anchors to gather useful evidence before the first selected-model round. Current public conditions, versions, prices, schedules, hours and policies can qualify. Explicit official historical verification can qualify even if the fact may already be known.

`model_led` means the normal model path should handle the first step: direct reasoning, supplied arithmetic, writing, definitions, ordinary historical questions without a verification request, conversational closing, essential clarification, or explicitly requested planning before comparison. This does not deny later web use and does not imply that a native executor already exists.

Critical missing anchors are essential absent identities, places or other query targets, not a lack of permission for a future action. Relevant context is wholly supplied in `prev`; no unseen assistant answers are assumed. Explicit current requests supersede earlier context.

Labels describe an opportunity proxy, not guaranteed source availability or an end-to-end latency benefit. Related topics cross operations and routes; topic alone is not the intended decision rule.

## Input boundary and freeze

Only `prompt` and, when present, `prev` are encoder inputs. IDs, family, stratum, critical-anchor flags, expected routes and reasons are metadata and must remain outside encoder input. This document intentionally contains no case examples.

Freeze both evaluation artifacts and the matched adaptation/control artifact pairs before exposing prompts or case labels to the parent. An independent reviewer must read all raw evaluation cases before fitting. Review and freeze precede predictions; schema checks alone are not semantic review. No model fitting, inference, source retrieval or runtime acceptance is represented by these artifacts.

## Test-data repair provenance

The original annotation review preceded training. The initial separation guards found one exact training-input duplicate and twelve ID collisions and stopped evaluation before test predictions. The user approved a fixed-artifact, test-data-only repair; the original JSON and Markdown are archived outside Git under `artifacts/router-adaptation/evaluation-repair-v1-20261018/` in the outer project directory.

Only the original `b01` input and its reason were replaced, and all 120 case IDs were namespaced with `eval1-`. All labels, families, strata and critical-anchor flags remain unchanged. The matched models and heads were already frozen and remain unchanged; no model results informed the replacement. Repair review and freezing of the repaired evaluation artifacts must precede predictions.
