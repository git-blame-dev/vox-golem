import importlib.util
import json
import tempfile
import unittest
import subprocess
import sys
from pathlib import Path

import numpy as np

spec = importlib.util.spec_from_file_location("router_fit", Path(__file__).with_name("fit.py"))
fit = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fit)


class FitTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="router-synthetic-", dir="/tmp/opencode")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)

    def features(self, name, family=None):
        records = []
        for i, label in enumerate([0, 1, 0, 1]):
            x = [0.0] * 384
            x[0] = 1.0 if label else -1.0
            records.append(dict(index=i, id=f"{name}-{i}", family=family or name,
                                label=label, embedding=x))
        return dict(schema="retrieval_features_v1", input_version="literal-labelled-context-v1",
                    embedding_dim=384, records=records)

    def write(self, name, value):
        path = self.root / name
        path.write_text(json.dumps(value))
        return path

    def test_fit_and_evaluate(self):
        train = self.write("train.json", self.features("train"))
        cal = self.write("cal.json", self.features("cal"))
        out = self.root / "head.json"
        result = fit.fit_head(train, cal, out, "a" * 64)
        self.assertEqual(result["selected_lambda"], 1.0)
        head = fit.load_head(out)
        self.assertEqual(len(head["coefficients"]), 384)
        test = self.write("test.json", self.features("test"))
        self.assertEqual(fit.evaluate(out, test)["correct"], 4)
        with self.assertRaises(fit.Invalid):
            fit.evaluate(out, train)
        self.assertEqual(fit.evaluate(out, train, regression=True)["kind"], "regression")
        with self.assertRaises(fit.Invalid):
            fit.fit_head(train, cal, out, "a" * 64)

    def test_selection(self):
        self.assertEqual(fit.selection_key({"correct": 3, "fp": 0}, .1), (3, 0, .1))
        self.assertGreater(fit.selection_key({"correct": 3, "fp": 0}, .01),
                           fit.selection_key({"correct": 3, "fp": 1}, 1.0))

    def test_float32_normalization_and_counts(self):
        data = self.features("normalized")
        data["records"][0]["embedding"][0:2] = [3., 4.]
        loaded = fit.load_features(self.write("normalized.json", data))
        self.assertEqual(loaded["x"].dtype, np.float32)
        np.testing.assert_allclose(loaded["x"][0, :2], [.6, .8])
        metrics = fit.counts(np.array([0, 0, 1, 1]), np.array([-1, 0, -1, 1], dtype=np.float32))
        self.assertEqual([metrics[k] for k in ("tp", "tn", "fp", "fn")], [1, 1, 1, 1])
        self.assertEqual(metrics["precision"], .5)
        self.assertEqual(metrics["recall"], .5)
        self.assertIsNone(fit.counts(np.array([0]), np.array([-1]))["precision"])

    def test_cli_separation_and_safe_error(self):
        script = str(Path(__file__).with_name("fit.py"))
        for args in (["fit", "--heldout", "synthetic-private-path"],
                     ["evaluate", "--head", "synthetic-private-path", "--features", "missing"]):
            run = subprocess.run([sys.executable, script, *args], capture_output=True, text=True)
            self.assertEqual(run.returncode, 2)
            self.assertEqual(run.stdout, "")
            self.assertNotIn("synthetic-private-path", run.stderr)
            self.assertNotIn("Traceback", run.stderr)
            self.assertIn("error", json.loads(run.stderr))

    def test_invalid_features(self):
        mutations = [lambda d: d.update(embedding_dim=383),
                     lambda d: d.update(input_version="wrong"),
                     lambda d: d.update(records=[]),
                     lambda d: d["records"][0].update(label=True),
                     lambda d: d["records"][0].update(family="bad family"),
                     lambda d: d["records"][0].update(embedding=[0.] * 384),
                     lambda d: d["records"][0]["embedding"].__setitem__(0, float("nan")),
                     lambda d: d["records"][0]["embedding"].__setitem__(0, True),
                     lambda d: d["records"][1].update(id="bad-0"),
                     lambda d: d["records"][1].update(index=0)]
        for mutate in mutations:
            with self.subTest(mutate=mutate):
                data = self.features("bad")
                mutate(data)
                with self.assertRaises(fit.Invalid):
                    fit.load_features(self.write("bad.json", data))

    def test_isolation_and_destinations(self):
        train = self.write("train.json", self.features("train", "same"))
        cal = self.write("cal.json", self.features("cal", "same"))
        with self.assertRaises(fit.Invalid):
            fit.fit_head(train, cal, self.root / "out", "a" * 64)
        with self.assertRaises(fit.Invalid):
            fit.check_destination(Path(__file__).resolve().parent / "head.json", [])
        with self.assertRaises(fit.Invalid):
            fit.check_destination(train, [train])
        with self.assertRaises(fit.Invalid):
            fit.check_destination(Path("relative.json"), [])
        nested = self.root / "synthetic-repo"
        nested.mkdir()
        (nested / ".git").write_text("synthetic worktree marker")
        with self.assertRaises(fit.Invalid):
            fit.check_destination(nested / "out.json", [])

    def test_report_clobber_and_id_overlap(self):
        train_data, cal_data = self.features("train"), self.features("cal")
        train = self.write("train.json", train_data)
        cal = self.write("cal.json", cal_data)
        head = self.root / "head.json"
        fit.fit_head(train, cal, head, "c" * 64)
        independent = self.write("independent.json", self.features("independent"))
        report = self.root / "report.json"
        fit.evaluate(head, independent, report)
        self.assertEqual(json.loads(report.read_text())["n"], 4)
        with self.assertRaises(fit.Invalid):
            fit.evaluate(head, independent, report)
        overlap = self.features("overlap")
        overlap["records"][0]["id"] = train_data["records"][0]["id"]
        overlapping = self.write("overlap.json", overlap)
        with self.assertRaises(fit.Invalid):
            fit.evaluate(head, overlapping)
        self.assertEqual(fit.evaluate(head, overlapping, regression=True)["kind"], "regression")
        with self.assertRaises(fit.Invalid):
            fit.fit_head(train, overlapping, self.root / "other-head.json", "c" * 64)

    def test_invalid_head(self):
        train = self.write("train.json", self.features("train"))
        cal = self.write("cal.json", self.features("cal"))
        out = self.root / "head.json"
        fit.fit_head(train, cal, out, "b" * 64)
        original = json.loads(out.read_text())
        for key, value in [("threshold", 1), ("bias", float("inf")),
                           ("coefficients", [0.] * 383), ("bias", True),
                           ("input_version", "wrong")]:
            data = dict(original)
            data[key] = value
            with self.assertRaises(fit.Invalid):
                fit.load_head(self.write("invalid-head.json", data))


if __name__ == "__main__":
    unittest.main()
