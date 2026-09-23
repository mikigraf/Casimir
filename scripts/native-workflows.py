#!/usr/bin/env python3
"""Authenticated replay, checkpoint and interrupted-resume gate; never a fixture substitute."""
import argparse
import json
import pathlib
import platform
import subprocess
import sys
import time

from acceptance import coding_permission_args, command, permission_args, valid_report, write


def read(path):
    return json.loads(path.read_text(encoding='utf-8'))


def interrupt_second_turn(argv, run, log, deadline=1800):
    with log.open('wb') as stream:
        child = subprocess.Popen(argv, stdout=stream, stderr=subprocess.STDOUT)
        until = time.monotonic() + deadline
        try:
            while child.poll() is None and time.monotonic() < until:
                try:
                    journal = read(run / 'recovery.json')
                except (OSError, ValueError):
                    journal = {}
                if journal.get('active') and journal.get('nextTurn') == 2:
                    child.terminate()
                    child.wait(timeout=30)
                    return True
                time.sleep(0.02)
            return False
        finally:
            if child.poll() is None:
                child.kill()
                child.wait(timeout=30)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--casimir', type=pathlib.Path, required=True)
    parser.add_argument('--output', type=pathlib.Path, required=True)
    parser.add_argument('--harness', action='append', choices=['claude-code', 'codex'], help='Subset for compatibility validation; releases require both')
    parser.add_argument('--claude-permission-mode', choices=['preserve', 'acceptEdits'], default='preserve')
    parser.add_argument('--allow-subscription-usage', '--allow-paid', dest='allow_paid', action='store_true')
    parser.add_argument('--allow-unrestricted', action='store_true')
    args = parser.parse_args()
    if not args.allow_paid:
        parser.error('authenticated acceptance requires explicit --allow-subscription-usage')
    binary = str(args.casimir.resolve())
    root = args.output.resolve()
    if root.exists() and any(root.iterdir()):
        parser.error('output must be empty')
    root.mkdir(parents=True, exist_ok=True)
    harnesses = args.harness or ['claude-code', 'codex']
    doctor = json.loads(command([binary, 'doctor', '--json']).stdout)
    write(root / 'doctor.json', doctor)
    missing = [h['id'] for h in doctor['harnesses'] if h['id'] in harnesses and not (h.get('subscriptionReady') and h.get('runtimeReady'))]
    if missing:
        write(root / 'native-workflows.json', {'schemaVersion': 1, 'status': 'blocked', 'missingAuthentication': missing, 'fixtureSubstitution': False})
        return 1
    tasks = read(pathlib.Path(__file__).resolve().parents[1] / 'acceptance/live/tasks.json')['tasks']
    task = next(t for t in tasks if len(t['prompts']) >= 2)
    records = []
    for harness in harnesses:
        directory = root / harness
        repo = directory / 'source'
        repo.mkdir(parents=True)
        for name, content in task['files'].items():
            path = repo / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content, encoding='utf-8')
        for argv in [['init', '-q'], ['add', '.'], ['-c', 'user.name=acceptance', '-c', 'user.email=acceptance@example.invalid', 'commit', '-qm', 'baseline']]:
            command(['git', *argv], repo)
        source = directory / 'input.json'
        write(source, {'schemaVersion': 1, 'id': task['id'], 'harness': harness, 'cwd': str(repo), 'gitCommit': command(['git', 'rev-parse', 'HEAD'], repo).stdout.strip(), 'events': [{'turn': i + 1, 'ts': '', 'kind': 'user', 'text': text} for i, text in enumerate(task['prompts'])]})
        checker = directory / 'checker.py'
        checker.write_text(task['checker'], encoding='utf-8')
        checks = directory / 'checks.json'
        write(checks, {'schemaVersion': 1, 'checks': [{'executable': sys.executable, 'args': [str(checker)], 'timeoutSecs': 60, 'expectedExitStatus': 0}]})
        options = ['--replicates', '1', '--harness', harness, '--workspace', str(repo), '--checks', str(checks), '--quiet', *permission_args(harness, args.allow_unrestricted)]
        if harness == 'codex' and not args.allow_unrestricted:
            options += coding_permission_args(harness, False)
        if harness == 'claude-code' and args.claude_permission_mode != 'preserve' and not args.allow_unrestricted:
            options += ['--permission-mode', args.claude_permission_mode]
        normal = directory / 'replay'
        result = command([binary, 'rerun', str(source), *options, '-o', str(normal)], output=directory / 'replay.log')
        replay_ok = (normal / 'report.json').exists() and valid_report(result.returncode, read(normal / 'report.json'))
        record = {'harness': harness, 'replay': 'passed' if replay_ok else 'failed', 'checkpoint': 'blocked', 'resume': 'blocked'}
        if replay_ok:
            fork = directory / 'fork'
            result = command([binary, 'fork', str(normal), '--at-turn', '2', *options, '-o', str(fork)], output=directory / 'fork.log')
            fork_ok = (fork / 'report.json').exists() and valid_report(result.returncode, read(fork / 'report.json'))
            if fork_ok:
                record['checkpoint'] = 'passed'
            else:
                record['checkpointReason'] = 'Checkpoint workflow failed or pinned native format is not yet validated; inspect private fork.log.'
            interrupted = directory / 'interrupted'
            if interrupt_second_turn([binary, 'rerun', str(source), *options, '-o', str(interrupted)], interrupted, directory / 'interrupted.log'):
                refused = command([binary, 'resume', str(interrupted)], output=directory / 'refused.log')
                # Refusal must not reserve an attempt or send a prompt.
                refused_ok = refused.returncode != 0 and not read(interrupted / 'recovery.json').get('resumedAs')
                retried = command([binary, 'resume', str(interrupted), '--retry-interrupted'], output=directory / 'retried.log')
                attempt_path = read(interrupted / 'recovery.json').get('resumedAs')
                if refused_ok and attempt_path:
                    attempt = pathlib.Path(attempt_path)
                    if (attempt / 'report.json').exists() and valid_report(retried.returncode, read(attempt / 'report.json')):
                        session = read(attempt / 'session.json')
                        users = [e for e in session['events'] if e['kind'] == 'user' and not e.get('sidechain')]
                        no_repeat = len(users) == len(task['prompts']) and not (attempt / 'turns/1').exists()
                        before = (attempt / 'session.json').read_bytes()
                        noop = command([binary, 'resume', str(attempt)], output=directory / 'completed-noop.log')
                        if no_repeat and noop.returncode == 0 and (attempt / 'session.json').read_bytes() == before:
                            record['resume'] = 'passed'
        record['sourceUnchanged'] = not command(['git', 'status', '--porcelain'], repo).stdout.strip()
        records.append(record)
        write(root / 'progress.json', records)
    passed = all(r['sourceUnchanged'] and all(r[w] == 'passed' for w in ['replay', 'resume', 'checkpoint']) for r in records)
    write(root / 'native-workflows.json', {'schemaVersion': 1, 'commit': command(['git', 'rev-parse', 'HEAD']).stdout.strip(), 'platform': platform.system().lower(), 'status': 'passed' if passed else 'failed', 'workflows': ['replay', 'resume', 'checkpoint'], 'harnesses': harnesses, 'records': records, 'fixtureSubstitution': False})
    return int(not passed)


if __name__ == '__main__':
    sys.exit(main())
