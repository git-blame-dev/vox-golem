#!/usr/bin/env python3
"""Deterministic, synthetic-only wake-word corpus and score-trace benchmark."""

import argparse
import hashlib
import inspect
import json
import math
import os
import random
import re
import shutil
import stat
import statistics
import struct
import subprocess
import sys
import wave
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_SUITE = Path(__file__).with_name("suite.json")
RATE = 16_000
LEAD_MS = 2_000
TRAIL_MS = 1_500
ALLOWANCE = 8_000
HOP = 1_280
WINDOW = 32_000
GLOBAL_RUNNER_BINARY = ROOT / "target/release/examples/wakeword_bench"
RUNNER_SNAPSHOT = "runner.bin"
RUN_INCOMPLETE = ".run-incomplete"
SOURCES = [
    "Cargo.toml",
    "apps/desktop-tauri/src-tauri/Cargo.toml",
    "apps/desktop-tauri/src-tauri/examples/wakeword_bench.rs",
    "apps/desktop-tauri/src-tauri/src/livekit_wakeword/mod.rs",
    "apps/desktop-tauri/src-tauri/src/livekit_wakeword/melspectrogram.rs",
    "apps/desktop-tauri/src-tauri/src/livekit_wakeword/embedding.rs",
    "apps/desktop-tauri/src-tauri/src/livekit_wakeword/wakeword.rs",
    "apps/desktop-tauri/src-tauri/src/livekit_wakeword/onnx/melspectrogram.onnx",
    "apps/desktop-tauri/src-tauri/src/livekit_wakeword/onnx/embedding_model.onnx",
    "apps/desktop-tauri/src-tauri/src/wake_word.rs",
    "apps/desktop-tauri/src-tauri/src/wake_diagnostics.rs",
    "Cargo.lock",
]


class BenchError(Exception):
    pass


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def canonical_hash(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def safe_child(root, relative):
    candidate = Path(relative)
    if candidate.is_absolute() or ".." in candidate.parts:
        raise BenchError(f"unsafe relative path: {relative}")
    result = (root / candidate).resolve()
    if not result.is_relative_to(root.resolve()):
        raise BenchError(f"path escapes output directory: {relative}")
    return result


def run_tool(args, stream=False):
    if args[0] == "cargo" or stream:
        try:
            process = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                       text=True, cwd=ROOT, bufsize=1)
            assert process.stdout is not None
            for line in process.stdout:
                print(line, end="", flush=True)
            status = process.wait()
        except OSError as error:
            raise BenchError(f"cannot run {' '.join(map(str, args))}: {error}") from error
        if status:
            raise BenchError(f"command failed with exit status {status}: {' '.join(map(str, args))}")
        return ""
    try:
        result = subprocess.run(args, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, cwd=ROOT)
    except (OSError, subprocess.CalledProcessError) as error:
        detail = getattr(error, "stderr", "")
        raise BenchError(f"command failed ({' '.join(map(str, args))}): {detail or error}") from error
    return result.stdout.strip()


def tool_version(binary):
    return run_tool([binary, "-version" if binary == "ffmpeg" else "--version"]).splitlines()[0]


def pcm_from_espeak(text, voice, speed, tmp):
    raw = tmp / "speech.wav"
    run_tool(["espeak-ng", "-D", "-v", voice, "-s", str(speed), "-w", str(raw), text])
    converted = tmp / "speech-16k.wav"
    run_tool(["ffmpeg", "-v", "error", "-y", "-i", str(raw), "-ac", "1", "-ar", str(RATE), "-sample_fmt", "s16", str(converted)])
    with wave.open(str(converted), "rb") as audio:
        if audio.getnchannels() != 1 or audio.getframerate() != RATE or audio.getsampwidth() != 2:
            raise BenchError("ffmpeg did not produce mono 16 kHz PCM16")
        return list(struct.unpack("<" + "h" * audio.getnframes(), audio.readframes(audio.getnframes())))


def noise_samples(case_id, count, suite_seed=0):
    seed_material = f"{suite_seed}:{case_id}".encode()
    seed = int.from_bytes(hashlib.sha256(seed_material).digest()[:8], "big")
    generator = random.Random(seed)
    return [generator.randint(-32768, 32767) for _ in range(count)]


def rounded_sample(value):
    return max(-32768, min(32767, int(math.floor(value + 0.5) if value >= 0 else math.ceil(value - 0.5))))


def active_rms(samples):
    if not samples:
        return 0.0
    return math.sqrt(sum(sample * sample for sample in samples) / len(samples))


def perturb(source, scenario, case_id, suite_seed=0):
    gain = float(scenario.get("gain", 1))
    speech = [rounded_sample(sample * gain) for sample in source]
    if scenario.get("echo_ms"):
        delay = round(float(scenario["echo_ms"]) * RATE / 1000)
        echo_gain = float(scenario.get("echo_gain", 0))
        speech = [rounded_sample(value + (source[index - delay] * gain * echo_gain if index >= delay else 0))
                  for index, value in enumerate(speech)]
    if scenario.get("snr_db") is not None:
        noise = noise_samples(case_id, len(speech), suite_seed)
        rms = active_rms(source)
        noise_rms = active_rms(noise)
        scale = rms / (10 ** (float(scenario["snr_db"]) / 20) * noise_rms) if noise_rms else 0
        speech = [rounded_sample(value + n * scale) for value, n in zip(speech, noise)]
    return speech


def write_wav(path, samples):
    path.parent.mkdir(parents=True, exist_ok=True)
    with wave.open(str(path), "wb") as audio:
        audio.setnchannels(1)
        audio.setsampwidth(2)
        audio.setframerate(RATE)
        audio.writeframes(struct.pack("<" + "h" * len(samples), *samples))


def suite_load(path):
    try:
        suite = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise BenchError(f"cannot read suite {path}: {error}") from error
    if suite.get("schema_version") != 1 or suite.get("sample_rate_hz") != RATE:
        raise BenchError("unsupported suite schema or sample rate")
    for split in ("calibration", "holdout"):
        if split not in suite.get("splits", {}) or not suite["splits"][split].get("voices"):
            raise BenchError(f"suite missing {split} voice/split definition")
    if not isinstance(suite.get("seed"), int) or isinstance(suite["seed"], bool):
        raise BenchError("suite seed must be an integer")
    ids = expected_case_ids(suite)
    if len(ids) != len(set(ids)):
        raise BenchError("suite definitions produce colliding corpus case IDs")
    return suite


def generation_fingerprint():
    generation_functions = (safe_child, suite_load, pcm_from_espeak, noise_samples,
                            rounded_sample, active_rms, perturb, write_wav, case_id,
                            expected_case_ids, expected_case_metadata, make_cases)
    source = "\n".join(inspect.getsource(function) for function in generation_functions)
    constants = {"sample_rate_hz": RATE, "lead_ms": LEAD_MS, "trail_ms": TRAIL_MS,
                 "wake_allowance_samples": ALLOWANCE}
    return canonical_hash({"functions": source, "constants": constants})


def case_id(split, voice_index, voice, kind, item_id, scenario):
    token = lambda value: re.sub(r"[^A-Za-z0-9-]+", "-", str(value)).strip("-") or "unnamed"
    return (f"{token(split)}-voice{voice_index}-{token(voice['name'])}-speed{int(voice['speed'])}-"
            f"{token(kind)}-{token(item_id)}-{token(scenario)}")


def expected_case_ids(suite):
    return sorted(expected_case_metadata(suite))


def expected_case_metadata(suite):
    expected = {}
    for split in ("calibration", "holdout"):
        definition = suite["splits"][split]
        for voice_index, voice in enumerate(definition["voices"]):
            for kind, items in (("positive", definition["positive"]), ("negative", definition["negative"])):
                for item in items:
                    for scenario in suite["scenarios"]:
                        identifier = case_id(split, voice_index, voice, kind, item["id"], scenario["id"])
                        if identifier in expected:
                            raise BenchError(f"suite definitions produce colliding corpus case ID: {identifier}")
                        lead_samples = int(scenario.get("lead_ms", LEAD_MS)) * RATE // 1000
                        expected[identifier] = {
                            "id": identifier, "split": split, "kind": kind, "scenario": scenario["id"],
                            "wav": f"audio/{split}/{identifier}.wav", "group": f"{split}-{item['id']}",
                            "voice": voice["name"], "voice_index": voice_index,
                            "speed": voice["speed"], "text_id": item["id"],
                            "lead_samples": lead_samples, "speech_start_sample": lead_samples,
                            "requested_snr_db": scenario.get("snr_db"),
                        }
        stream_id = f"{split}-negative-stream"
        expected[stream_id] = {
            "id": stream_id, "split": split, "kind": "stream", "scenario": "negative_stream",
            "wav": f"audio/{split}/{stream_id}.wav", "group": stream_id,
            "stream_duration_samples": int(suite["stream_seconds"] * RATE),
        }
    return expected


def provenance(suite_path, suite):
    return {"suite_sha256": sha256(suite_path), "suite_name": suite["name"],
            "generator_fingerprint": generation_fingerprint(), "generator": "wakeword-bench/2",
             "suite_seed": suite["seed"],
             "python": sys.version.splitlines()[0],
            "sample_rate_hz": RATE, "leading_default_ms": LEAD_MS, "trailing_default_ms": TRAIL_MS,
            "espeak_ng": tool_version("espeak-ng"), "ffmpeg": tool_version("ffmpeg")}


def make_cases(output, suite, prov):
    cases = []
    expected_metadata = expected_case_metadata(suite)
    (output / "source-tmp").mkdir(parents=True, exist_ok=True)
    for split in ("calibration", "holdout"):
        definition = suite["splits"][split]
        texts = [("positive", item) for item in definition["positive"]] + [("negative", item) for item in definition["negative"]]
        rendered = []
        for voice_index, voice in enumerate(definition["voices"]):
            for kind, item in texts:
                samples = pcm_from_espeak(item["text"], voice["name"], voice["speed"], output / "source-tmp")
                rendered.append((voice_index, voice, kind, item, samples))
        for voice_index, voice, kind, item, source in rendered:
            for scenario in suite["scenarios"]:
                lead_ms = int(scenario.get("lead_ms", LEAD_MS))
                lead_samples = lead_ms * RATE // 1000
                case = case_id(split, voice_index, voice, kind, item["id"], scenario["id"])
                body = perturb(source, scenario, case, suite["seed"])
                noise_snr = None
                if scenario.get("snr_db") is not None:
                    clean_scenario = {key: value for key, value in scenario.items() if key != "snr_db"}
                    clean_body = perturb(source, clean_scenario, case, suite["seed"])
                    noise_rms = active_rms([mixed - clean for mixed, clean in zip(body, clean_body)])
                    speech_rms = active_rms(clean_body)
                    noise_snr = 20 * math.log10(speech_rms / noise_rms) if noise_rms and speech_rms else None
                silence_tail = TRAIL_MS * RATE // 1000
                full = [0] * lead_samples + body + [0] * silence_tail
                rel = f"audio/{split}/{case}.wav"
                path = safe_child(output, rel)
                write_wav(path, full)
                start = lead_samples
                end = min(len(full), start + len(source) + ALLOWANCE)
                cases.append({**expected_metadata[case], "sha256": sha256(path),
                              "duration_samples": len(full),
                              "expected": ([{"start_sample": start, "end_sample": end}] if kind == "positive" else []),
                              "speech_end_sample": start + len(source), "measured_snr_db": noise_snr})
        # A clean two-minute negative stream made only from this split's synthesized negative utterances.
        stream_pieces = [samples for _, _, kind, _, samples in rendered if kind == "negative"]
        gap = [0] * (RATE // 4)
        target = int(suite["stream_seconds"] * RATE)
        stream = []
        index = 0
        while len(stream) < target:
            stream.extend(stream_pieces[index % len(stream_pieces)])
            stream.extend(gap)
            index += 1
        stream = stream[:target]
        stream_id = f"{split}-negative-stream"
        rel = f"audio/{split}/{stream_id}.wav"
        write_wav(safe_child(output, rel), stream)
        path = safe_child(output, rel)
        cases.append({**expected_metadata[stream_id], "sha256": sha256(path),
                      "duration_samples": len(stream), "expected": []})
    return cases


def verify_manifest(output, suite=None):
    manifest_path = output / "manifest.json"
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise BenchError(f"manifest unavailable or malformed: {error}") from error
    if manifest.get("schema_version") != 1 or manifest.get("sample_rate_hz") != RATE:
        raise BenchError("unsupported manifest schema")
    cases = manifest.get("cases")
    recipe = manifest.get("recipe")
    if (not isinstance(cases, list) or not isinstance(recipe, dict) or
            not isinstance(recipe.get("expected_case_ids"), list) or
            any(not isinstance(case_id, str) for case_id in recipe["expected_case_ids"])):
        raise BenchError("manifest has no valid corpus recipe")
    expected_ids = set(recipe["expected_case_ids"])
    if len(expected_ids) != len(recipe["expected_case_ids"]):
        raise BenchError("recipe has duplicate expected case IDs")
    suite_definition = None
    expected_metadata = None
    if suite is not None:
        if manifest.get("provenance", {}).get("suite_sha256") != sha256(suite):
            raise BenchError("frozen corpus suite hash differs; choose a new output directory")
        suite_definition = suite_load(suite)
        expected_metadata = expected_case_metadata(suite_definition)
    if suite is not None:
        expected_from_suite = expected_case_ids(suite_definition)
        if set(expected_from_suite) != expected_ids:
            raise BenchError("manifest recipe does not match the supplied suite")
    seen = set()
    for case in cases:
        if (not isinstance(case, dict) or not isinstance(case.get("id"), str) or
                case.get("split") not in ("calibration", "holdout") or
                case.get("kind") not in ("positive", "negative", "stream") or
                not isinstance(case.get("expected"), list) or
                not isinstance(case.get("duration_samples"), int) or case["duration_samples"] < 0 or
                not isinstance(case.get("sha256"), str) or len(case["sha256"]) != 64):
            raise BenchError("malformed manifest case")
        if (case["kind"] == "positive") != (len(case["expected"]) == 1) or (
                case["kind"] != "positive" and case["expected"]):
            raise BenchError(f"ground-truth interval does not match case kind: {case['id']}")
        if case.get("id") in seen:
            raise BenchError(f"duplicate manifest case id: {case.get('id')}")
        seen.add(case.get("id"))
        expected_case = expected_metadata.get(case["id"]) if expected_metadata is not None else None
        if expected_metadata is not None:
            if expected_case is None or any(case.get(key) != value for key, value in expected_case.items()):
                raise BenchError(f"case metadata does not match supplied suite: {case['id']}")
            if case["kind"] == "stream":
                if (case["duration_samples"] != expected_case["stream_duration_samples"] or
                        case["expected"] != []):
                    raise BenchError(f"stream duration or labels do not match supplied suite: {case['id']}")
            else:
                lead = expected_case["lead_samples"]
                trailing = TRAIL_MS * RATE // 1000
                if case["duration_samples"] < lead + trailing + 1:
                    raise BenchError(f"clip is too short for annotated speech and padding: {case['id']}")
                speech_end = case["duration_samples"] - trailing
                if case.get("speech_end_sample") != speech_end:
                    raise BenchError(f"speech sample annotation does not match WAV duration: {case['id']}")
                intervals = ([{"start_sample": lead,
                               "end_sample": min(case["duration_samples"], speech_end + ALLOWANCE)}]
                             if case["kind"] == "positive" else [])
                if case["expected"] != intervals:
                    raise BenchError(f"positive target interval does not match suite/WAV bounds: {case['id']}")
        for interval in case["expected"]:
            if (not isinstance(interval, dict) or not isinstance(interval.get("start_sample"), int) or
                    not isinstance(interval.get("end_sample"), int) or interval["start_sample"] < 0 or
                    interval["end_sample"] < interval["start_sample"] or
                    interval["end_sample"] > case.get("duration_samples", -1)):
                raise BenchError(f"invalid expected sample interval: {case['id']}")
        path = safe_child(output, case.get("wav", ""))
        if not path.is_file() or sha256(path) != case.get("sha256"):
            raise BenchError(f"missing or changed corpus WAV: {case.get('wav')}")
        with wave.open(str(path), "rb") as audio:
            if audio.getframerate() != RATE or audio.getnchannels() != 1 or audio.getsampwidth() != 2 or audio.getnframes() != case.get("duration_samples"):
                raise BenchError(f"WAV format/count mismatch: {case.get('wav')}")
    if seen != expected_ids:
        raise BenchError("manifest case IDs do not match its frozen recipe")
    return manifest


def prepare(output, suite_path):
    suite = suite_load(suite_path)
    if (output / "manifest.json").exists():
        manifest = verify_manifest(output, suite_path)
        if manifest.get("provenance") != provenance(suite_path, suite):
            raise BenchError("frozen corpus generator/tool provenance differs; choose a new output directory")
        print(f"Verified existing immutable corpus ({len(manifest['cases'])} cases): {output}")
        return manifest
    if output.exists() and any(output.iterdir()):
        raise BenchError(f"output exists without a corpus manifest; refusing to overwrite: {output}")
    output.mkdir(parents=True, exist_ok=True)
    prov = provenance(suite_path, suite)
    cases = make_cases(output, suite, prov)
    shutil.rmtree(output / "source-tmp", ignore_errors=True)
    manifest = {"schema_version": 1, "sample_rate_hz": RATE,
                "evaluation_settings": {key: suite[key] for key in
                    ("baseline", "thresholds", "consecutive_hits", "cooldown_ms", "stream_seconds")},
                "recipe": {"expected_case_ids": expected_case_ids(suite)},
                "provenance": prov, "cases": cases}
    (output / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return verify_manifest(output, suite_path)


def float32(value):
    return struct.unpack("<f", struct.pack("<f", float(value)))[0]


def validate_trace(lines, expected_cases, expected_baseline=None):
    if not lines or not isinstance(lines[0], dict) or lines[0].get("kind") != "metadata":
        raise BenchError("trace missing first metadata record")
    metadata = lines[0]
    if not isinstance(metadata.get("model_sha256"), str) or not re.fullmatch(r"[0-9a-f]{64}", metadata["model_sha256"]):
        raise BenchError("runner metadata is missing its loaded model fingerprint")
    if (metadata.get("schema_version") != 1 or metadata.get("sample_rate_hz") != RATE or
            metadata.get("hop_samples") != HOP or metadata.get("window_samples") != WINDOW or
            metadata.get("feature_profile") != "normalized_f32"):
        raise BenchError("runner metadata contract mismatch")
    for key in ("baseline_threshold",):
        if (not isinstance(metadata.get(key), (int, float)) or isinstance(metadata[key], bool) or
                not math.isfinite(metadata[key]) or not 0 <= metadata[key] <= 1):
            raise BenchError(f"invalid runner metadata: {key}")
    if expected_baseline is not None and not math.isclose(
            metadata["baseline_threshold"], float32(expected_baseline), rel_tol=0, abs_tol=1e-7):
        raise BenchError("runner baseline threshold differs from frozen suite")
    expected_ids = set(expected_cases)
    by_id = {}
    for record in lines[1:]:
        if not isinstance(record, dict) or record.get("kind") != "case" or not isinstance(record.get("id"), str) or record.get("id") in by_id:
            raise BenchError("trace has malformed or duplicate case records")
        case_id = record["id"]
        if case_id not in expected_ids:
            raise BenchError(f"unexpected trace case: {case_id}")
        if not isinstance(record.get("input_samples"), int) or isinstance(record["input_samples"], bool) or record["input_samples"] < 0:
            raise BenchError(f"invalid input length in trace: {case_id}")
        expected_length = expected_cases[case_id]["duration_samples"]
        if record["input_samples"] != expected_length:
            raise BenchError(f"trace input length differs from manifest: {case_id}")
        elapsed = record.get("elapsed_ms")
        if (not isinstance(elapsed, (int, float)) or isinstance(elapsed, bool) or
                not math.isfinite(elapsed) or elapsed < 0 or "baseline_detection_sample" not in record):
            raise BenchError(f"trace case metadata is incomplete: {case_id}")
        samples = []
        if not isinstance(record.get("scores"), list):
            raise BenchError(f"missing score array in trace: {case_id}")
        for score in record["scores"]:
            if not isinstance(score, dict):
                raise BenchError(f"malformed score point in trace: {case_id}")
            end, value = score.get("sample_end"), score.get("score")
            if (not isinstance(end, int) or isinstance(end, bool) or end < 0 or end > record["input_samples"] or
                    not isinstance(value, (int, float)) or isinstance(value, bool) or not math.isfinite(value) or not 0 <= value <= 1):
                raise BenchError(f"invalid score point in trace: {case_id}")
            if samples and end - samples[-1] != HOP:
                raise BenchError(f"non-80ms score cadence in trace: {case_id}")
            samples.append(end)
        expected_grid = list(range(WINDOW, record["input_samples"] + 1, HOP))
        if samples != expected_grid:
            raise BenchError(f"incomplete or misaligned score grid in trace: {case_id}")
        baseline = record.get("baseline_detection_sample")
        if baseline is not None and (not isinstance(baseline, int) or isinstance(baseline, bool) or baseline not in samples):
            raise BenchError(f"invalid baseline parity point in trace: {case_id}")
        expected_baseline = first_crossing([point["score"] for point in record["scores"]], samples,
                                           float(metadata["baseline_threshold"]), 1)
        if baseline != expected_baseline:
            raise BenchError(f"baseline buffered-runtime parity mismatch in trace: {case_id}")
        by_id[case_id] = record
    if set(by_id) != expected_ids:
        raise BenchError(f"trace case set mismatch: missing {sorted(expected_ids - set(by_id))[:3]}")
    return metadata, by_id


def first_crossing(scores, samples, threshold, streak, interval=None):
    consecutive = 0
    for index, (score, sample) in enumerate(zip(scores, samples)):
        if interval and not interval[0] <= sample <= interval[1]:
            consecutive = 0
            continue
        consecutive = consecutive + 1 if score >= threshold else 0
        if consecutive >= streak:
            return sample
    return None


def stream_episodes(scores, samples, threshold, streak, cooldown_samples, hop_samples):
    if len(scores) != len(samples):
        raise BenchError("stream score/sample count mismatch")
    events = []
    armed = True
    high_streak = 0
    last_event = None
    previous = None
    for score, sample in zip(scores, samples):
        if previous is not None and sample - previous != hop_samples:
            raise BenchError("stream timestamps do not follow the score hop")
        previous = sample
        if score < threshold:
            armed = True
            high_streak = 0
        else:
            high_streak += 1
            if armed and high_streak >= streak:
                if last_event is None or sample - last_event >= cooldown_samples:
                    events.append(sample)
                    last_event = sample
                armed = False
    return events


def evaluate(manifest, traces, thresholds, streaks, baseline, cooldown_ms):
    policies = [(float32(t), int(s)) for t in thresholds for s in streaks]
    results = {}
    cooldown_samples = cooldown_ms * RATE // 1000
    for policy in policies:
        key = f"{policy[0]:g}/{policy[1]}"
        rows = {"policy": {"threshold": policy[0], "consecutive_hits": policy[1]}, "splits": {}}
        for split in ("calibration", "holdout"):
            splitcases = [case for case in manifest["cases"] if case["split"] == split]
            positive = negative = hits = false_clips = 0
            paired_success = 0
            baseline_successes = 0
            latencies = []
            offtarget = 0
            scenarios = {}
            stream_events = stream_samples = 0
            for case in splitcases:
                record = traces[case["id"]]
                points = record["scores"]
                scores = [float(p["score"]) for p in points]
                ends = [int(p["sample_end"]) for p in points]
                case_result = scenarios.setdefault(case["scenario"], {
                    "positive_cases": 0, "positive_hits": 0,
                    "baseline_positive_successes": 0, "baseline_paired_successes": 0,
                    "offtarget_first_crossings": 0, "onset_latencies_ms": [],
                    "negative_clips": 0, "negative_false_positive_clips": 0,
                    "stream_false_episodes": 0, "stream_seconds": 0})
                if case["kind"] == "positive":
                    positive += 1; case_result["positive_cases"] += 1
                    start, end = case["expected"][0]["start_sample"], case["expected"][0]["end_sample"]
                    first = first_crossing(scores, ends, policy[0], policy[1])
                    baseline_first = first_crossing(scores, ends, float32(baseline["threshold"]), int(baseline["hits"]))
                    hit = first is not None and start <= first <= end
                    baseline_hit = baseline_first is not None and start <= baseline_first <= end
                    offtarget += int(first is not None and not hit)
                    baseline_successes += int(baseline_hit)
                    paired_success += int(baseline_hit and hit)
                    case_result["offtarget_first_crossings"] += int(first is not None and not hit)
                    case_result["baseline_positive_successes"] += int(baseline_hit)
                    case_result["baseline_paired_successes"] += int(baseline_hit and hit)
                    if hit:
                        latency = (first - start) / RATE * 1000
                        hits += 1; case_result["positive_hits"] += 1
                        latencies.append(latency)
                        case_result["onset_latencies_ms"].append(latency)
                elif case["kind"] == "negative":
                    negative += 1; case_result["negative_clips"] += 1
                    if first_crossing(scores, ends, policy[0], policy[1]) is not None:
                        false_clips += 1; case_result["negative_false_positive_clips"] += 1
                else:
                    stream_samples += record["input_samples"]
                    events = len(stream_episodes(scores, ends, policy[0], policy[1], cooldown_samples, HOP))
                    seconds_in_stream = record["input_samples"] / RATE
                    stream_events += events
                    case_result["stream_false_episodes"] += events
                    case_result["stream_seconds"] += seconds_in_stream
            seconds = stream_samples / RATE
            for scenario in scenarios.values():
                scenario["positive_recall"] = (scenario["positive_hits"] / scenario["positive_cases"]
                                                if scenario["positive_cases"] else None)
                scenario["negative_false_positive_fraction"] = (
                    scenario["negative_false_positive_clips"] / scenario["negative_clips"]
                    if scenario["negative_clips"] else None)
                scenario["median_onset_latency_ms"] = (statistics.median(scenario.pop("onset_latencies_ms"))
                                                         if scenario["positive_hits"] else None)
            rows["splits"][split] = {"positive_hits": hits, "positive_cases": positive,
                "positive_misses": positive - hits,
                "recall": hits / positive if positive else None, "baseline_paired_successes": paired_success,
                "baseline_positive_successes": baseline_successes,
                "negative_false_positive_clips": false_clips, "negative_clips": negative,
                "false_positive_clip_fraction": false_clips / negative if negative else None,
                "offtarget_first_crossings": offtarget,
                "median_onset_latency_ms": statistics.median(latencies) if latencies else None,
                "stream_false_episodes": stream_events, "stream_seconds": seconds,
                "synthetic_stream_events_per_hour": stream_events * 3600 / seconds if seconds else None,
                "scenarios": scenarios}
        results[key] = rows
    return results


def fingerprints(model=None, runner_binary=None):
    missing = [path for path in SOURCES if not (ROOT / path).is_file()]
    if missing:
        raise BenchError(f"cannot fingerprint missing runner/source inputs: {', '.join(missing)}")
    files = {path: sha256(ROOT / path) for path in SOURCES}
    binary = Path(runner_binary) if runner_binary is not None else GLOBAL_RUNNER_BINARY
    if runner_binary is not None and not binary.is_file():
        raise BenchError(f"runner snapshot is missing: {binary}")
    return {"benchmark_sha256": sha256(Path(__file__)), "production_sources": files,
            "feature_model_hashes": {path: files[path] for path in (
                "apps/desktop-tauri/src-tauri/src/livekit_wakeword/onnx/melspectrogram.onnx",
                "apps/desktop-tauri/src-tauri/src/livekit_wakeword/onnx/embedding_model.onnx")},
            "runner_binary_sha256": sha256(binary) if binary.is_file() else None,
            "model_sha256": sha256(model) if model else None}


def snapshot_runner(output, source=None):
    source = GLOBAL_RUNNER_BINARY if source is None else Path(source)
    destination = output / RUNNER_SNAPSHOT
    if not source.is_file():
        raise BenchError(f"built runner binary is missing: {source}")
    try:
        with source.open("rb") as reader, destination.open("xb") as writer:
            shutil.copyfileobj(reader, writer)
            writer.flush()
            os.fsync(writer.fileno())
        mode = stat.S_IMODE(source.stat().st_mode) | 0o111
        destination.chmod(mode)
    except OSError as error:
        raise BenchError(f"cannot create immutable runner snapshot: {error}") from error
    if sha256(source) != sha256(destination) or not os.access(destination, os.X_OK):
        raise BenchError("runner snapshot failed copy hash or executable-mode verification")
    return destination


def source_identity(fingerprints):
    return {key: fingerprints[key] for key in
            ("benchmark_sha256", "production_sources", "feature_model_hashes", "model_sha256")}


def require_unchanged_sources(before, after):
    if source_identity(before) != source_identity(after):
        raise BenchError("model, benchmark, or production source fingerprints changed during runner build/scoring")


def trace_hash(output):
    path = output / "scores.jsonl"
    if not path.is_file():
        raise BenchError("scores.jsonl missing")
    return sha256(path)


def require_trace_hash(output, expected):
    actual = trace_hash(output)
    if actual != expected:
        raise BenchError("scores.jsonl changed since the recorded runner execution")
    return actual


def select_candidate(results, baseline_key):
    baseline = results[baseline_key]["splits"]["calibration"]
    baseline_latency = baseline["median_onset_latency_ms"] or 0
    candidates = []
    for key, result in results.items():
        candidate = result["splits"]["calibration"]
        if (key != baseline_key and
                candidate["baseline_paired_successes"] == baseline["baseline_positive_successes"] and
                candidate["negative_false_positive_clips"] < baseline["negative_false_positive_clips"] and
                candidate["offtarget_first_crossings"] <= baseline["offtarget_first_crossings"] and
                candidate["stream_false_episodes"] <= baseline["stream_false_episodes"]):
            added_latency = (candidate["median_onset_latency_ms"] or 0) - baseline_latency
            candidates.append((candidate["negative_false_positive_clips"],
                               candidate["stream_false_episodes"], added_latency, key))
    selected = min(candidates)[-1] if candidates else None
    baseline_holdout = results[baseline_key]["splits"]["holdout"]
    if selected is None:
        return None, False, {"selected_candidate": False}
    candidate_holdout = results[selected]["splits"]["holdout"]
    checks = {
        "selected_candidate": True,
        "holdout_paired_positive_preservation": candidate_holdout["baseline_paired_successes"] == baseline_holdout["baseline_positive_successes"],
        "holdout_negative_clip_improvement": candidate_holdout["negative_false_positive_clips"] < baseline_holdout["negative_false_positive_clips"],
        "holdout_no_extra_offtarget_first_crossings": candidate_holdout["offtarget_first_crossings"] <= baseline_holdout["offtarget_first_crossings"],
        "holdout_stream_nonworsening": candidate_holdout["stream_false_episodes"] <= baseline_holdout["stream_false_episodes"],
    }
    return selected, all(checks.values()), checks


def rejection_reason(selected, checks):
    if selected is None:
        return "no calibration policy preserved all baseline-positive cases while reducing negative clip false positives"
    failed = [key for key, passed in checks.items() if not passed]
    return "none" if not failed else "holdout failed: " + ", ".join(failed)


def report_run(output, suite_path, model=None, *, finalizing=False):
    if (output / RUN_INCOMPLETE).exists() and not finalizing:
        raise BenchError("run directory contains an incomplete runner operation; use a fresh output directory")
    suite = suite_load(suite_path)
    manifest = verify_manifest(output, suite_path)
    run_file = output / "run-provenance.json"
    if not run_file.is_file():
        raise BenchError("score trace has no run provenance")
    try:
        stored = json.loads(run_file.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise BenchError(f"invalid run provenance: {error}") from error
    require_trace_hash(output, stored.get("trace_sha256"))
    trace_path = output / "scores.jsonl"
    if not trace_path.is_file():
        raise BenchError("scores.jsonl missing; run the runner first")
    try:
        lines = [json.loads(line) for line in trace_path.read_text(encoding="utf-8").splitlines()]
    except (json.JSONDecodeError, OSError) as error:
        raise BenchError(f"invalid scores.jsonl: {error}") from error
    case_map = {case["id"]: case for case in manifest["cases"]}
    metadata, traces = validate_trace(lines, case_map, suite["baseline"]["threshold"])
    model = Path(stored.get("model_path", "")) if model is None else Path(model)
    if not model.is_file():
        raise BenchError("recorded model path is unavailable; cannot verify cached score provenance")
    if stored.get("runner_snapshot") != RUNNER_SNAPSHOT:
        raise BenchError("run provenance does not identify its immutable runner snapshot")
    runner_snapshot = safe_child(output, stored["runner_snapshot"])
    if not runner_snapshot.is_file() or not os.access(runner_snapshot, os.X_OK):
        raise BenchError("recorded runner snapshot is missing or not executable")
    fp = fingerprints(model, runner_snapshot)
    if metadata["model_sha256"] != fp["model_sha256"]:
        raise BenchError("runner loaded a different model than the recorded input fingerprint")
    if (stored.get("fingerprints") != fp or
            stored.get("corpus_sha256") != sha256(output / "manifest.json") or
            stored.get("suite_sha256") != sha256(suite_path)):
        raise BenchError("score trace provenance is stale or mismatched; use a fresh output directory")
    results = evaluate(manifest, traces, suite["thresholds"], suite["consecutive_hits"], suite["baseline"], suite["cooldown_ms"])
    baseline_key = f"{float32(suite['baseline']['threshold']):g}/{int(suite['baseline']['hits'])}"
    selected, accepted, holdout_checks = select_candidate(results, baseline_key)
    report = {"schema_version": 1, "measurement_complete": True, "candidate_accepted": accepted,
              "selection": selected, "selection_rule": "calibration only: preserve each baseline positive and reduce negative clip triggers; rank by negative clips, stream episodes, then added latency",
              "holdout_gate": holdout_checks,
              "candidate_rejection_reason": rejection_reason(selected, holdout_checks),
              "candidate_gate": "synthetic corpus evidence only; no claim of live performance", "runner_metadata": metadata,
              "fingerprints": fp, "corpus_sha256": sha256(output / "manifest.json"), "policies": results}
    (output / "report.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    lines_md = ["# Synthetic wake-word benchmark", "", f"Measurement complete: **yes**; candidate accepted: **{str(report['candidate_accepted']).lower()}**.",
                f"Calibration selection: **{selected or 'none (no qualifying improvement)'}**.",
                f"Holdout rejection reason: **{report['candidate_rejection_reason']}**.", "", "These eSpeak/FFmpeg synthetic speech clips and concatenated streams are artificial adversarial measurements, not representative room recordings or everyday false-wake rates.", "", "## Policy results", "", "| Policy | Split | Positive recall | Negative clip false positives | False-positive clip fraction | Stream episodes/hour (synthetic only) | Median onset latency ms |", "|---|---|---:|---:|---:|---:|---:|"]
    for key, value in results.items():
        for split in ("calibration", "holdout"):
            item = value["splits"][split]
            lines_md.append(f"| {key} | {split} | {item['positive_hits']}/{item['positive_cases']} | {item['negative_false_positive_clips']}/{item['negative_clips']} | {item['false_positive_clip_fraction']} | {item['synthetic_stream_events_per_hour']} | {item['median_onset_latency_ms']} |")
    (output / "report.md").write_text("\n".join(lines_md) + "\n", encoding="utf-8")
    return report


def run_benchmark(args):
    prepare(args.output, args.suite)
    model_hash = sha256(args.model)
    corpus_hash = sha256(args.output / "manifest.json")
    suite_hash = sha256(args.suite)
    model_path = str(args.model.resolve())
    prov_path = args.output / "run-provenance.json"
    scores_path = args.output / "scores.jsonl"
    runner_snapshot = args.output / RUNNER_SNAPSHOT
    incomplete_path = args.output / RUN_INCOMPLETE
    if incomplete_path.exists():
        raise BenchError("previous runner operation is incomplete; use a fresh output directory")
    if prov_path.exists() or scores_path.exists() or runner_snapshot.exists():
        if not prov_path.is_file() or not scores_path.is_file() or not runner_snapshot.is_file():
            raise BenchError("incomplete previous score run; use a fresh output directory")
        try:
            previous = json.loads(prov_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            raise BenchError(f"invalid existing run provenance: {error}") from error
        run_fp = fingerprints(args.model, runner_snapshot)
        if (previous.get("fingerprints") != run_fp or
                previous.get("corpus_sha256") != corpus_hash or
                previous.get("suite_sha256") != suite_hash or
                previous.get("model_path") != model_path or
                previous.get("runner_snapshot") != RUNNER_SNAPSHOT):
            raise BenchError("existing scores belong to a different model/corpus/runner; use a fresh output directory")
        require_trace_hash(args.output, previous.get("trace_sha256"))
        report_run(args.output, args.suite, args.model)
        return
    initial_fp = fingerprints(args.model)
    try:
        with incomplete_path.open("x", encoding="utf-8") as marker:
            marker.write(json.dumps({"phase": "build-and-score-started"}) + "\n")
            marker.flush()
            os.fsync(marker.fileno())
    except OSError as error:
        raise BenchError(f"cannot mark runner operation incomplete: {error}") from error
    build_command = ["cargo", "build", "--locked", "--release", "--target-dir", str(ROOT / "target"),
                     "-p", "vox-golem", "--example", "wakeword_bench"]
    run_tool(build_command)
    after_build_fp = fingerprints(args.model)
    require_unchanged_sources(initial_fp, after_build_fp)
    runner_snapshot = snapshot_runner(args.output)
    snapshot_fp = fingerprints(args.model, runner_snapshot)
    command = [str(runner_snapshot), "--model", str(args.model.resolve()),
               "--manifest", str((args.output / "manifest.json").resolve()),
               "--output", str(scores_path.resolve())]
    run_tool(command, stream=True)
    verify_manifest(args.output, args.suite)
    if sha256(args.output / "manifest.json") != corpus_hash or sha256(args.suite) != suite_hash:
        raise BenchError("corpus manifest or suite changed during scoring")
    if sha256(args.model) != model_hash:
        raise BenchError("model file changed during runner scoring")
    post_fp = fingerprints(args.model, runner_snapshot)
    require_unchanged_sources(initial_fp, post_fp)
    if post_fp["runner_binary_sha256"] != snapshot_fp["runner_binary_sha256"]:
        raise BenchError("runner snapshot changed during scoring")
    run_prov = {"fingerprints": post_fp, "model_path": model_path,
                "corpus_sha256": corpus_hash, "suite_sha256": suite_hash,
                "runner_snapshot": RUNNER_SNAPSHOT,
                "trace_sha256": trace_hash(args.output)}
    prov_path.write_text(json.dumps(run_prov, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    report_run(args.output, args.suite, args.model, finalizing=True)
    incomplete_path.unlink()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    subs = parser.add_subparsers(dest="command", required=True)
    for command in ("prepare", "run", "report"):
        child = subs.add_parser(command)
        child.add_argument("--output", type=Path, required=True)
        child.add_argument("--suite", type=Path, default=DEFAULT_SUITE)
        if command == "run": child.add_argument("--model", type=Path, required=True)
    verify = subs.add_parser("verify")
    verify.add_argument("--output", type=Path, required=True)
    verify.add_argument("--suite", type=Path, default=DEFAULT_SUITE)
    args = parser.parse_args()
    args.output = args.output.resolve()
    if hasattr(args, "suite"):
        args.suite = args.suite.resolve()
    if hasattr(args, "model"):
        args.model = args.model.resolve()
    try:
        if args.command == "prepare":
            manifest = prepare(args.output, args.suite)
            print(f"Prepared immutable corpus: {len(manifest['cases'])} cases; manifest {args.output / 'manifest.json'}")
        elif args.command == "verify":
            manifest = verify_manifest(args.output, args.suite)
            print(f"Verified {len(manifest['cases'])} corpus WAV files")
        elif args.command == "report":
            report = report_run(args.output, args.suite)
            print(f"Wrote report; accepted={report['candidate_accepted']}")
        else:
            if not args.model.is_file(): raise BenchError(f"model file missing: {args.model}")
            run_benchmark(args)
            print(f"Completed measurement: {args.output / 'report.md'}")
    except BenchError as error:
        parser.exit(2, f"error: {error}\n")


if __name__ == "__main__":
    main()
