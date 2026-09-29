#!/usr/bin/env python3
"""Verify two independently generated/scored benchmark runs are reproducible."""

import argparse
import json
from pathlib import Path

from bench import DEFAULT_SUITE, BenchError, report_run, sha256, verify_manifest


def compare(left, right, suite):
    left_manifest = verify_manifest(left, suite)
    right_manifest = verify_manifest(right, suite)
    if sha256(left / "manifest.json") != sha256(right / "manifest.json"):
        raise BenchError("independent corpus manifests differ")
    left_report = report_run(left, suite)
    right_report = report_run(right, suite)
    runner_hashes = [report["fingerprints"].pop("runner_binary_sha256")
                     for report in (left_report, right_report)]
    if left_report != right_report:
        raise BenchError("run fingerprints, policy results, or selection differ")
    traces = []
    for directory in (left, right):
        records = [json.loads(line) for line in (directory / "scores.jsonl").read_text().splitlines()]
        cases = {record["id"]: {key: value for key, value in record.items() if key != "elapsed_ms"}
                 for record in records[1:]}
        traces.append((records[0], cases))
    if traces[0] != traces[1]:
        raise BenchError("score values, sample positions, or buffered detector results differ")
    return {
        "corpus_cases": len(left_manifest["cases"]),
        "independent_corpus_cases": len(right_manifest["cases"]),
        "score_points": sum(len(case["scores"]) for case in traces[0][1].values()),
        "corpus_sha256": sha256(left / "manifest.json"),
        "identical_audio_scores_and_decisions": True,
        "runner_binary_sha256": runner_hashes,
        "runner_binaries_identical": runner_hashes[0] == runner_hashes[1],
        "timing_comparison": "elapsed wall-clock timings intentionally excluded",
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--left", type=Path, required=True)
    parser.add_argument("--right", type=Path, required=True)
    parser.add_argument("--suite", type=Path, default=DEFAULT_SUITE)
    args = parser.parse_args()
    try:
        print(json.dumps(compare(args.left.resolve(), args.right.resolve(), args.suite.resolve()), indent=2))
    except BenchError as error:
        parser.exit(2, f"error: {error}\n")


if __name__ == "__main__":
    main()
