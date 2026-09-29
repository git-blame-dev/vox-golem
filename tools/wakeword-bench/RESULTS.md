# Validated synthetic v1 results

## Reproduction evidence

Completed two independently generated and scored runs of `hey-livekit-synthetic-v1` using eSpeak NG 1.52.0 with deterministic mode enabled:

- 282 cases per run: 280 clips and two 120-second negative streams.
- 14,072 score points per run.
- All 282 first-detection comparisons against the production buffered runtime passed in each run.
- Every generated WAV, score, sample position, buffered-runtime decision, and policy result matched exactly between runs. Elapsed wall-clock timings were excluded.
- Classifier SHA-256: `8bd634fb7acf1e52d06307fb8f460abf2c7a40e561fb4532fc56e087e0246f62` (LiveKit official example model).
- Suite SHA-256: `91dcb4588fa6d062896bdfde42df376452e2a57fd2e1aa188c66c70b167adf4b`.
- Corpus manifest SHA-256: `6549712dc143632531dcfc101a588adc2263467e4bd1d06d912b6fca6a4c3a65`.

Full local artifacts are under `target/wakeword-bench/validated-v1/` and `target/wakeword-bench/verified-repeat-v1/`. They are ignored by Git. Earlier exploratory artifacts without deterministic synthesis are superseded; an interrupted repeat remains marked incomplete and is not used as evidence.

## Policy comparison

Calibration selected **threshold 0.90 with two consecutive hits**. This candidate also passed the separate holdout gate for this corpus, preserving every baseline-positive case while reducing negative-clip triggers and stream episodes.

| Metric | Current policy: 0.68 / 1 hit | Candidate: 0.90 / 2 hits |
| --- | ---: | ---: |
| Calibration target detections | 48/56 | 48/56 |
| Calibration negative clips triggered | 36/84 | 22/84 |
| Calibration stream episodes, 120 seconds | 11 | 7 |
| Holdout target detections | 48/56 | 48/56 |
| Holdout negative clips triggered | 46/84 | 32/84 |
| Holdout stream episodes, 120 seconds | 14 | 5 |
| Holdout median latency from utterance onset | 760 ms | 880 ms |

Across both splits, both policies detected all **96 target conditions with prefilled audio history** and missed all **16 immediate cold-start target conditions**. Cold-start detection is a separate reproducible failure. Raising sensitivity or changing the streak did not resolve it in the tested grid.

The candidate reduced triggered negative clips from 82 to 54 across the two splits, but still has substantial errors on deliberately confusing speech. This is a relative improvement, not an absolute accuracy qualification.

## Limits and contrary evidence

The corpus uses correlated eSpeak voices/variants and synthetic perturbations. Its negative streams deliberately repeat confusable phrases, so event counts must not be presented as everyday false-wakes/hour. The replay bypasses hardware capture, device-rate resampling and UI transport.

A separate earlier Windows TTS experiment is material contrary evidence: at 0.90/two hits, target detections fell from 9/16 to 6/16 for Hazel and from 16/16 to 14/16 for Zira. Thus passing this eSpeak corpus does **not** establish that the policy preserves accuracy across engines or human voices. Those examples should be retained as regression coverage when evaluating a deployment change. No live application setting was changed by this benchmark.

## Checks

- 21 Python benchmark tests passed, including repeated synthesis of the stochastic voice variant under deterministic mode, corpus/label/trace validation and executable-snapshot provenance.
- 31 Rust example tests passed, including included production wake tests and mismatched-WAV rejection.
- Rust formatting, all-target desktop Clippy with warnings denied, and diff whitespace checks passed.
- Independent scoped source review found no actionable findings after remediation.

See [README.md](README.md) for the one-command benchmark and independent-run comparison commands. New model or policy development must not treat this now-inspected holdout as an untouched test set indefinitely.
