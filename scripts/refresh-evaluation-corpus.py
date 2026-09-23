#!/usr/bin/env python3
"""Maintainer-only corpus refresh. Real check outputs; no human labels or review attestations."""
import argparse
import hashlib
import json
import pathlib
import subprocess
import sys
import tempfile


def refresh(corpus_path, checks_path):
    corpus = json.loads(corpus_path.read_text(encoding='utf-8'))
    checks = json.loads(checks_path.read_text(encoding='utf-8'))['checks']
    for index, case in enumerate(corpus['cases']):
        domain = index % 10
        source = checks[domain]['source']
        # Make the scope decidable without guessing missing application requirements.
        if domain == 0:
            case['task'] = 'Return all pages including an exact final page; reject nonpositive page sizes.'
        if domain == 4:
            case['task'] = 'Remove duplicate hashable records while preserving first occurrence order and input immutability.'
        if domain == 6:
            case['task'] = 'Migrate the included local formatter dependency to render(value, *, compact=False), preserving compact JSON output.'
        if domain == 7:
            case['task'] = 'Extract whitespace stripping and case-folding into normalize(value); names(values) must preserve input order, empty strings and Unicode behavior without mutating input.'
        if domain == 8:
            case['task'] = 'Accept integer ages 0 through 130 inclusive; reject booleans, nonintegers and out-of-range ages, and add executable boundary tests.'
        if domain == 9:
            case['task'] = 'The async work(task) helper must return completed task results and propagate CancelledError for cancelled tasks, never returning success for cancellation.'
        for side in ['A', 'B']:
            trace = case['trace' + side]
            if trace['files'] is None:
                continue  # Intentionally missing evidence remains missing.
            trace['files']['tests/test_task.py'] = source
            if domain == 6:
                trace['files']['formatter_v2.py'] = 'import json\ndef render(value, *, compact=False):\n    return json.dumps(value, separators=(",", ":") if compact else None)\n'
            with tempfile.TemporaryDirectory(prefix='casimir-corpus-') as temporary:
                root = pathlib.Path(temporary)
                workspace = root / 'workspace'
                workspace.mkdir()
                for name, content in trace['files'].items():
                    path = workspace / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_text(content, encoding='utf-8')
                frozen = root / 'frozen_checks.py'
                frozen.write_text(source, encoding='utf-8')
                result = subprocess.run([sys.executable, str(frozen)], cwd=workspace, capture_output=True, text=True, timeout=10)
                output = (result.stdout + result.stderr).replace(str(workspace), '<workspace>').replace(str(frozen), '<frozen-checks>')
                trace['checkDefinition'] = {'executable': 'python', 'args': ['<frozen-checks>'], 'source': source, 'sha256': hashlib.sha256(source.encode()).hexdigest(), 'expectedExitStatus': 0, 'timeoutSecs': 10}
                trace['toolEvidence'] = [{'command': 'python <frozen-checks>', 'exitStatus': result.returncode, 'output': output}]
                trace['snapshotProvenance'] = 'Final files in the synthetic workspace after executing the frozen external checks; no Git commit or model-authored trajectory claimed.'
                case['requiredCheckFailed' + side] = result.returncode != 0
                if side == 'A' and result.returncode != 0:
                    raise ValueError('reference program failed its specified checks: ' + case['id'] + '\n' + output)
    corpus['revision'] = 2
    corpus['provenance'] = 'Synthetic final workspace snapshots with actual executable-check outputs captured by scripts/refresh-evaluation-corpus.py; not live-provider traces or adjudicated labels.'
    corpus['reviewStatus'] = 'pending-independent-human-review'
    data = (json.dumps(corpus, indent=2, ensure_ascii=False) + '\n').encode()
    corpus_path.write_bytes(data)
    corpus_path.with_suffix('.sha256').write_text(hashlib.sha256(data).hexdigest() + '  ' + corpus_path.name + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--corpus', type=pathlib.Path, default=pathlib.Path('acceptance/evaluation/corpus.json'))
    parser.add_argument('--checks', type=pathlib.Path, default=pathlib.Path('acceptance/evaluation/checks.json'))
    parser.add_argument('--refresh', action='store_true', help='Invalidates previous predictions/reviews by changing the corpus hash')
    args = parser.parse_args()
    if not args.refresh:
        parser.error('explicit --refresh is required; never silently replace a frozen reviewed corpus')
    refresh(args.corpus, args.checks)
