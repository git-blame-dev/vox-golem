# Early retrieval head screening corpus v1

This fixed synthetic corpus screens one narrow decision under
`literal-labelled-context-v1`: whether a useful bounded external lookup is ready
before the first selected-model round. It does not select an answer model, grant
web permission, or describe commands or device actions.

## Annotation rubric

- **early_lookup**: a clear external query is sufficiently specified to prepare
  useful evidence now. This includes current public facts and explicit single
  official historical verification. An editing subtask does not erase a separate
  explicit verification request. A polite or implicit factual request can qualify.
- **model_led**: use the normal chosen-model path. Examples include explanations,
  supplied-data reasoning, translation, formatting, closing, missing essential
  context, and explicit planning or requirements work that must precede search.
  This label does **not** prohibit web access or mean research will never occur.
- Follow the operative instruction, not topic words. Quoted text can be adopted
  as a question or remain a transformation payload. The latest user instruction
  can replace an earlier one. All necessary prior context is supplied in `prev`;
  no case depends on an unseen answer.
- Public entities are question subjects, not user data. All invented payloads,
  worksheet states and identities are explicitly fictional or synthetic. No
  answers, current observations, private identifiers or live web data are stored.

Each split is a plain JSON array of `{id, family, expected, prompt, prev, reason}`.
Only `prompt` and optional `prev` are input content. IDs, families, expected labels
and rationales are annotation metadata, never encoder features. Input preparation
must use the experiment's existing versioned contract, not reconstruct it here.
Prompts are limited to 400 characters and previous context to 180. Short English
is intended to fit the 256-token reference; no tokenizer was run to certify that.

## Fixed split and grouping

| File | Families | Rows | early_lookup | model_led |
|---|---:|---:|---:|---:|
| `train.json` | 15 | 180 | 90 | 90 |
| `calibration.json` | 5 | 60 | 30 | 30 |
| `heldout.json` | 10 | 120 | 60 | 60 |
| Total | 30 | 360 | 180 | 180 |

Each family contains six instruction-contrast pairs, twelve rows with six of each
label. The paired public subject, relevant context, supplied payload, quotation
variants and close paraphrases stay together. Split membership is authored and
fixed before fitting; the recorded seed is an experiment identifier, not a claim
of randomized assignment. See `manifest.json` for the exact family assignments.

Families are defined by instruction/context construction rather than noun topic.
Training covers direct facts versus explanations, historical verification,
supplied calculations, quotation operations, referent completeness, planning
gates, mixed editing and verification, implicit needs, formatting versus refresh,
fictional datasets, resolved conditions, source-method questions, closing,
conflicting notes, and essential parameters. Calibration introduces scoped task
brackets, explicit repairs, date-source boundaries, ordered task pairs and
flag-selected branches. Held-out groups use different surrounding constructions;
their detailed content is deliberately not duplicated in this README.

Core competencies recur across splits so held-out labels are not a new task.
New constructions must nevertheless be independently reviewed for near-template
leakage and unfair ambiguity before fitting. Group labels alone do not establish
independence. Related operations such as quotation, revision, ordering and
clarification can still share semantic structure; this residual correlation is
an explicit limitation, not a certified disjoint distribution.

## Freeze and interpretation

An independent reviewer must adjudicate data before predictions or fitting.
The fitting owner must not inspect held-out prompts or labels before the head
artifact is frozen. Return only counts, filenames and cryptographic hashes to
that owner during preparation. Do not revise held-out labels after predictions.
Review issues belong to the parent-owned experiment record, not a competing task.

This is **not representative traffic** and is **not blind certification**.
Balance is artificial; rows within each family are statistically correlated.
Lexical instruction cues and authored grouping may inflate apparent performance.
Report family-aware limitations alongside row-level scores. A high classification
score does not demonstrate useful retrieval or lower supported-answer latency.

No training, feature extraction, inference, predictions, live lookup or model
download was performed in preparing these files. Structural validation is not
semantic adjudication, tokenization proof, or runtime acceptance.
