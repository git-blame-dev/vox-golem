import importlib.util
import copy
import json
import hashlib
import os
import stat
import tempfile
import unittest
import shutil
from pathlib import Path
from unittest.mock import patch


MODULE_PATH = Path(__file__).with_name("bench.py")
SPEC = importlib.util.spec_from_file_location("wakeword_bench", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("cannot import bench module")
bench = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bench)


class EvaluationTests(unittest.TestCase):
    @unittest.skipUnless(shutil.which("espeak-ng") and shutil.which("ffmpeg"),
                         "requires installed espeak-ng and ffmpeg")
    def test_espeak_heldout_voice_pcm_is_deterministic(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            outputs = []
            for index in range(6):
                destination = root / str(index)
                destination.mkdir()
                outputs.append(bench.pcm_from_espeak("hey livekit open the settings", "en-us+f3", 150, destination))
            self.assertTrue(all(samples == outputs[0] for samples in outputs[1:]))

    def test_first_hit_respects_interval_and_streak(self):
        scores = [0.8, 0.8, 0.1, 0.8, 0.8]
        samples = [100, 200, 300, 400, 500]
        self.assertEqual(bench.first_crossing(scores, samples, 0.7, 2, (350, 500)), 500)
        self.assertEqual(bench.first_crossing(scores, samples, 0.7, 2, (100, 250)), 200)

    def test_stream_requires_streak_and_reports_confirmation_not_onset(self):
        samples = list(range(0, 20_000, 1_280))
        scores = [0.9, 0.1, 0.9, 0.1, 0.9, 0.9] + [0.9] * (len(samples) - 6)
        self.assertEqual(bench.stream_episodes(scores, samples, 0.68, 3, 2_000, 1_280), [7_680])

    def test_stream_rearm_and_cooldown_prevent_duplicate_plateau_events(self):
        samples = list(range(0, 12 * 1_280, 1_280))
        scores = [0.9, 0.9, 0.9, 0.1, 0.9, 0.9, 0.9, 0.1, 0.9, 0.9, 0.9, 0.1]
        self.assertEqual(bench.stream_episodes(scores, samples, 0.68, 3, 6_000, 1_280), [2_560, 12_800])

    def test_paired_positive_success_is_not_hidden_by_equal_total_recall(self):
        cases = [
            {"id": "baseline-only", "split": "calibration", "kind": "positive", "scenario": "clean",
             "expected": [{"start_sample": 1_000, "end_sample": 5_000}]},
            {"id": "candidate-only", "split": "calibration", "kind": "positive", "scenario": "clean",
             "expected": [{"start_sample": 1_000, "end_sample": 5_000}]},
        ]
        trace = {"baseline-only": {"input_samples": 5_000, "scores": [
                    {"sample_end": 1_000, "score": 0.8}, {"sample_end": 2_280, "score": 0.1}]},
                 "candidate-only": {"input_samples": 5_000, "scores": [
                    {"sample_end": 1_000, "score": 0.6}, {"sample_end": 2_280, "score": 0.6}]}}
        measured = bench.evaluate({"cases": cases}, trace, [0.68, 0.5], [1, 2], {"threshold": 0.68, "hits": 1}, 2_000)
        item = measured["0.5/2"]["splits"]["calibration"]
        self.assertEqual(item["positive_hits"], 1)
        self.assertEqual(item["baseline_positive_successes"], 1)
        self.assertEqual(item["baseline_paired_successes"], 0)

    def test_stale_or_malformed_trace_is_rejected(self):
        with self.assertRaises(bench.BenchError):
            bench.validate_trace([{"kind": "case", "id": "a", "input_samples": 10,
                                   "scores": [{"sample_end": 5, "score": float("nan")}]}], {"a": {"duration_samples": 10}})
        with self.assertRaises(bench.BenchError):
            bench.validate_trace([{"kind": "case", "id": "a", "input_samples": 10, "scores": []},
                                  {"kind": "case", "id": "a", "input_samples": 10, "scores": []}], {"a": {"duration_samples": 10}})

    def test_source_path_rejects_escape(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            with self.assertRaises(bench.BenchError):
                bench.safe_child(root, "../outside.wav")

    def test_verify_detects_missing_or_modified_wav(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            audio = root / "clip.wav"
            bench.write_wav(audio, [0, 1, -1])
            manifest = {"schema_version": 1, "sample_rate_hz": 16000,
                        "recipe": {"expected_case_ids": ["one-case"]},
                        "cases": [{"id": "one-case", "split": "calibration", "kind": "negative",
                                   "expected": [], "wav": "clip.wav", "sha256": bench.sha256(audio),
                                   "duration_samples": 3}]}
            (root / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
            bench.verify_manifest(root)
            manifest["recipe"]["expected_case_ids"].append("missing-case")
            (root / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
            with self.assertRaisesRegex(bench.BenchError, "do not match"):
                bench.verify_manifest(root)
            audio.write_bytes(b"changed")
            with self.assertRaises(bench.BenchError):
                bench.verify_manifest(root)

    def test_suite_labels_annotations_and_paths_are_authoritative(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / "corpus"
            root.mkdir()
            suite_path = Path(temp) / "suite.json"
            suite = {"schema_version": 1, "name": "tiny", "sample_rate_hz": 16000, "seed": 3,
                     "baseline": {"threshold": 0.68, "hits": 1}, "thresholds": [0.68],
                     "consecutive_hits": [1], "cooldown_ms": 2000, "stream_seconds": 1,
                     "scenarios": [{"id": "clean", "lead_ms": 0}],
                     "splits": {split: {"voices": [{"name": "synthetic", "speed": 160}],
                                        "positive": [{"id": "target", "text": "synthetic target"}],
                                        "negative": [{"id": "negative", "text": "synthetic negative"}]}
                                for split in ("calibration", "holdout")}}
            suite_path.write_text(json.dumps(suite), encoding="utf-8")
            specs = bench.expected_case_metadata(suite)
            cases = []
            for case_id, spec in specs.items():
                duration = (bench.RATE if spec["kind"] == "stream"
                            else spec["lead_samples"] + 100 + bench.TRAIL_MS * bench.RATE // 1000)
                relative = spec["wav"]
                audio_path = root / relative
                bench.write_wav(audio_path, [0] * duration)
                case = {**spec, "duration_samples": duration, "sha256": bench.sha256(audio_path)}
                if spec["kind"] != "stream":
                    case["speech_end_sample"] = duration - bench.TRAIL_MS * bench.RATE // 1000
                if spec["kind"] == "positive":
                    start = spec["lead_samples"]
                    case["expected"] = [{"start_sample": start,
                                         "end_sample": min(duration, start + 100 + bench.ALLOWANCE)}]
                else:
                    case["expected"] = []
                cases.append(case)
            manifest = {"schema_version": 1, "sample_rate_hz": 16000,
                        "recipe": {"expected_case_ids": sorted(specs)},
                        "provenance": {"suite_sha256": bench.sha256(suite_path)}, "cases": cases}
            manifest_path = root / "manifest.json"
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
            bench.verify_manifest(root, suite_path)

            positive_id = next(case["id"] for case in cases if case["kind"] == "positive")
            for corrupt in ("kind", "split", "expected"):
                changed = copy.deepcopy(manifest)
                case = next(case for case in changed["cases"] if case["id"] == positive_id)
                if corrupt == "kind":
                    case["kind"] = "negative"
                    case["expected"] = []
                elif corrupt == "split":
                    case["split"] = "holdout" if case["split"] == "calibration" else "calibration"
                else:
                    case["expected"][0]["start_sample"] += 1
                manifest_path.write_text(json.dumps(changed), encoding="utf-8")
                with self.assertRaisesRegex(bench.BenchError, "suite"):
                    bench.verify_manifest(root, suite_path)

    def test_noise_seed_is_stable(self):
        self.assertEqual(bench.noise_samples("case-1", 128, 10), bench.noise_samples("case-1", 128, 10))
        self.assertNotEqual(bench.noise_samples("case-1", 128, 10), bench.noise_samples("case-1", 128, 11))
        self.assertNotEqual(bench.noise_samples("case-1", 128, 10), bench.noise_samples("case-2", 128, 10))

    def test_case_ids_encode_voice_speed_and_kind(self):
        voice = {"name": "synthetic", "speed": 160}
        variants = {bench.case_id("calibration", 0, voice, kind, "phrase", "clean")
                    for kind in ("positive", "negative")}
        faster = bench.case_id("calibration", 0, {"name": "synthetic", "speed": 180},
                               "positive", "phrase", "clean")
        self.assertEqual(len(variants), 2)
        self.assertNotIn(faster, variants)

    def test_corpus_provenance_is_generation_only(self):
        with tempfile.TemporaryDirectory() as temp:
            suite_path = Path(temp) / "suite.json"
            suite_path.write_text('{"name":"synthetic","seed":99}', encoding="utf-8")
            with patch.object(bench, "tool_version", return_value="synthetic-tool 1"):
                provenance = bench.provenance(suite_path, {"name": "synthetic", "seed": 99})
        self.assertEqual(provenance["suite_seed"], 99)
        self.assertIn("generator_fingerprint", provenance)
        self.assertNotIn("production_source_hashes", provenance)

    def test_run_fingerprints_include_runner_helpers_and_both_feature_models(self):
        source_hashes = bench.fingerprints()["production_sources"]
        self.assertIn("apps/desktop-tauri/src-tauri/src/wake_diagnostics.rs", source_hashes)
        self.assertEqual(len(bench.fingerprints()["feature_model_hashes"]), 2)

    def test_seeded_pcm_and_wav_hash_repeat(self):
        pcm = bench.perturb([100, -200, 300, -400] * 64, {"gain": 1, "snr_db": 10}, "stable")
        with tempfile.TemporaryDirectory() as temp:
            first, second = Path(temp) / "a.wav", Path(temp) / "b.wav"
            bench.write_wav(first, pcm)
            bench.write_wav(second, bench.perturb([100, -200, 300, -400] * 64,
                                                    {"gain": 1, "snr_db": 10}, "stable"))
            self.assertEqual(bench.sha256(first), bench.sha256(second))

    def test_early_first_crossing_cannot_be_rescued_by_later_target_hit(self):
        case = {"id": "clip", "split": "calibration", "kind": "positive", "scenario": "clean",
                "expected": [{"start_sample": 2_560, "end_sample": 6_400}]}
        trace = {"clip": {"input_samples": 6_400, "scores": [
            {"sample_end": 1_280, "score": 0.9}, {"sample_end": 2_560, "score": 0.1},
            {"sample_end": 3_840, "score": 0.9}, {"sample_end": 5_120, "score": 0.9}]}}
        results = bench.evaluate({"cases": [case]}, trace, [0.68], [1], {"threshold": 0.68, "hits": 1}, 2_000)
        measured = results["0.68/1"]["splits"]["calibration"]
        self.assertEqual(measured["positive_hits"], 0)
        self.assertEqual(measured["offtarget_first_crossings"], 1)

    def test_selection_is_calibration_only_and_holdout_can_reject(self):
        good = {"baseline_positive_successes": 2, "baseline_paired_successes": 2,
                "negative_false_positive_clips": 4, "stream_false_episodes": 1,
                "offtarget_first_crossings": 1, "median_onset_latency_ms": 100}
        result = {"0.68/1": {"splits": {"calibration": good, "holdout": good}},
                  "0.9/2": {"splits": {
                      "calibration": {**good, "negative_false_positive_clips": 2, "median_onset_latency_ms": 120},
                      "holdout": {**good, "baseline_paired_successes": 1,
                                  "negative_false_positive_clips": 2, "stream_false_episodes": 2,
                                  "offtarget_first_crossings": 2}}},
                  "0.95/3": {"splits": {
                      "calibration": {**good, "baseline_paired_successes": 1,
                                      "negative_false_positive_clips": 1},
                      "holdout": {**good, "negative_false_positive_clips": 0,
                                  "stream_false_episodes": 0}}}}
        selected, accepted, checks = bench.select_candidate(result, "0.68/1")
        self.assertEqual(selected, "0.9/2")
        self.assertFalse(accepted)
        self.assertFalse(checks["holdout_paired_positive_preservation"])
        self.assertTrue(checks["holdout_negative_clip_improvement"])
        self.assertFalse(checks["holdout_no_extra_offtarget_first_crossings"])
        self.assertFalse(checks["holdout_stream_nonworsening"])

    def test_trace_requires_complete_grid_and_case_length(self):
        metadata = {"kind": "metadata", "schema_version": 1, "sample_rate_hz": 16000,
                    "model_sha256": "a" * 64,
                    "hop_samples": 1280, "window_samples": 32000,
                    "feature_profile": "normalized_f32", "baseline_threshold": 0.680000007}
        case = {"clip": {"duration_samples": 34_560}}
        incomplete = {"kind": "case", "id": "clip", "input_samples": 34_560,
                      "baseline_detection_sample": None, "elapsed_ms": 1.5,
                      "scores": [{"sample_end": 32_000, "score": 0.1}]}
        with self.assertRaises(bench.BenchError):
            bench.validate_trace([metadata, incomplete], case)
        incomplete["scores"] = [
            {"sample_end": 33_280, "score": 0.1}, {"sample_end": 34_560, "score": 0.1}]
        with self.assertRaises(bench.BenchError):
            bench.validate_trace([metadata, incomplete], case)
        incomplete["scores"] = []
        with self.assertRaises(bench.BenchError):
            bench.validate_trace([metadata, incomplete], case)
        incomplete["scores"] = [{"sample_end": 32_000, "score": 0.1}]
        incomplete["scores"].append({"sample_end": 33_280, "score": 0.1})
        incomplete["scores"].append({"sample_end": 34_560, "score": 0.1})
        bench.validate_trace([metadata, incomplete], case, expected_baseline=0.68)
        with self.assertRaises(bench.BenchError):
            bench.validate_trace([metadata, incomplete], {"clip": {"duration_samples": 35_000}})

    def test_cached_trace_hash_detects_tampering(self):
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp)
            trace = output / "scores.jsonl"
            trace.write_text("frozen trace\n", encoding="utf-8")
            expected = hashlib.sha256(trace.read_bytes()).hexdigest()
            self.assertEqual(bench.trace_hash(output), expected)
            trace.write_text("tampered trace\n", encoding="utf-8")
            with self.assertRaises(bench.BenchError):
                bench.require_trace_hash(output, expected)

    def test_report_rejects_tampered_cached_scores_before_using_them(self):
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp) / "run"
            output.mkdir()
            suite_path = Path(temp) / "suite.json"
            suite = {"schema_version": 1, "name": "tiny-synthetic", "sample_rate_hz": 16000,
                     "seed": 7, "baseline": {"threshold": 0.68, "hits": 1},
                     "thresholds": [0.68], "consecutive_hits": [1], "cooldown_ms": 2000,
                     "stream_seconds": 1, "scenarios": [{"id": "clean"}],
                     "splits": {split: {"voices": [{"name": "test", "speed": 160}],
                                        "positive": [], "negative": [{"id": "negative", "text": "synthetic"}]}
                                for split in ("calibration", "holdout")}}
            suite_path.write_text(json.dumps(suite), encoding="utf-8")
            case_specs = bench.expected_case_metadata(suite)
            case_ids = sorted(case_specs)
            cases = []
            for case_id in case_ids:
                spec = case_specs[case_id]
                if spec["kind"] == "stream":
                    duration = spec["stream_duration_samples"]
                else:
                    duration = spec["lead_samples"] + 3 + bench.TRAIL_MS * bench.RATE // 1000
                audio_path = output / spec["wav"]
                bench.write_wav(audio_path, [0] * duration)
                case = {**spec, "expected": [], "sha256": bench.sha256(audio_path),
                        "duration_samples": duration}
                if spec["kind"] != "stream":
                    case["speech_end_sample"] = duration - bench.TRAIL_MS * bench.RATE // 1000
                cases.append(case)
            manifest = {"schema_version": 1, "sample_rate_hz": 16000,
                        "recipe": {"expected_case_ids": case_ids},
                        "provenance": {"suite_sha256": bench.sha256(suite_path)}, "cases": cases}
            (output / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
            trace = output / "scores.jsonl"
            trace.write_text("tampered after run\n", encoding="utf-8")
            (output / "run-provenance.json").write_text(
                json.dumps({"trace_sha256": "0" * 64}), encoding="utf-8")
            with self.assertRaisesRegex(bench.BenchError, "changed since"):
                bench.report_run(output, suite_path)

    def test_cached_report_uses_its_runner_snapshot_not_mutable_global_binary(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            output = root / "run"
            output.mkdir()
            suite_path = root / "suite.json"
            suite = {"schema_version": 1, "name": "snapshot-test", "sample_rate_hz": 16000,
                     "seed": 9, "baseline": {"threshold": 0.68, "hits": 1},
                     "thresholds": [0.68], "consecutive_hits": [1], "cooldown_ms": 2000,
                     "stream_seconds": 1, "scenarios": [{"id": "clean"}],
                     "splits": {split: {"voices": [{"name": "test", "speed": 160}],
                                        "positive": [], "negative": [{"id": "negative", "text": "synthetic"}]}
                                for split in ("calibration", "holdout")}}
            suite_path.write_text(json.dumps(suite), encoding="utf-8")
            specs = bench.expected_case_metadata(suite)
            cases = []
            trace_lines = [{"kind": "metadata", "schema_version": 1, "sample_rate_hz": 16000,
                            "hop_samples": 1280, "window_samples": 32000, "baseline_threshold": 0.68,
                            "feature_profile": "normalized_f32", "model_sha256": None}]
            model = root / "synthetic-model.onnx"
            model.write_bytes(b"synthetic-model")
            trace_lines[0]["model_sha256"] = bench.sha256(model)
            for case_id, spec in specs.items():
                duration = (spec["stream_duration_samples"] if spec["kind"] == "stream"
                            else spec["lead_samples"] + 100 + bench.TRAIL_MS * bench.RATE // 1000)
                audio_path = output / spec["wav"]
                bench.write_wav(audio_path, [0] * duration)
                case = {**spec, "duration_samples": duration, "sha256": bench.sha256(audio_path), "expected": []}
                scores = [{"sample_end": end, "score": 0.1}
                          for end in range(bench.WINDOW, duration + 1, bench.HOP)]
                if spec["kind"] != "stream":
                    case["speech_end_sample"] = duration - bench.TRAIL_MS * bench.RATE // 1000
                cases.append(case)
                trace_lines.append({"kind": "case", "id": case_id, "input_samples": duration,
                                    "baseline_detection_sample": None, "elapsed_ms": 0.1, "scores": scores})
            manifest = {"schema_version": 1, "sample_rate_hz": 16000,
                        "recipe": {"expected_case_ids": sorted(specs)},
                        "provenance": {"suite_sha256": bench.sha256(suite_path)}, "cases": cases}
            (output / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
            trace_path = output / "scores.jsonl"
            trace_path.write_text("".join(json.dumps(line) + "\n" for line in trace_lines), encoding="utf-8")

            built_runner = root / "built-runner"
            built_runner.write_bytes(b"runner-snapshot-a")
            built_runner.chmod(stat.S_IRUSR | stat.S_IWUSR | stat.S_IXUSR)
            snapshot = bench.snapshot_runner(output, built_runner)
            self.assertEqual(bench.sha256(snapshot), bench.sha256(built_runner))
            self.assertTrue(os.access(snapshot, os.X_OK))
            with self.assertRaises(bench.BenchError):
                bench.snapshot_runner(output, built_runner)
            global_runner = root / "mutable-global-runner"
            global_runner.write_bytes(b"global-before")
            with patch.object(bench, "GLOBAL_RUNNER_BINARY", global_runner):
                run_fingerprints = bench.fingerprints(model, snapshot)
                run_provenance = {"fingerprints": run_fingerprints, "model_path": str(model),
                                  "corpus_sha256": bench.sha256(output / "manifest.json"),
                                  "suite_sha256": bench.sha256(suite_path),
                                  "runner_snapshot": bench.RUNNER_SNAPSHOT,
                                  "trace_sha256": bench.sha256(trace_path)}
                (output / "run-provenance.json").write_text(json.dumps(run_provenance), encoding="utf-8")
                global_runner.write_bytes(b"global-after-build")
                report = bench.report_run(output, suite_path)
                self.assertFalse(report["candidate_accepted"])
                marker = output / bench.RUN_INCOMPLETE
                marker.write_text("synthetic in-flight finalization", encoding="utf-8")
                with self.assertRaisesRegex(bench.BenchError, "incomplete"):
                    bench.report_run(output, suite_path)
                final_report = bench.report_run(output, suite_path, finalizing=True)
                self.assertEqual(report, final_report)
                marker.unlink()
                snapshot.write_bytes(b"runner-snapshot-mutated")
                with self.assertRaisesRegex(bench.BenchError, "provenance is stale"):
                    bench.report_run(output, suite_path)

    def test_trace_missing_and_out_of_bounds_points_rejected(self):
        meta = {"kind": "metadata", "schema_version": 1, "sample_rate_hz": 16000,
                "model_sha256": "a" * 64,
                "hop_samples": 1280, "window_samples": 32000,
                "feature_profile": "normalized_f32", "baseline_threshold": 0.68}
        with self.assertRaises(bench.BenchError):
            bench.validate_trace([meta], {"required": {"duration_samples": 1}})
        with self.assertRaises(bench.BenchError):
            bench.validate_trace([meta, {"kind": "case", "id": "a", "input_samples": 100,
                                         "scores": [{"sample_end": 101, "score": 0.2}]}], {"a": {"duration_samples": 100}})


if __name__ == "__main__":
    unittest.main()
