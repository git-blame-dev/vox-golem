"""Bounded offline ridge decision head; never loads an encoder or raw text."""
import argparse
import hashlib
import json
import os
import platform
import re
import sys
from pathlib import Path

import numpy as np

DIM = 384
VERSION = "literal-labelled-context-v1"
LIMIT = 40 * 1024 * 1024
CANDIDATES = (0.01, 0.1, 1.0)


class Invalid(Exception):
    pass


def require(condition):
    if not condition:
        raise Invalid("invalid_contract")


def read_json(path):
    with Path(path).open("rb") as stream:
        raw = stream.read(LIMIT + 1)
    require(len(raw) <= LIMIT)
    def pairs(items):
        obj = {}
        for key, value in items:
            require(key not in obj)
            obj[key] = value
        return obj
    return json.loads(raw, object_pairs_hook=pairs), hashlib.sha256(raw).hexdigest()


def identifier(value):
    require(isinstance(value, str) and re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.:/-]{0,255}", value))
    return value


def number(value):
    require(type(value) in (int, float) and abs(value) <= float(np.finfo(np.float32).max)
            and np.isfinite(value))
    return value


def load_features(path):
    data, digest = read_json(path)
    require(type(data) is dict and set(data) == {"schema", "input_version", "embedding_dim", "records"})
    require(data["schema"] == "retrieval_features_v1" and data["input_version"] == VERSION)
    require(type(data["embedding_dim"]) is int and data["embedding_dim"] == DIM)
    records = data["records"]
    require(type(records) is list and 1 <= len(records) <= 512)
    ids, indices, families, rows, labels = set(), set(), set(), [], []
    for row in records:
        require(type(row) is dict and set(row) == {"index", "id", "family", "label", "embedding"})
        ident, family = identifier(row["id"]), identifier(row["family"])
        index, label, vector = row["index"], row["label"], row["embedding"]
        require(type(index) is int and 0 <= index <= 2**31 - 1 and index not in indices)
        require(ident not in ids and type(label) is int and label in (0, 1))
        require(type(vector) is list and len(vector) == DIM)
        x = np.array([number(v) for v in vector], dtype=np.float32)
        norm = np.linalg.norm(x.astype(np.float64))
        require(norm > 0)
        rows.append((x.astype(np.float64) / norm).astype(np.float32))
        labels.append(label)
        ids.add(ident)
        families.add(family)
        indices.add(index)
    return dict(x=np.stack(rows), y=np.array(labels), ids=sorted(ids), families=sorted(families),
                sha256=digest)


def provenance(data):
    return {key: data[key] for key in ("ids", "families", "sha256")}


def disjoint(a, b):
    require(not set(a["ids"]) & set(b["ids"]))
    require(not set(a["families"]) & set(b["families"]))


def counts(y, scores):
    predicted = scores >= np.float32(0)
    tp = int(np.sum(predicted & (y == 1)))
    tn = int(np.sum(~predicted & (y == 0)))
    fp = int(np.sum(predicted & (y == 0)))
    fn = int(np.sum(~predicted & (y == 1)))
    n = len(y)
    return dict(tp=tp, tn=tn, fp=fp, fn=fn, n=n, expected=n, coverage=1.0,
                correct=tp + tn, accuracy=(tp + tn) / n, percent_correct=100 * (tp + tn) / n,
                precision=tp / (tp + fp) if tp + fp else None,
                recall=tp / (tp + fn) if tp + fn else None)


def selection_key(metrics, regularization):
    return metrics["correct"], -metrics["fp"], regularization


def check_destination(path, inputs):
    path = Path(path)
    require(path.is_absolute())
    resolved = path.resolve()
    require(resolved not in [Path(p).resolve() for p in inputs])
    require(not path.exists() and not path.is_symlink() and path.parent.is_dir())
    for parent in (resolved.parent, *resolved.parents):
        require(not (parent / ".git").exists())
    # Also reject the source checkout even when its .git marker is unavailable.
    require(not resolved.is_relative_to(Path(__file__).resolve().parents[2]))
    return path


def write_new(path, data, inputs):
    path = check_destination(path, inputs)
    raw = (json.dumps(data, sort_keys=True, indent=2, allow_nan=False) + "\n").encode()
    with open(path, "xb", opener=lambda name, flags: os.open(name, flags, 0o600)) as stream:
        stream.write(raw)
        stream.flush()
        os.fsync(stream.fileno())
    return hashlib.sha256(raw).hexdigest()


def fit_head(train_path, cal_path, output, encoder_sha256):
    require(isinstance(encoder_sha256, str) and re.fullmatch(r"[0-9a-fA-F]{64}", encoder_sha256))
    check_destination(output, [train_path, cal_path])
    train, cal = load_features(train_path), load_features(cal_path)
    disjoint(train, cal)
    require(set(train["y"]) == {0, 1})
    x = np.column_stack((train["x"].astype(np.float64), np.ones(len(train["y"]))))
    gram, rhs = x.T @ x, x.T @ (2 * train["y"] - 1)
    candidates = []
    best = None
    for regularization in CANDIDATES:
        penalty = np.full(DIM + 1, regularization)
        penalty[-1] = 0
        weights = np.linalg.solve(gram + np.diag(penalty), rhs).astype(np.float32)
        require(np.isfinite(weights).all())
        metrics = counts(cal["y"], cal["x"] @ weights[:-1] + weights[-1])
        candidates.append(dict(lambda_value=regularization, **metrics))
        key = selection_key(metrics, regularization)
        if best is None or key > best[0]:
            best = key, weights, regularization
    _, weights, selected = best
    head = dict(schema="retrieval_ridge_head_v1", input_version=VERSION, embedding_dim=DIM,
                encoder_sha256=encoder_sha256.lower(), coefficients=weights[:-1].tolist(),
                bias=float(weights[-1]), threshold=0, selected_lambda=selected,
                candidates=candidates, train=provenance(train), calibration=provenance(cal),
                numpy_version=np.__version__, python_version=platform.python_version())
    digest = write_new(output, head, [train_path, cal_path])
    return dict(head_sha256=digest, selected_lambda=selected, candidates=candidates)


def load_head(path, *, include_digest=False):
    head, digest = read_json(path)
    require(type(head) is dict and head.get("schema") == "retrieval_ridge_head_v1")
    require(set(head) == {"schema", "input_version", "embedding_dim", "encoder_sha256",
                         "coefficients", "bias", "threshold", "selected_lambda", "candidates",
                         "train", "calibration", "numpy_version", "python_version"})
    require(head.get("input_version") == VERSION and type(head.get("embedding_dim")) is int
            and head["embedding_dim"] == DIM)
    require(type(head.get("threshold")) in (int, float) and head["threshold"] == 0)
    require(type(head["selected_lambda"]) is float and head["selected_lambda"] in CANDIDATES)
    for field in ("numpy_version", "python_version"):
        require(isinstance(head[field], str) and
                re.fullmatch(r"[A-Za-z0-9.+_-]{1,80}", head[field]))
    require(type(head["candidates"]) is list and len(head["candidates"]) == len(CANDIDATES))
    for candidate, regularization in zip(head["candidates"], CANDIDATES):
        require(type(candidate) is dict and candidate.get("lambda_value") == regularization)
        for field in ("tp", "tn", "fp", "fn", "n", "expected", "correct"):
            require(type(candidate.get(field)) is int and 0 <= candidate[field] <= 512)
        require(candidate["n"] > 0 and candidate["n"] == candidate["expected"] ==
                sum(candidate[field] for field in ("tp", "tn", "fp", "fn")))
        require(candidate["correct"] == candidate["tp"] + candidate["tn"])
        tp, fp, fn, n = (candidate[k] for k in ("tp", "fp", "fn", "n"))
        expected_metrics = dict(coverage=1.0, accuracy=candidate["correct"] / n,
                                percent_correct=100 * candidate["correct"] / n,
                                precision=tp / (tp + fp) if tp + fp else None,
                                recall=tp / (tp + fn) if tp + fn else None)
        require(set(candidate) == {"lambda_value", "tp", "tn", "fp", "fn", "n", "expected",
                                   "correct", *expected_metrics})
        for field, value in expected_metrics.items():
            require(candidate[field] is None if value is None else
                    type(candidate[field]) in (int, float) and candidate[field] == value)
    selected = max(head["candidates"], key=lambda c: selection_key(c, c["lambda_value"]))
    require(head["selected_lambda"] == selected["lambda_value"])
    require(isinstance(head.get("encoder_sha256"), str) and
            re.fullmatch(r"[0-9a-f]{64}", head["encoder_sha256"]))
    vector = head.get("coefficients")
    require(type(vector) is list and len(vector) == DIM)
    for value in vector + [head.get("bias")]:
        number(value)
        require(float(np.float32(value)) == value)
    for split in ("train", "calibration"):
        source = head.get(split)
        require(type(source) is dict and set(source) == {"ids", "families", "sha256"})
        require(isinstance(source["sha256"], str) and re.fullmatch(r"[0-9a-f]{64}", source["sha256"]))
        for field in ("ids", "families"):
            values = source[field]
            require(type(values) is list and 1 <= len(values) <= 512)
            for value in values:
                identifier(value)
            require(len(set(values)) == len(values))
    disjoint(head["train"], head["calibration"])
    require(all(c["n"] == len(head["calibration"]["ids"]) for c in head["candidates"]))
    return (head, digest) if include_digest else head


def evaluate(head_path, features_path, report=None, regression=False):
    if report is not None:
        check_destination(report, [head_path, features_path])
    head, head_digest = load_head(head_path, include_digest=True)
    data = load_features(features_path)
    if not regression:
        disjoint(head["train"], data)
        disjoint(head["calibration"], data)
    with np.errstate(over="ignore", invalid="ignore"):
        scores = data["x"] @ np.array(head["coefficients"], dtype=np.float32) + np.float32(head["bias"])
    require(np.isfinite(scores).all())
    result = dict(kind="regression" if regression else "heldout", **counts(data["y"], scores),
                  head_sha256=head_digest,
                  features_sha256=data["sha256"])
    if report is not None:
        write_new(report, result, [head_path, features_path])
    return result


class SafeParser(argparse.ArgumentParser):
    def error(self, message):
        self.exit(2, '{"error":"invalid_arguments"}\n')


def main():
    parser = SafeParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    fit = commands.add_parser("fit")
    for arg in ("train", "calibration", "output", "encoder-sha256"):
        fit.add_argument("--" + arg, required=True)
    evaluation = commands.add_parser("evaluate")
    evaluation.add_argument("--head", required=True)
    evaluation.add_argument("--features", required=True)
    evaluation.add_argument("--report")
    evaluation.add_argument("--regression", action="store_true")
    args = parser.parse_args()
    try:
        if args.command == "fit":
            result = fit_head(args.train, args.calibration, args.output, args.encoder_sha256)
        else:
            result = evaluate(args.head, args.features, args.report, args.regression)
        print(json.dumps(result, sort_keys=True, allow_nan=False))
        return 0
    except (Invalid, OSError, ValueError, TypeError, KeyError, OverflowError, RecursionError, np.linalg.LinAlgError):
        print('{"error":"invalid_input_or_operation"}', file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
