"""The published receipt retains outcomes but never private acceptance paths."""
import importlib.util
import json
import pathlib
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('acceptance_summary', pathlib.Path(__file__).parents[1] / 'scripts/summarize-acceptance.py')
summary = importlib.util.module_from_spec(spec)
spec.loader.exec_module(summary)


class AcceptanceSummary(unittest.TestCase):
    def test_full_run_is_redacted_and_incomplete_native_workflow_is_refused(self):
        tasks = [f'task-{number}' for number in range(10)]
        records = [{'task': task, 'harness': harness, 'replicate': replicate,
                    'orchestration': 'passed', 'taskOutcome': 'inconclusive',
                    'run': '/private/transcripts/do-not-publish'}
                   for task in tasks for harness in ('claude-code', 'codex') for replicate in (1, 2)]
        acceptance = {'schemaVersion': 1, 'status': 'passed', 'commit': 'commit', 'platform': 'linux',
                      'fixtureSubstitution': False, 'harnesses': ['claude-code', 'codex'],
                      'replayDirections': ['claude-code->codex', 'codex->claude-code'],
                      'records': records, 'issues': []}
        native = {'schemaVersion': 1, 'status': 'passed', 'commit': 'commit', 'platform': 'linux',
                  'fixtureSubstitution': False, 'harnesses': ['claude-code', 'codex'],
                  'workflows': ['replay', 'resume', 'checkpoint'],
                  'records': [{'harness': harness, 'sourceUnchanged': True,
                               'replay': 'passed', 'resume': 'passed', 'checkpoint': 'passed'}
                              for harness in ('claude-code', 'codex')]}
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            a, n = root / 'acceptance.json', root / 'native.json'
            a.write_text(json.dumps(acceptance)); n.write_text(json.dumps(native))
            receipt = summary.summarize(a, n, 'commit', '123', 'linux')
            self.assertEqual(len(receipt['records']), 40)
            self.assertNotIn('/private/', json.dumps(receipt))
            native['records'][0]['checkpoint'] = 'failed'
            n.write_text(json.dumps(native))
            with self.assertRaisesRegex(ValueError, 'native recovery'):
                summary.summarize(a, n, 'commit', '123', 'linux')


if __name__ == '__main__':
    unittest.main()
