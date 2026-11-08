import concurrent.futures
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import runner
import windows_assignment as assignment


class AssignmentTests(unittest.TestCase):
    def test_canonical_assignment_is_exclusive_bound_and_cannot_be_replayed_from_a_copy(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            binary = root/'synthetic.exe'
            binary.write_bytes(b'synthetic executable')
            binding = dict(harness_sha256=runner.windows_harness_identity(),
                           binary_sha256=assignment.hashlib.sha256(binary.read_bytes()).hexdigest())
            grant = 'a'*32
            payload = dict(binding=binding, windows_run='C:\\synthetic\\run')
            with patch.object(assignment, 'artifact_root', return_value=root), \
                    patch.object(assignment.subprocess, 'run', return_value=SimpleNamespace(stdout=str(binary))):
                self.assertEqual(assignment.reserve(root, grant, payload), 12)
                for bad in (dict(binding, binary_sha256='0'*64), dict(binding, extra=True)):
                    with self.assertRaises(ValueError):
                        assignment.claim(root, grant, 'C:\\synthetic.exe', payload['windows_run'], bad)
                def claim(_):
                    try:
                        return assignment.claim(root, grant, 'C:\\synthetic.exe', payload['windows_run'], binding)
                    except (ValueError, FileExistsError, BlockingIOError):
                        return None
                with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
                    self.assertEqual(list(pool.map(claim, range(8))).count(12), 1)
                # A handoff copied to a new ID has no authoritative registration.
                original = root/'windows-assignments'/f'{grant}.json'
                copied = original.with_name('b'*32+'.json')
                copied.write_bytes(original.read_bytes())
                copied.chmod(0o600)
                self.assertIsNone(claim(0))
                with self.assertRaises(ValueError):
                    assignment.claim(root, 'b'*32, 'C:\\synthetic.exe', payload['windows_run'], binding)
                ledger = json.loads((root/'unified-app-bench-live-budget.json').read_text())
                self.assertEqual(ledger['reserved'], 12)

    def test_reservations_share_legacy_ceiling_and_uncertain_failure_is_never_refunded(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            binding = dict(harness_sha256=runner.windows_harness_identity())
            with patch.object(assignment, 'artifact_root', return_value=root):
                ledger = root/'unified-app-bench-live-budget.json'
                (root/'legacy').mkdir()
                runner.reserve_live_budget(ledger, root/'legacy/reservation.json')
                for index in range(7):
                    assignment.reserve(root, f'{index:032x}', dict(binding=binding, windows_run='C:\\synthetic'))
                self.assertEqual(json.loads(ledger.read_text())['reserved'], 96)
                with self.assertRaises(ValueError):
                    assignment.reserve(root, 'f'*32, dict(binding=binding, windows_run='C:\\synthetic'))
                self.assertEqual(json.loads(ledger.read_text())['reserved'], 96)
            other = root/'second'
            other.mkdir()
            with patch.object(assignment, 'artifact_root', return_value=other), \
                    patch.object(runner, 'private_json', side_effect=OSError('synthetic fsync failure')):
                with self.assertRaises(OSError):
                    assignment.reserve(root, 'e'*32, dict(binding=binding, windows_run='C:\\synthetic'))
            self.assertEqual(json.loads((other/'unified-app-bench-live-budget.json').read_text())['reserved'], 12)

    def test_unregistered_public_symlink_and_altered_binary_claims_are_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            binary = root/'synthetic.exe'
            binary.write_bytes(b'synthetic')
            binding = dict(harness_sha256=runner.windows_harness_identity(), binary_sha256='0'*64)
            with patch.object(assignment, 'artifact_root', return_value=root), \
                    patch.object(assignment.subprocess, 'run', return_value=SimpleNamespace(stdout=str(binary))):
                assignment.reserve(root, 'a'*32, dict(binding=binding, windows_run='synthetic'))
                with self.assertRaises(ValueError):
                    assignment.claim(root, 'a'*32, 'synthetic', 'synthetic', binding)
                path = root/'windows-assignments'/('a'*32+'.json')
                path.chmod(0o644)
                with self.assertRaises(ValueError): assignment.read_private(path)
                link = root/'linked.json'
                link.symlink_to(path)
                with self.assertRaises(ValueError): assignment.read_private(link)
