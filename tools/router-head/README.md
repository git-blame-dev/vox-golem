# Frozen-encoder ridge router head

`fit.py` remains the NumPy-only cached-feature tool below. `adapt.py` is a
separate, tightly bounded runner for the single approved 2026-10-18 adaptation
protocol; it does not change the app or authorize further experiments.

Offline NumPy-only fitter/evaluator for **already exported** BGE embeddings.
It does not load weights, run an encoder, train an encoder, discover artifacts,
read prompts, or invoke the app. Dependencies are Python 3.9+ and NumPy (already
provided by the execution environment); no SciPy/sklearn, pickle, or network.
This tool does not authorize additional fit runs: the approved experiment is
one frozen encoder / one binary decision head (modelled versus early lookup), then
stop for results. Its three fixed ridge candidates are internal selection, not
additional encoder training. `fit` cannot accept heldout input.

## Commands

Paths below are placeholders, not artifact autodiscovery. Outputs must be
absolute, outside every Git checkout, with an existing parent directory.
Inputs and existing files are never overwritten. Fit and evaluate are separate
commands so the parent can freeze the emitted head SHA-256 before evaluation.
New artifacts are created exclusively with mode 0600 on Unix and flushed before
returning success. Feature vectors are sensitive derived data, not anonymized
text; use only the authorized synthetic fixtures and keep artifacts outside Git.

```sh
python3 tools/router-head/fit.py fit \
  --train /external/train.features.json \
  --calibration /external/cal.features.json \
  --output /external/frozen-head.json \
  --encoder-sha256 <64-hex-encoder-sha256>

python3 tools/router-head/fit.py evaluate \
  --head /external/frozen-head.json \
  --features /external/heldout.features.json \
  --report /external/heldout-report.json

# Explicitly not heldout evidence; permits train/calibration ID/family overlap:
python3 tools/router-head/fit.py evaluate \
  --head /external/frozen-head.json \
  --features /external/regression.features.json --regression \
  --report /external/regression-report.json

python3 -m unittest discover -s tools/router-head -p 'test_*.py'
```

Successful stdout is aggregate JSON only: fit reports the written file hash,
selected lambda and calibration candidate counts; evaluation reports aggregate
counts and source hashes. IDs/families appear only in the head provenance,
never stdout. Operational errors return status 2 and bounded categorical JSON
on stderr, without input paths, vectors, provider content or tracebacks.

## Feature contract

The Rust exporter supplies one JSON object with exactly these keys:

```json
{
  "schema": "retrieval_features_v1",
  "input_version": "literal-labelled-context-v1",
  "embedding_dim": 384,
  "records": [
    {"index": 0, "id": "synthetic-id", "family": "synthetic-family",
     "label": 1, "embedding": [0.5, 0.5]}
  ]
}
```

The abbreviated embedding above illustrates structure only: real records must
have exactly **384** finite float32-representable numbers and nonzero L2 norm.
Labels are integers `0` (modelled) or `1` (early lookup), never booleans.
Indices are unique nonnegative signed-32-bit-range integers; IDs are unique.
IDs/families are 1–256 ASCII characters, begin alphanumeric, and otherwise use
alphanumerics or `_.:/-`. Each record has exactly the five shown keys. Files
are capped at 40 MiB and 1–512 records; duplicate JSON keys are rejected.
No raw text is accepted. Both labels must occur in training. Train/calibration
IDs **and** families must be disjoint. Default evaluation rejects overlap of
either set with either fitting split; `--regression` explicitly relaxes this
and labels the result `regression`, not `heldout`.

## Fixed statistical rules and limitations

Rows are L2-normalized using float64 norm arithmetic and stored as float32.
Fitting uses float64 NumPy LAPACK `solve` on augmented normalized rows (384
features plus a ones bias) with targets -1/+1:

`(X.T @ X + diag(lambda repeated 384 times, 0)) w = X.T @ y`.

Only lambdas **0.01, 0.1, 1.0** are considered. Each candidate is cast to
float32 before calibration scoring. Selection maximizes calibration correct
count, then minimizes false positives (unnecessary early lookup), then prefers
stronger regularization. No refit on calibration, threshold search or probability
calibration occurs. The exported head stores 384 float32 coefficients, float32
bias, threshold exactly zero, selected lambda, candidate aggregate counts,
source file hashes and ID/family sets, input version, encoder SHA-256 and
Python/NumPy versions. Scores are float32 dot products plus float32 bias;
score **>= 0** predicts early lookup. These are signed scores, not calibrated
probabilities. Evaluation validates head compatibility and finite float32
parameters. It cannot independently verify encoder identity from feature files
because the feature contract does not carry that identity; the parent must
ensure the same frozen encoder produced every export.

Reports contain TP/TN/FP/FN, correct count, fraction accuracy, percent correct,
precision and recall (null when undefined). `n`, `expected` and coverage refer
only to all records supplied in that validated feature file (`expected=n`,
coverage=1); the tool cannot establish completeness against an external manifest.
Classification timing on cached features is **not** encoder/inference acceptance,
app/phone latency, a 250 ms guarantee, or a net latency speedup. Small curated
sets and calibration selection do not establish population-level accuracy.
BLAS/LAPACK and floating-point reduction differences can affect near-zero
scores across runtimes; independent production scoring parity remains necessary.
The file hash freezes exact exported bytes, not cross-platform numerical
reproducibility. Synthetic unittest fixtures exercise only this offline path.

## Approved embedding adaptation runner

Use the already prepared isolated Python environment with the approved CPU
packages. Framework imports are lazy: the ordinary synthetic tests require
only NumPy and never load an encoder or perform training. No dependencies or
weights are downloaded by this runner. All model loads are local-only CPU,
remote code disabled, safetensors-only. Offline/telemetry-disable flags are set
before importing frameworks.

```sh
<prepared-python> tools/router-head/adapt.py run \
  --protocol /external/adaptation-protocol-v1-20261018.json \
  --model /external/immutable-base-model \
  --train /external/train.json --everyday /external/everyday.json \
  --calibration /external/calibration.json \
  --output /external/new-run-directory

# Parent records freeze.json SHA-256 before this separate command:
<prepared-python> tools/router-head/adapt.py evaluate \
  --run /external/new-run-directory --cases /external/fresh.json \
  --report /external/new-fresh-report.json
```

The protocol's complete canonical JSON is pinned, including approval status,
base weight SHA-256, caps and gates; changes/unknown fields fail closed. `run`
has no held-out argument. Its sources must contain exactly 180 + 80 balanced
training cases (130 per label) and 60 calibration cases. Raw case arrays use
`id`, `family`, `expected` (`model_led` or `early_lookup`), `prompt`, and optional
`prev`; other metadata is ignored for features. IDs/families follow the feature
consumer's bounded ASCII identifier contract. Literal input matches Rust exactly:
absent/null prior means the prompt verbatim, otherwise
`Prior user context:\n{prev}\n\nCurrent user request:\n{prompt}`. No lexical rules
or normalization are applied. Labels enter pair training and ridge fitting,
never embedding text. Duplicate IDs/literal inputs and train/calibration family
overlap are rejected. Raw inputs are capped at 32 KiB and 512 rows per file.
Tokenizer preflight includes special tokens and rejects more than 256 tokens
before any control encoding or training; there is no silent truncation.

The unchanged encoder first exports the same 260/60 data and fits a control
ridge head. A fresh base load then receives embedding-only SetFit training:
CosineSimilarityLoss, seed 20261018, two CPU threads, batches (8,8), epochs
(1,1), exactly 64 optimizer steps (at most 512 generated pairs), learning rates
(2e-5,2e-5), warmup .1, no AMP/evaluation/checkpoint selection/integrations.
The classifier placeholder is never trained or serialized. A monotonic
900-second body-training deadline is checked after each step and on return;
expired or incomplete runs cannot freeze or resume. Metadata-only loss logs
are emitted every 16 steps. Elapsed time is body-training metadata, not an
inference/app latency measurement.

Only the adapted SentenceTransformer body is saved with safe serialization,
without a generated model card; the original upstream card is retained. Saved
weights are reloaded before feature export and identical ridge fitting. Finite
384-dimensional unit vectors are required, using the exact `fit.py` cache
schema and numerical/head-selection rules. Parameter-byte hashes must change;
original base files must remain unchanged. Model outputs live inside an
exclusive new external run directory; files are private (0600 with CLI umask
077), never pickle/sklearn head serialization. Prompts/vectors are not stdout.

`freeze.json` is written **last**, only after both model inventories, heads,
feature-cache hashes, source/protocol hashes, package versions and completed
64-step training metadata exist. Partial run directories are deliberately left
without a freeze marker and cannot be evaluated. Do not retry or resume them
automatically. The parent owns the authorized actual run, independent review,
freeze hash recording and fresh evaluation. Synthetic helper tests are not
proof of the actual framework/training boundary.

Evaluation verifies frozen files/model inventories and head identities before
reading fresh cases. It performs **no fitting**. Fresh inputs must contain
120 cases: 72 `ordinary`, 36 `context`, 12 `mixed` in `stratum`, balanced in
each stratum, with boolean `critical_missing_anchor`: exactly twelve negative
cases, six ordinary and six context. These fields affect reports/gates only. IDs, families and exact-input
fingerprints must be separate from fitting inputs. New private feature caches
are written next to the new report, one per frozen encoder, then `fit.evaluate`
checks held-out provenance. Reports contain overall/stratum confusion counts,
missing-anchor false positives and the fixed .95 accuracy/precision, .90
recall/context/mixed, zero-critical-FP gates. No regression relaxation is added;
old diagnostic datasets can still use the explicit `fit.py --regression` path.
Results are synthetic screening only, not population quality, optimized phone
performance, final-answer benefit or permission for another training run.
