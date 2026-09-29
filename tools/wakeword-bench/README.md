# Reproducible wake-word benchmark

Generate artificial speech, replay it through the production wake-word implementation, and compare detection policies on a fixed corpus. The suite is for **“hey livekit”** with the existing **normalized-f32 LiveKit feature profile**. Using a different classifier does not automatically make its phrase or preprocessing compatible with this suite.

## Run

From the repository root on Linux, with the project's existing Rust build prerequisites plus Python 3.10+, eSpeak NG, FFmpeg and GNU coreutils (`sha256sum`) available:

```bash
make test-wakeword-bench
make wakeword-bench WAKE_WORD_MODEL=/absolute/path/to/hey_livekit.onnx
```

The command does not install dependencies, download models, record the microphone, play sounds, or change the running app. Supply a local classifier yourself. The official example classifier used during investigation has SHA-256 `8bd634fb7acf1e52d06307fb8f460abf2c7a40e561fb4532fc56e087e0246f62`; it differs from the SDK's smaller test-fixture classifier.

The default artifact directory is ignored by Git: `target/wakeword-bench/validated-v1/`. Choose a new directory when changing the model, scorer, generator, or suite:

```bash
make wakeword-bench \
  WAKE_WORD_MODEL=/absolute/path/to/hey_livekit.onnx \
  WAKE_BENCH_DIR=target/wakeword-bench/comparison-v1
```

Reusing a completed run verifies its fingerprints and recomputes its report from the frozen traces. It does not silently replace the corpus or rerun inference. Use a fresh artifact directory for an independent rerun. A failed/incomplete scoring run is rejected on reuse; its partial output remains available for investigation.

## Fixed v1 corpus

[`suite.json`](suite.json) defines all phrases, voice settings, splits, perturbations, thresholds and temporal policies before evaluation:

- **280 clip cases:** two voice/speed settings per split, four target texts and six negative texts, each under seven scenarios.
- **Two 120-second negative speech streams:** one per split, built only from that split's negative phrases with 250 ms gaps.
- **Scenarios:** clean audio, quarter gain, seeded white noise at a requested 10 dB SNR, a synthetic 60 ms echo, deliberate clipping, immediate cold-start speech, and a 40 ms shift relative to the inference hop.
- **Labels:** target clips begin their speech at a known sample offset. Their acceptance interval extends through the generated utterance plus 500 ms. A first crossing outside that interval is an off-target activation, not a successful detection.

All clips are generated as mono PCM16 at 16 kHz. Synthesis uses eSpeak NG's `-D` deterministic random mode; variants with breath/noise components are not otherwise guaranteed repeatable. Ordinary clips have two seconds of leading silence and 1.5 seconds trailing silence; cold-start and offset cases override the leading interval. Noise is applied to the generated speech segment, with measured segment SNR recorded after quantization/clipping. The echo is a controlled delayed copy, not a measured room impulse response. The source WAV hashes define the exact evaluated audio; tool and generator fingerprints explain its provenance.

Holdout uses different eSpeak voice variants/speeds and different negative texts. Canonical target phrases necessarily repeat. Voice variants and audio perturbations are correlated synthetic examples, not independent human speakers.

## Replay and policy evaluation

The Cargo example imports the existing mel/embedding/classifier implementation directly. It hashes each WAV's loaded bytes against the manifest and parses that same in-memory buffer. It also snapshots the classifier for the run and records the snapshot's hash. It scores two-second windows at 80 ms hops. For every case it also feeds the production buffered `WakeWordRuntime` with 30 ms frames and requires its first detection at threshold 0.68 to match the score trace exactly. A cadence or detection mismatch fails the run.

The evaluator compares the suite's thresholds and one/two/three-consecutive-hit policies. A streak triggers at its **confirming** frame, not the first frame of the streak. Clip metrics use the first crossing, so an early false wake cannot be rescued by a later correct score. Stream events are clustered using the policy's streak, a below-threshold rearm and a two-second cooldown; a sustained high plateau counts once. This is a declared evaluation policy, not a simulation of how long an assistant conversation would keep detection disarmed.

Candidate selection uses calibration only and preserves every baseline-positive case while reducing negative-clip triggers, with no extra off-target first crossings or stream episodes. The selected candidate is then checked on holdout for paired-positive preservation, fewer negative-clip false positives, no extra off-target first crossings and non-worsening stream episodes. Holdout never selects a replacement candidate. **A successful benchmark command means measurement completed; it does not mean any candidate met the acceptance gate.** Check `candidate_accepted` and rejection reasons in the report.

## Artifacts and verification

| Artifact | Contents |
| --- | --- |
| `manifest.json` | Frozen case IDs, labels/sample counts, per-WAV SHA-256, suite/generator/tool provenance |
| `audio/` | Generated synthetic WAV files |
| `scores.jsonl` | Every score and sample position, input lengths, production first-detection parity and runner timings |
| `run-provenance.json` | Model, feature assets, runner binary/source, lockfile, corpus and trace fingerprints |
| `runner.bin` | Executable snapshot used for this run, verified independently of later builds |
| `report.json` | Machine-readable policy, split/scenario metrics, calibration selection and holdout gate |
| `report.md` | Human-readable policy comparison |

```bash
python3 tools/wakeword-bench/bench.py verify \
  --output target/wakeword-bench/validated-v1
python3 tools/wakeword-bench/bench.py report \
  --output target/wakeword-bench/validated-v1
```

`verify` checks corpus files and labels; `report` additionally checks trace completeness, score cadence, parity and provenance. Changed artifacts fail validation rather than being treated as a comparable result. For a full reproducibility check, independently generate/run into another directory and compare corpus hashes and per-case score traces. Timing values can differ across executions; score equality must be measured rather than assumed across hardware/runtime changes.

```bash
make wakeword-bench \
  WAKE_WORD_MODEL=/absolute/path/to/hey_livekit.onnx \
  WAKE_BENCH_DIR=target/wakeword-bench/verified-repeat-v1
python3 tools/wakeword-bench/compare_runs.py \
  --left target/wakeword-bench/validated-v1 \
  --right target/wakeword-bench/verified-repeat-v1
```

The comparison verifies both runs before requiring identical corpus bytes, scorer sources, models, scores, sample positions, buffered-detector decisions and policy results. Elapsed wall-clock measurements are excluded. Each executable snapshot is independently fingerprint-verified; native build metadata can make executable bytes differ between builds, so their hashes are reported separately rather than treated as a score-reproducibility failure.

The [validated v1 results](RESULTS.md) record the completed baseline and independent reproduction. One-off LiveKit parity, Sherpa and Kokoro investigations are retained as historical research under the outer project's local `docs/` directory, outside the source repository.

## Interpretation

Report positive detection and negative-clip trigger counts separately, including cold-start and perturbed subgroups. Onset latency is measured from the beginning of the synthesized utterance, **not** from the end of the wake phrase. The runner's elapsed time includes offline inference, model setup and parity checking, so it is not microphone-to-UI latency.

The negative streams deliberately repeat confusing phrases. Their reported events/hour describe only those synthetic streams; four total minutes cannot establish everyday false-wake rates. These tests bypass hardware capture, the app's device-rate resampler and UI transport. They provide a repeatable regression/tuning baseline, not proof of real-room or universal voice accuracy. Holdout becomes development data once used to guide further changes; version a new untouched holdout for later claims.
