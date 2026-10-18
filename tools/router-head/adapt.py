"""One approved offline embedding adaptation; run and fresh evaluation are separate."""
import argparse
import gc
import hashlib
import importlib.metadata
import json
import logging
import os
from pathlib import Path
import sys
import time

import numpy as np
import fit

PROTOCOL = "dc345cc33512adb6a3d871b264a55c14ac4c848a43d1684252be8b8e5b270bd5"
BASE = "3c9f31665447c8911517620762200d2245a2518d6e7208acc78cd9db317e21ad"
require = fit.require


def stage(name, **metadata):
    print(json.dumps(dict(stage=name, **metadata), sort_keys=True), flush=True)


def sha(path):
    h = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def inventory(directory):
    directory = Path(directory)
    require(directory.is_dir() and not directory.is_symlink())
    result = {}
    for p in sorted(directory.rglob("*")):
        require(not p.is_symlink())
        if p.is_file():
            result[p.relative_to(directory).as_posix()] = sha(p)
    require(bool(result))
    return result


def tree_sha(files):
    return hashlib.sha256(json.dumps(files, sort_keys=True).encode()).hexdigest()


def protocol(path):
    data, digest = fit.read_json(path)
    canonical = json.dumps(data, sort_keys=True, separators=(",", ":")).encode()
    # Pin the complete approved object, including every cap and all unknown fields.
    require(hashlib.sha256(canonical).hexdigest() == PROTOCOL)
    return digest


def text(row):
    prompt, prev = row["prompt"], row.get("prev")
    require(type(prompt) is str and (prev is None or type(prev) is str))
    value = prompt if prev is None else f"Prior user context:\n{prev}\n\nCurrent user request:\n{prompt}"
    require(len(value.encode()) <= 32 * 1024)
    return value


def input_hash(row):
    return hashlib.sha256(text(row).encode()).hexdigest()


def cases(path):
    rows, digest = fit.read_json(path)
    require(type(rows) is list and 1 <= len(rows) <= 512)
    ids, inputs = set(), set()
    for row in rows:
        require(type(row) is dict)
        ident = fit.identifier(row["id"])
        fit.identifier(row["family"])
        require(type(row["expected"]) is str and row["expected"] in ("model_led", "early_lookup"))
        value = text(row)
        require(bool(row["prompt"].strip()) and
                (row.get("prev") is None or bool(row["prev"].strip())))
        require(ident not in ids and value not in inputs)
        ids.add(ident)
        inputs.add(value)
    return rows, digest


def label(row):
    return int(row["expected"] == "early_lookup")


def split_check(train, cal):
    for field in ("id", "family"):
        require(not {r[field] for r in train} & {r[field] for r in cal})
    require(not {text(r) for r in train} & {text(r) for r in cal})


def training_cases(train, everyday, calibration):
    a, ah = cases(train)
    b, bh = cases(everyday)
    c, ch = cases(calibration)
    require((len(a), len(b), len(c)) == (180, 80, 60))
    # Reuse the same split checker to forbid duplicates between source files too.
    require(not {r["id"] for r in a} & {r["id"] for r in b})
    require(not {text(r) for r in a} & {text(r) for r in b})
    merged = a + b
    require(sum(map(label, merged)) == 130)
    split_check(merged, c)
    return merged, c, dict(train=ah, everyday=bh, calibration=ch)


def preflight(model, rows):
    for row in rows:
        tokens = model.tokenizer(text(row), truncation=False, add_special_tokens=True)
        require(len(tokens["input_ids"]) <= 256)


def export(model, rows, destination):
    # Only the exact literal strings cross the embedding boundary.
    vectors = np.asarray(model.encode([text(r) for r in rows], batch_size=8,
                                     show_progress_bar=False, convert_to_numpy=True,
                                     normalize_embeddings=True), dtype=np.float32)
    require(vectors.shape == (len(rows), fit.DIM) and np.isfinite(vectors).all())
    require(np.allclose(np.linalg.norm(vectors.astype(np.float64), axis=1), 1, atol=1e-5))
    records = [dict(index=i, id=r["id"], family=r["family"], label=label(r),
                    embedding=vectors[i].tolist()) for i, r in enumerate(rows)]
    fit.write_new(destination, dict(schema="retrieval_features_v1", input_version=fit.VERSION,
                                   embedding_dim=fit.DIM, records=records), [])
    return fit.load_features(destination)


class Budget:
    """Dependency-free deadline state, shared by callback and post-training check."""
    def __init__(self, clock=time.monotonic):
        self.clock = clock
        self.start = clock()
        self.hit = False

    def elapsed(self):
        elapsed = self.clock() - self.start
        self.hit |= elapsed >= 900
        return elapsed

    def stop(self, control):
        self.elapsed()
        if self.hit:
            control.should_training_stop = True
        return control

    def complete(self, steps):
        elapsed = self.elapsed()
        require(steps == 64 and not self.hit)
        return elapsed


def frameworks():
    # Never import training frameworks during NumPy-only validation/tests.
    for package, version in (("torch", "2.14.1+cpu"), ("setfit", "1.2.0"),
                             ("sentence-transformers", "6.1.0"), ("transformers", "5.20.1")):
        require(importlib.metadata.version(package) == version)
    for name in ("HF_HUB_OFFLINE", "TRANSFORMERS_OFFLINE", "HF_HUB_DISABLE_TELEMETRY",
                 "DO_NOT_TRACK", "HF_DATASETS_OFFLINE"):
        os.environ[name] = "1"
    os.environ["TOKENIZERS_PARALLELISM"] = "false"
    logging.disable(logging.CRITICAL)
    import torch
    from sentence_transformers import SentenceTransformer
    require(torch.version.cuda is None)
    torch.set_num_threads(2)
    torch.set_num_interop_threads(1)
    return torch, SentenceTransformer


def load_model(path, cls):
    model = cls(str(path), device="cpu", local_files_only=True, trust_remote_code=False,
                model_kwargs={"use_safetensors": True})
    require(model.get_sentence_embedding_dimension() == 384)
    require(model[1].pooling_mode == "cls")
    require(all(str(p.dtype) == "torch.float32" and p.device.type == "cpu"
                for p in model.parameters()))
    model.max_seq_length = 256
    return model


def parameter_sha(model):
    h = hashlib.sha256()
    for name, parameter in model.named_parameters():
        h.update(name.encode())
        h.update(parameter.detach().cpu().contiguous().numpy().tobytes())
    return h.hexdigest()


def body_loss_record(step, logs):
    loss = (logs or {}).get("embedding_loss")
    if step and step % 16 == 0 and type(loss) in (float, int) and np.isfinite(loss):
        return dict(stage="body_training", step=step, loss=float(loss))
    return None


def train_body(model, rows, output):
    from sentence_transformers.sentence_transformer.losses import CosineSimilarityLoss
    from setfit import SetFitModel, Trainer, TrainingArguments
    from sklearn.linear_model import LogisticRegression
    from transformers import TrainerCallback, set_seed

    class Callback(TrainerCallback):
        def on_step_end(self, args, state, control, **kwargs):
            return budget.stop(control)

        def on_log(self, args, state, control, logs=None, **kwargs):
            record = body_loss_record(state.global_step, logs)
            if record is not None:
                print(json.dumps(record), flush=True)

    set_seed(20261018)
    args = TrainingArguments(output_dir=str(output), batch_size=(8, 8), num_epochs=(1, 1),
                             max_steps=64, body_learning_rate=(2e-5, 2e-5),
                             loss=CosineSimilarityLoss, warmup_proportion=0.1,
                             max_length=256, use_amp=False, seed=20261018,
                             report_to=[], eval_strategy="no", save_strategy="no",
                             load_best_model_at_end=False, show_progress_bar=False,
                             logging_steps=16, logging_first_step=False)
    trainer = Trainer(model=SetFitModel(model_body=model, model_head=LogisticRegression(),
                                       normalize_embeddings=True), args=args,
                      metric=lambda *a, **k: {}, callbacks=[Callback()])
    # No classifier training, automatic evaluation, checkpoint selection or resume.
    budget = Budget()
    trainer.train_embeddings([text(r) for r in rows], [label(r) for r in rows])
    return budget.complete(trainer.st_trainer.state.global_step)


def new_directory(path, inputs):
    path = fit.check_destination(path, inputs)
    path.mkdir(mode=0o700)
    return path


def run(args):
    protocol_sha = protocol(args.protocol)
    train, cal, sources = training_cases(args.train, args.everyday, args.calibration)
    base = Path(args.model).resolve()
    require(sha(base / "model.safetensors") == BASE)
    base_files = inventory(base)
    require(not Path(args.output).resolve().is_relative_to(base))
    output = new_directory(args.output, [args.protocol, args.model, args.train,
                                         args.everyday, args.calibration])
    torch, cls = frameworks()
    stage("control_preflight")
    model = load_model(base, cls)
    preflight(model, train + cal)  # Before either control encoding or training.
    control_sha = tree_sha(base_files)
    export(model, train, output / "control.train.json")
    export(model, cal, output / "control.calibration.json")
    fit.fit_head(output / "control.train.json", output / "control.calibration.json",
                  output / "control.head.json", control_sha)
    stage("control_frozen", train_rows=len(train), calibration_rows=len(cal))
    del model
    gc.collect()
    model = load_model(base, cls)
    before = parameter_sha(model)
    stage("body_requested", maximum_steps=64, batch_size=8, budget_seconds=900)
    elapsed = train_body(model, train, output / "training")
    after = parameter_sha(model)
    require(before != after)
    stage("body_completed", steps=64, elapsed_seconds=elapsed, parameters_changed=True)
    adapted = output / "adapted-model"
    require(not adapted.exists())
    model.save_pretrained(str(adapted), safe_serialization=True, create_model_card=False)
    # Preserve the original upstream notices/card, never generated training examples.
    if not (adapted / "README.md").exists() and (base / "README.md").exists():
        with (adapted / "README.md").open("xb") as stream:
            stream.write((base / "README.md").read_bytes())
    del model
    gc.collect()
    adapted_files = inventory(adapted)
    adapted_sha = tree_sha(adapted_files)
    model = load_model(adapted, cls)
    preflight(model, train + cal)
    export(model, train, output / "adapted.train.json")
    export(model, cal, output / "adapted.calibration.json")
    fit.fit_head(output / "adapted.train.json", output / "adapted.calibration.json",
                  output / "adapted.head.json", adapted_sha)
    stage("adapted_saved_and_head_frozen", train_rows=len(train), calibration_rows=len(cal))
    require(inventory(base) == base_files and sha(base / "model.safetensors") == BASE)
    require(protocol(args.protocol) == protocol_sha)
    require(all(sha(path) == sources[name] for name, path in
                (("train", args.train), ("everyday", args.everyday),
                 ("calibration", args.calibration))))
    # Freeze is written last, never on incomplete/expired training.
    frozen = dict(schema="router_adaptation_freeze_v1", protocol_sha256=protocol_sha,
                  input_version=fit.VERSION, sources=sources, protocol_contract=PROTOCOL,
                  fitting_inputs=[input_hash(r) for r in train + cal], steps=64,
                  parameter_changed=True, parameter_before=before, parameter_after=after,
                  body_elapsed_seconds=elapsed, time_completed=True,
                  versions={p: importlib.metadata.version(p) for p in
                            ("torch", "setfit", "sentence-transformers", "transformers", "numpy")},
                  models=dict(control=dict(path=str(base), files=base_files, sha256=control_sha),
                              adapted=dict(path=str(adapted), files=adapted_files, sha256=adapted_sha)),
                  files=inventory(output))
    digest = fit.write_new(output / "freeze.json", frozen, [])
    return dict(stage="frozen", freeze_sha256=digest, steps=64, body_elapsed_seconds=elapsed)


def frozen_run(directory):
    directory = Path(directory).resolve()
    frozen, digest = fit.read_json(directory / "freeze.json")
    require(frozen["schema"] == "router_adaptation_freeze_v1" and
            frozen["protocol_contract"] == PROTOCOL and
            frozen["input_version"] == fit.VERSION and frozen["steps"] == 64 and
            frozen["parameter_changed"] is True and frozen["time_completed"] is True and
            frozen["parameter_before"] != frozen["parameter_after"] and
            0 <= frozen["body_elapsed_seconds"] < 900)
    required = {f"{kind}.{split}.json" for kind in ("control", "adapted")
                for split in ("head", "train", "calibration")}
    require(required <= set(frozen["files"]))
    require(len(frozen["fitting_inputs"]) == 320 and
            len(set(frozen["fitting_inputs"])) == 320)
    for relative, expected in frozen["files"].items():
        p = directory / relative
        require(p.resolve().is_relative_to(directory) and not p.is_symlink())
        require(sha(p) == expected)
    for kind in ("control", "adapted"):
        entry = frozen["models"][kind]
        require(inventory(entry["path"]) == entry["files"])
        require(tree_sha(entry["files"]) == entry["sha256"])
        head = fit.load_head(directory / f"{kind}.head.json")
        require(head["encoder_sha256"] == entry["sha256"])
        for split in ("train", "calibration"):
            require(frozen["files"][f"{kind}.{split}.json"] == head[split]["sha256"])
    require(sha(Path(frozen["models"]["control"]["path"]) / "model.safetensors") == BASE)
    return frozen, digest


def gate_counts(rows, scores):
    y = np.array([label(r) for r in rows])
    overall = fit.counts(y, scores)
    strata = {}
    for stratum in ("ordinary", "context", "mixed"):
        indices = [i for i, r in enumerate(rows) if r["stratum"] == stratum]
        require(bool(indices))
        strata[stratum] = fit.counts(y[indices], scores[indices])
    critical = sum(r["critical_missing_anchor"] and label(r) == 0 and scores[i] >= 0
                   for i, r in enumerate(rows))
    gates = dict(accuracy=overall["accuracy"] >= .95,
                 precision=overall["precision"] is not None and overall["precision"] >= .95,
                 recall=overall["recall"] is not None and overall["recall"] >= .90,
                 context=strata["context"]["accuracy"] >= .90,
                  mixed=strata["mixed"]["accuracy"] >= .90, missing_anchor=bool(critical == 0))
    return dict(overall=overall, strata=strata, missing_anchor_fp=int(critical),
                 gates=gates, passes=all(gates.values()))


def evaluation_layout(rows):
    require(len(rows) == 120)
    for stratum, count in (("ordinary", 72), ("context", 36), ("mixed", 12)):
        subset = [r for r in rows if r.get("stratum") == stratum]
        require(len(subset) == count and sum(map(label, subset)) == count // 2)
    require(all(type(r.get("critical_missing_anchor")) is bool for r in rows))
    critical = [r for r in rows if r["critical_missing_anchor"]]
    require(len(critical) == 12 and all(label(r) == 0 for r in critical))
    require(sum(r["stratum"] == "ordinary" for r in critical) == 6)
    require(sum(r["stratum"] == "context" for r in critical) == 6)


def evaluate(args):
    frozen, digest = frozen_run(args.run)  # Do not read cases before verifying freeze.
    report = fit.check_destination(args.report, [args.cases])
    require(not report.resolve().is_relative_to(Path(args.run).resolve()))
    require(all(not report.resolve().is_relative_to(Path(m["path"]).resolve())
                for m in frozen["models"].values()))
    rows, cases_sha = cases(args.cases)
    evaluation_layout(rows)
    require(not {input_hash(r) for r in rows} & set(frozen["fitting_inputs"]))
    # IDs/families are checked against BOTH frozen fitting splits before model loading.
    for kind in ("control", "adapted"):
        head = fit.load_head(Path(args.run) / f"{kind}.head.json")
        data = dict(ids=[r["id"] for r in rows], families=[r["family"] for r in rows])
        fit.disjoint(head["train"], data)
        fit.disjoint(head["calibration"], data)
    torch, cls = frameworks()
    result = {}
    for kind in ("control", "adapted"):
        model = load_model(frozen["models"][kind]["path"], cls)
        preflight(model, rows)
        features = report.with_name(report.name + f".{kind}.features.json")
        export(model, rows, features)
        head_path = Path(args.run) / f"{kind}.head.json"
        aggregate = fit.evaluate(head_path, features)
        head, data = fit.load_head(head_path), fit.load_features(features)
        scores = data["x"] @ np.array(head["coefficients"], dtype=np.float32) + np.float32(head["bias"])
        result[kind] = dict(**gate_counts(rows, scores), provenance=aggregate)
        del model
        gc.collect()
    result.update(freeze_sha256=digest, cases_sha256=cases_sha,
                  interpretation="Synthetic screening only; no population, phone or final-answer claim.")
    fit.write_new(report, result, [args.cases])
    return result


def main():
    parser = fit.SafeParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    runner = commands.add_parser("run")
    for field in ("protocol", "model", "train", "everyday", "calibration", "output"):
        runner.add_argument("--" + field, required=True)
    evaluator = commands.add_parser("evaluate")
    for field in ("run", "cases", "report"):
        evaluator.add_argument("--" + field, required=True)
    args = parser.parse_args()
    old_umask = os.umask(0o077)
    try:
        result = run(args) if args.command == "run" else evaluate(args)
        print(json.dumps(result, sort_keys=True, allow_nan=False))
        return 0
    except Exception as error:
        print(json.dumps(dict(error="invalid_input_or_operation", failure_type=type(error).__name__)), file=sys.stderr)
        return 2
    finally:
        os.umask(old_umask)


if __name__ == "__main__":
    sys.exit(main())
