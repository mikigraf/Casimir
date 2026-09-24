#!/usr/bin/env python3
"""Publish a minimal, redacted receipt from real authenticated acceptance runs."""
import argparse
import json
import os
import pathlib
import platform
import sys


def summarize(acceptance, native, commit, run_id, expected_platform):
    source = json.loads(pathlib.Path(acceptance).read_text(encoding='utf-8'))
    recovery = json.loads(pathlib.Path(native).read_text(encoding='utf-8'))
    if not run_id.isdigit() or not commit or expected_platform not in ('linux', 'macos', 'windows'):
        raise ValueError('GitHub run ID, commit and supported platform required')
    for label, data in [('task acceptance', source), ('native workflows', recovery)]:
        if data.get('schemaVersion') != 1 or data.get('commit') != commit or data.get('platform') != expected_platform or data.get('status') != 'passed' or data.get('fixtureSubstitution') is not False:
            raise ValueError(label + ' failed or does not match this runner and commit')
    if set(source.get('harnesses') or []) != {'claude-code', 'codex'} or set(source.get('replayDirections') or []) != {'claude-code->codex', 'codex->claude-code'}:
        raise ValueError('both harnesses and cross-replay directions required')
    if set(recovery.get('harnesses') or []) != {'claude-code', 'codex'} or set(recovery.get('workflows') or []) != {'replay', 'resume', 'checkpoint'}:
        raise ValueError('both harnesses and all native workflows required')
    native_records = recovery.get('records') or []
    if len(native_records) != 2 or {r.get('harness') for r in native_records} != {'claude-code', 'codex'} or not all(r.get('sourceUnchanged') is True and all(r.get(step) == 'passed' for step in ('replay', 'checkpoint', 'resume')) for r in native_records):
        raise ValueError('native recovery or checkpoint record failed')
    records = [{key: item[key] for key in ('task', 'harness', 'replicate', 'orchestration', 'taskOutcome')} for item in source.get('records') or []]
    tasks = sorted({r['task'] for r in records})
    expected = {(task, harness, replicate) for task in tasks for harness in ('claude-code', 'codex') for replicate in (1, 2)}
    actual = {(r['task'], r['harness'], r['replicate']) for r in records if r['orchestration'] == 'passed'}
    if len(tasks) != 10 or len(records) != 40 or actual != expected or source.get('issues') != []:
        raise ValueError('ten tasks times two harnesses and replicates must pass orchestration')
    return {'schemaVersion': 1, 'redacted': True, 'status': 'passed', 'kind': 'authenticated',
            'source': 'GitHub Actions authenticated acceptance workflow', 'runId': run_id,
            'commit': commit, 'platform': expected_platform, 'fixtureSubstitution': False,
            'taskIds': tasks, 'harnesses': source['harnesses'], 'replayDirections': source['replayDirections'],
            'workflows': recovery['workflows'], 'nativeRecordsPassed': True, 'issues': [], 'records': records}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--acceptance', type=pathlib.Path, required=True)
    parser.add_argument('--native', type=pathlib.Path, required=True)
    parser.add_argument('--output', type=pathlib.Path, required=True)
    args = parser.parse_args()
    receipt = summarize(args.acceptance, args.native, os.environ.get('GITHUB_SHA', ''), os.environ.get('GITHUB_RUN_ID', ''), platform.system().lower())
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(receipt, indent=2) + '\n', encoding='utf-8')


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, KeyError, TypeError) as error:
        print('authenticated summary unavailable: ' + str(error), file=sys.stderr)
        sys.exit(1)
