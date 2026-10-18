"""Synthetic boundary tests only: no encoder load, inference or adaptation."""
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import numpy as np
import adapt
import fit


def row(i=0, family=None, **extra):
    return dict(id=f"synthetic-{i}", family=family or f"family-{i}",
                expected="early_lookup" if i % 2 else "model_led",
                prompt=f"Synthetic request {i}", prev=None, **extra)


class AdaptTests(unittest.TestCase):
    def setUp(self):
        temporary_root = "/tmp/opencode" if Path("/tmp/opencode").is_dir() else None
        self.tmp = tempfile.TemporaryDirectory(prefix="router-adapt-", dir=temporary_root)
        self.root = Path(self.tmp.name)

    def tearDown(self):
        self.tmp.cleanup()

    def source(self, name, rows):
        p = self.root / name
        p.write_text(json.dumps(rows))
        return p

    def test_literal_preserves_whitespace_and_empty_prior(self):
        r = row()
        r["prompt"] = "  Synthetic\nrequest "
        self.assertEqual(adapt.text(r), r["prompt"])
        r["prev"] = ""
        self.assertEqual(adapt.text(r), "Prior user context:\n\n\nCurrent user request:\n  Synthetic\nrequest ")
        r["prev"] = " Synthetic prior "
        self.assertEqual(adapt.text(r), "Prior user context:\n Synthetic prior \n\nCurrent user request:\n  Synthetic\nrequest ")

    def test_export_only_strings_not_metadata(self):
        rows = [row(0, reason="must not encode", stratum="mixed"), row(1)]
        captured = []
        class Fake:
            def encode(self, strings, **kwargs):
                captured.extend(strings)
                x = np.zeros((len(strings), 384), dtype=np.float32)
                x[:, 0] = 1
                return x
        p = self.root / "features.json"
        data = adapt.export(Fake(), rows, p)
        self.assertEqual(captured, ["Synthetic request 0", "Synthetic request 1"])
        self.assertEqual(data["x"].shape, (2, 384))
        self.assertEqual(p.stat().st_mode & 0o777, 0o600)
        self.assertEqual(set(json.loads(p.read_text())),
                         {"schema", "input_version", "embedding_dim", "records"})

    def test_nonfinite_nonunit_and_wrong_dimension_rejected(self):
        for value in (np.zeros((1, 384)), np.ones((1, 384)),
                      np.full((1, 384), np.nan), np.ones((1, 383))):
            model = SimpleNamespace(encode=lambda *a, **k: value)
            with self.assertRaises(fit.Invalid):
                adapt.export(model, [row()], self.root / "invalid.json")

    def test_raw_validation(self):
        for rows in ([], [row()] * 513, [row(), row()],
                     [row(), dict(row(1), prompt=row()["prompt"])],
                      [dict(row(), expected=1)], [dict(row(), expected=True)],
                      [dict(row(), prompt=" ")], [dict(row(), prev=" ")],
                     [dict(row(), id="bad metadata!")], [dict(row(), family="bad!")],
                     [dict(row(), prev=3)]):
            with self.subTest(rows=len(rows)), self.assertRaises(fit.Invalid):
                adapt.cases(self.source("invalid.json", rows))

    def test_split_ids_families_and_literal_duplicates(self):
        for invalid in (dict(row(1), id=row()["id"]),
                        dict(row(1), family=row()["family"]),
                        dict(row(1), prompt=row()["prompt"])):
            with self.assertRaises(fit.Invalid):
                adapt.split_check([row()], [invalid])
        adapt.split_check([row()], [row(1)])

    def test_fixed_source_counts_and_balance(self):
        a = self.source("a.json", [row(i) for i in range(180)])
        b = self.source("b.json", [row(i) for i in range(180, 260)])
        c = self.source("c.json", [row(i) for i in range(260, 320)])
        train, cal, hashes = adapt.training_cases(a, b, c)
        self.assertEqual((len(train), len(cal), len(hashes)), (260, 60, 3))
        self.source("b.json", [row(i) for i in range(180, 258)])
        with self.assertRaises(fit.Invalid):
            adapt.training_cases(a, b, c)
        self.source("b.json", [dict(row(i), expected="model_led") for i in range(180, 260)])
        with self.assertRaises(fit.Invalid):
            adapt.training_cases(a, b, c)

    def test_protocol_unknown_fields_and_caps_rejected(self):
        # A synthetic object cannot impersonate the pinned complete approval.
        for obj in ({"max_steps": 65}, {"max_steps": 64, "unknown": True}):
            with self.assertRaises(fit.Invalid):
                adapt.protocol(self.source("protocol.json", obj))
        obj = {"synthetic_protocol": {"steps": 64}}
        canonical = json.dumps(obj, sort_keys=True, separators=(",", ":")).encode()
        with patch.object(adapt, "PROTOCOL", adapt.hashlib.sha256(canonical).hexdigest()):
            self.assertEqual(len(adapt.protocol(self.source("protocol.json", obj))), 64)
            for changed in ({"synthetic_protocol": {"steps": 65}},
                            dict(obj, unknown=True)):
                with self.assertRaises(fit.Invalid):
                    adapt.protocol(self.source("protocol.json", changed))

    def test_token_preflight_includes_special_tokens_never_truncates(self):
        calls = []
        def tokenizer(value, **kwargs):
            calls.append((value, kwargs))
            return {"input_ids": list(range(256 if value.endswith("0") else 257))}
        model = SimpleNamespace(tokenizer=tokenizer)
        adapt.preflight(model, [row()])
        with self.assertRaises(fit.Invalid):
            adapt.preflight(model, [row(1)])
        self.assertEqual(calls[0][1], dict(truncation=False, add_special_tokens=True))

    def test_budget_timeout_and_incomplete(self):
        now = [100.0]
        budget = adapt.Budget(lambda: now[0])
        control = SimpleNamespace(should_training_stop=False)
        self.assertFalse(budget.stop(control).should_training_stop)
        now[0] = 1000.0
        self.assertTrue(budget.stop(control).should_training_stop)
        with self.assertRaises(fit.Invalid):
            budget.complete(64)
        budget = adapt.Budget(lambda: 0)
        with self.assertRaises(fit.Invalid):
            budget.complete(63)
        self.assertEqual(budget.complete(64), 0)

    def test_loss_records_use_the_pinned_embedding_loss_field(self):
        self.assertEqual(adapt.body_loss_record(16, {"embedding_loss": .25}),
                         dict(stage="body_training", step=16, loss=.25))
        for step, logs in ((15, {"embedding_loss": .25}), (16, {}),
                           (16, {"embedding_loss": float("nan")}),
                           (16, {"embedding_loss": True}), (16, {"loss": .25})):
            self.assertIsNone(adapt.body_loss_record(step, logs))

    def test_gate_counts_critical_false_positive(self):
        rows = [row(i, stratum=("ordinary", "context", "mixed")[i // 2],
                    critical_missing_anchor=(i == 0)) for i in range(6)]
        scores = np.array([-1, 1, -1, 1, -1, 1], dtype=np.float32)
        result = adapt.gate_counts(rows, scores)
        self.assertTrue(result["passes"])
        json.dumps(result, allow_nan=False)
        self.assertEqual(result["overall"]["tp"], 3)
        scores[0] = 0
        result = adapt.gate_counts(rows, scores)
        self.assertFalse(result["passes"])
        self.assertEqual(result["missing_anchor_fp"], 1)
        self.assertEqual(result["overall"]["fp"], 1)
        json.dumps(result, allow_nan=False)

    def test_evaluation_layout_requires_the_approved_strata_and_critical_slice(self):
        critical = set(range(0, 12, 2)) | set(range(72, 84, 2))
        def layout():
            return [row(i, stratum="ordinary" if i < 72 else "context" if i < 108 else "mixed",
                        critical_missing_anchor=i in critical) for i in range(120)]
        adapt.evaluation_layout(layout())
        invalid = layout()
        invalid[0]["stratum"] = "standalone"
        with self.assertRaises(fit.Invalid):
            adapt.evaluation_layout(invalid)
        invalid = layout()
        invalid[0]["critical_missing_anchor"] = False
        with self.assertRaises(fit.Invalid):
            adapt.evaluation_layout(invalid)
        invalid = layout()
        invalid[0]["critical_missing_anchor"] = False
        invalid[84]["critical_missing_anchor"] = True
        with self.assertRaises(fit.Invalid):
            adapt.evaluation_layout(invalid)

    def test_create_new_rejects_sources_git_and_existing(self):
        p = self.source("source.json", {})
        for destination in (p, Path(__file__).parent / "must-not-create"):
            with self.assertRaises(fit.Invalid):
                adapt.new_directory(destination, [p])
        directory = adapt.new_directory(self.root / "new", [p])
        with self.assertRaises(fit.Invalid):
            adapt.new_directory(directory, [])

    def test_freeze_required_before_reading_cases(self):
        with patch.object(adapt, "cases") as read:
            with self.assertRaises(FileNotFoundError):
                adapt.evaluate(SimpleNamespace(run=self.root, cases="unread", report="unused"))
            read.assert_not_called()

    def test_freeze_requires_heads_and_matching_hash(self):
        marker = dict(schema="router_adaptation_freeze_v1", protocol_contract=adapt.PROTOCOL,
                      input_version=fit.VERSION, steps=64, parameter_changed=True,
                      time_completed=True, parameter_before="before", parameter_after="after",
                      body_elapsed_seconds=1, files={}, fitting_inputs=[str(i) for i in range(320)])
        self.source("freeze.json", marker)
        with self.assertRaises(fit.Invalid):
            adapt.frozen_run(self.root)
        marker["files"] = {f"{kind}.{split}.json": "0" * 64
                           for kind in ("control", "adapted")
                           for split in ("head", "train", "calibration")}
        for name in marker["files"]:
            self.source(name, {})
        self.source("freeze.json", marker)
        with self.assertRaises(fit.Invalid):
            adapt.frozen_run(self.root)

    def test_valid_synthetic_freeze_and_model_tamper(self):
        base = self.root / "base"
        adapted = self.root / "adapted-model"
        base.mkdir()
        adapted.mkdir()
        (base / "model.safetensors").write_bytes(b"synthetic base, not weights")
        (adapted / "model.safetensors").write_bytes(b"synthetic candidate, not weights")
        models = {}
        files = {}
        for kind, directory in (("control", base), ("adapted", adapted)):
            model_files = adapt.inventory(directory)
            models[kind] = dict(path=str(directory), files=model_files,
                                sha256=adapt.tree_sha(model_files))
            for split in ("head", "train", "calibration"):
                p = self.source(f"{kind}.{split}.json", {})
                files[p.name] = adapt.sha(p)
        marker = dict(schema="router_adaptation_freeze_v1", protocol_contract=adapt.PROTOCOL,
                      input_version=fit.VERSION, steps=64, parameter_changed=True,
                      time_completed=True, parameter_before="before", parameter_after="after",
                      body_elapsed_seconds=1, files=files, models=models,
                      fitting_inputs=[str(i) for i in range(320)])
        self.source("freeze.json", marker)
        def fake_head(path):
            kind = Path(path).name.split(".")[0]
            return dict(encoder_sha256=models[kind]["sha256"],
                        train=dict(sha256=files[f"{kind}.train.json"]),
                        calibration=dict(sha256=files[f"{kind}.calibration.json"]))
        with patch.object(adapt, "BASE", adapt.sha(base / "model.safetensors")), \
                patch.object(fit, "load_head", side_effect=fake_head):
            self.assertEqual(adapt.frozen_run(self.root)[0], marker)
            (adapted / "model.safetensors").write_bytes(b"tampered synthetic model")
            with self.assertRaises(fit.Invalid):
                adapt.frozen_run(self.root)


if __name__ == "__main__":
    unittest.main()
