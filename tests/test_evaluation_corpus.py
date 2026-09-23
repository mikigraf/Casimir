"""Re-execute frozen synthetic evidence; this is not independent human calibration."""
import hashlib
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest


class EvaluationEvidence(unittest.TestCase):
    def test_frozen_candidate_has_unique_cases_and_matching_hash(self):
        path = pathlib.Path(__file__).parents[1] / 'acceptance/evaluation/corpus.json'
        data = path.read_bytes()
        corpus = json.loads(data)
        self.assertEqual(hashlib.sha256(data).hexdigest(), path.with_suffix('.sha256').read_text().split()[0])
        self.assertEqual(len({case['id'] for case in corpus['cases']}), 40)
        self.assertEqual(corpus['reviewStatus'], 'pending-independent-human-review')

    def test_recorded_check_statuses_match_executable_snapshots(self):
        corpus = json.loads((pathlib.Path(__file__).parents[1] / 'acceptance/evaluation/corpus.json').read_text(encoding='utf-8'))
        for case in corpus['cases']:
            for side in ['A', 'B']:
                with self.subTest(case=case['id'], side=side):
                    trace = case['trace' + side]
                    if trace['files'] is None:
                        self.assertIsNone(trace['toolEvidence'][0]['exitStatus'])
                        continue
                    definition = trace['checkDefinition']
                    self.assertEqual(hashlib.sha256(definition['source'].encode()).hexdigest(), definition['sha256'])
                    with tempfile.TemporaryDirectory() as temporary:
                        root = pathlib.Path(temporary)
                        workspace = root / 'workspace'
                        workspace.mkdir()
                        for name, content in trace['files'].items():
                            path = workspace / name
                            self.assertTrue(path.resolve().is_relative_to(workspace))
                            path.parent.mkdir(parents=True, exist_ok=True)
                            path.write_text(content, encoding='utf-8')
                        checker = root / 'check.py'
                        checker.write_text(definition['source'], encoding='utf-8')
                        result = subprocess.run([sys.executable, str(checker)], cwd=workspace, capture_output=True, timeout=10)
                        self.assertEqual(result.returncode, trace['toolEvidence'][0]['exitStatus'])
                        self.assertEqual(result.returncode != definition['expectedExitStatus'], case['requiredCheckFailed' + side])


if __name__ == '__main__':
    unittest.main()
