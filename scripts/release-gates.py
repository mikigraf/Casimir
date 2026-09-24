#!/usr/bin/env python3
"""Validate redacted release evidence; optionally verify CI origins with GitHub."""
import argparse
import hashlib
import io
import json
import os
import pathlib
import re
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request
import zipfile

PLATFORMS = {'linux': 'ubuntu-latest', 'macos': 'macos-latest', 'windows': 'windows-latest'}
TARGETS = {'aarch64-apple-darwin', 'x86_64-apple-darwin', 'x86_64-unknown-linux-gnu', 'x86_64-pc-windows-msvc'}
CHECKS = {'cargo test --locked --all-targets', "python -m unittest discover -s tests -p test_*.py"}
STEPS = {'install', 'doctor', 'experiment', 'interpret', 'recover', 'cleanup'}


def validate(evidence, commit, stage, base):
    """Check the evidence itself, not only its manifest or file hashes.

    The optional online origin check below is required by the publishing workflow.
    This structural check alone cannot authenticate human attestations.
    """
    blockers = []
    seen = set()

    def require(ok, message):
        if not ok:
            blockers.append(message)

    def artifact(record, label, kind, platform=None, same_commit=True):
        if not isinstance(record, dict):
            blockers.append(label + ': evidence missing')
            return None
        require(record.get('status') == 'passed', label + ': not passed')
        name, digest = record.get('artifact'), record.get('sha256')
        if not isinstance(name, str) or not isinstance(digest, str) or not re.fullmatch(r'[0-9a-f]{64}', digest):
            blockers.append(label + ': artifact/hash missing or invalid')
            return None
        path = (base / name).resolve()
        if not path.is_relative_to(base.resolve()) or path.suffix != '.json':
            blockers.append(label + ': artifact must be JSON inside evidence directory')
            return None
        if path in seen:
            blockers.append(label + ': artifact reused for multiple gates')
        seen.add(path)
        if not path.is_file():
            blockers.append(label + ': artifact unavailable')
            return None
        contents = path.read_bytes()
        require(hashlib.sha256(contents).hexdigest() == digest, label + ': artifact hash mismatch')
        try:
            data = json.loads(contents)
        except (ValueError, UnicodeDecodeError):
            blockers.append(label + ': artifact is not valid JSON')
            return None
        if not isinstance(data, dict):
            blockers.append(label + ': artifact is not an object')
            return None
        require(data.get('schemaVersion') == 1 and data.get('redacted') is True, label + ': missing schema or redaction attestation')
        require(data.get('status') == 'passed', label + ': artifact did not pass')
        require(data.get('kind') == kind, label + ': wrong artifact kind')
        if same_commit:
            require(data.get('commit') == commit, label + ': artifact commit mismatch')
        if platform:
            require(data.get('platform') == platform, label + ': artifact platform mismatch')
        return data

    require(isinstance(evidence, dict) and evidence.get('schemaVersion') == 1, 'unsupported evidence schema')
    if not isinstance(evidence, dict):
        return blockers
    require(evidence.get('commit') == commit, 'evidence does not match the release commit')
    require(evidence.get('redacted') is True, 'manifest missing redaction attestation')
    reliability = evidence.get('reliability') or {}
    for platform, runner in PLATFORMS.items():
        data = artifact(reliability.get(platform), 'reliability ' + platform, 'reliability', platform)
        if not data:
            continue
        require(data.get('source') == 'GitHub Actions CI push workflow' and data.get('event') == 'push', 'reliability ' + platform + ': wrong source')
        require(str(data.get('runId', '')).isdigit(), 'reliability ' + platform + ': missing CI run ID')
        jobs = data.get('jobs') if isinstance(data.get('jobs'), list) else []
        for rust in ('1.85.0', 'stable'):
            name = f'test ({runner}, {rust})'
            matches = [job for job in jobs if isinstance(job, dict) and job.get('name') == name and job.get('conclusion') == 'success' and isinstance(job.get('jobId'), int) and job['jobId'] > 0]
            require(len(matches) == 1, 'reliability ' + platform + ': successful job missing: ' + name)
        require(CHECKS.issubset(set(data.get('checks') or [])), 'reliability ' + platform + ': required checks missing')
    if stage != '1.0':
        return blockers

    live = evidence.get('live') or {}
    for platform in PLATFORMS:
        data = artifact(live.get(platform), 'authenticated ' + platform, 'authenticated', platform)
        if not data:
            continue
        require(data.get('source') == 'GitHub Actions authenticated acceptance workflow' and str(data.get('runId', '')).isdigit(), 'authenticated ' + platform + ': protected workflow provenance missing')
        require(data.get('fixtureSubstitution') is False, 'authenticated ' + platform + ': fixture substitution')
        require(set(data.get('harnesses') or []) == {'claude-code', 'codex'}, 'authenticated ' + platform + ': both harnesses required')
        require(set(data.get('replayDirections') or []) == {'claude-code->codex', 'codex->claude-code'}, 'authenticated ' + platform + ': both replay directions required')
        require(set(data.get('workflows') or []) == {'replay', 'resume', 'checkpoint'}, 'authenticated ' + platform + ': native workflows missing')
        require(data.get('issues') == [], 'authenticated ' + platform + ': unresolved orchestration issues')
        require(data.get('nativeRecordsPassed') is True, 'authenticated ' + platform + ': native records incomplete')
        if platform == 'linux':
            records = data.get('records') or []
            expected = {(task, harness, replicate) for task in data.get('taskIds', []) for harness in ('claude-code', 'codex') for replicate in (1, 2)}
            actual = {(r.get('task'), r.get('harness'), r.get('replicate')) for r in records if isinstance(r, dict) and r.get('orchestration') == 'passed'}
            require(len(data.get('taskIds') or []) >= 10 and len(set(data.get('taskIds') or [])) == len(data.get('taskIds') or []) and expected.issubset(actual), 'authenticated linux: ten tasks, both harnesses and two real replicates required')

    evaluation = artifact(evidence.get('evaluation'), 'reviewed evaluation', 'evaluation')
    if evaluation:
        calibration = evaluation.get('calibration') or {}
        require(calibration.get('cases') == 40 and calibration.get('passed') is True and calibration.get('agreement', 0) >= 0.90 and calibration.get('requiredCheckViolations') == 0, 'reviewed evaluation: calibration thresholds not met')
        require(calibration.get('falsePositives') is not None and calibration.get('abstentions') is not None, 'reviewed evaluation: false positives or abstentions missing')
        reviewers = evaluation.get('reviewers') or []
        require(len(reviewers) >= 2 and len(set(reviewers)) == len(reviewers) and evaluation.get('humanReviewed') is True and evaluation.get('adjudicated') is True, 'reviewed evaluation: independent human review/adjudication missing')
        require(evaluation.get('corpusHash') == calibration.get('corpusHash'), 'reviewed evaluation: corpus hash mismatch')
    for kind in ('simulator', 'attribution'):
        data = artifact(evidence.get(kind), kind + ' reviewed cases', kind)
        if data:
            require(data.get('humanReviewed') is True and isinstance(data.get('reviewedCaseCount'), int) and data['reviewedCaseCount'] > 0 and data.get('failures') == [], kind + ': reviewed cases incomplete')
    pilots = evidence.get('pilots') or []
    require(len(pilots) >= 3 and len({p.get('userId') for p in pilots if isinstance(p, dict) and p.get('userId')}) >= 3, 'three independent pilot users required')
    for pilot in pilots:
        label = 'pilot ' + str(pilot.get('userId'))
        data = artifact(pilot, label, 'pilot')
        if data:
            require(data.get('userId') == pilot.get('userId') and data.get('independentHuman') is True and data.get('ownRepository') is True, label + ': independent own-repository attestation missing')
            require(all(data.get('steps', {}).get(step) == 'passed' for step in STEPS) and data.get('unresolvedIssues') == [], label + ': journey incomplete')
    require(evidence.get('unresolvedBlockers') == [], 'unresolved data-loss, incorrect-result or onboarding blockers')
    candidate = artifact(evidence.get('releaseCandidate'), 'release candidate', 'releaseCandidate', same_commit=False)
    if candidate:
        require(re.fullmatch(r'v1\.0\.0-rc\.[1-9][0-9]*', candidate.get('tag', '')) is not None, 'release candidate: invalid tag')
        require(re.fullmatch(r'[0-9a-f]{40}', candidate.get('commit', '')) is not None, 'release candidate: invalid commit')
        require(set(candidate.get('targets') or []) == TARGETS and candidate.get('checksumsVerified') is True and candidate.get('provenanceVerified') is True, 'release candidate: four native archives, checksums and provenance required')
    return blockers


def github_get(repo, route, token):
    request = urllib.request.Request('https://api.github.com/repos/' + repo + '/' + route,
                                     headers={'Authorization': 'Bearer ' + token, 'Accept': 'application/vnd.github+json', 'User-Agent': 'casimir-release-gates'})
    with urllib.request.urlopen(request, timeout=20) as response:
        return json.load(response)


def github_artifact(repo, artifact_id, token):
    """Read one small Actions receipt, dropping authorization on signed redirects."""
    class SafeRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, request, fp, code, message, headers, new_url):
            redirected = super().redirect_request(request, fp, code, message, headers, new_url)
            if redirected and not new_url.startswith('https://api.github.com/'):
                redirected.remove_header('Authorization')
            return redirected

    request = urllib.request.Request(f'https://api.github.com/repos/{repo}/actions/artifacts/{artifact_id}/zip',
                                     headers={'Authorization': 'Bearer ' + token, 'Accept': 'application/vnd.github+json', 'User-Agent': 'casimir-release-gates'})
    with urllib.request.build_opener(SafeRedirect()).open(request, timeout=20) as response:
        blob = response.read(1024 * 1024 + 1)
    if len(blob) > 1024 * 1024:
        raise ValueError('acceptance receipt archive exceeds 1 MiB')
    with zipfile.ZipFile(io.BytesIO(blob)) as archive:
        if archive.namelist() != ['acceptance-summary.json']:
            raise ValueError('unexpected acceptance receipt archive contents')
        return archive.read('acceptance-summary.json')


def verify_github(evidence, commit, stage, base, repo, token):
    """Authenticate CI/acceptance run and job IDs against the GitHub API."""
    blockers = []
    if not token or not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', repo or ''):
        return ['GitHub token and owner/repository are required for provenance verification']
    for kind, platforms, path, event in [('reliability', PLATFORMS, '.github/workflows/ci.yml', 'push'),
                                          ('live', PLATFORMS if stage == '1.0' else {}, '.github/workflows/acceptance.yml', 'workflow_dispatch')]:
        for platform in platforms:
            record = (evidence.get(kind) or {}).get(platform) or {}
            try:
                data = json.loads((base / record['artifact']).read_text())
                run_id = str(data['runId'])
                run = github_get(repo, 'actions/runs/' + run_id, token)
                if run.get('head_sha') != commit or run.get('path') != path or run.get('event') != event or run.get('conclusion') != 'success':
                    blockers.append(kind + ' ' + platform + ': GitHub run provenance mismatch')
                jobs = []
                page = 1
                while True:
                    response = github_get(repo, f'actions/runs/{run_id}/jobs?per_page=100&page={page}', token)
                    jobs.extend(response.get('jobs') or [])
                    if len(response.get('jobs') or []) < 100:
                        break
                    page += 1
                if kind == 'reliability':
                    for item in data['jobs']:
                        if not any(j.get('id') == item['jobId'] and j.get('name') == item['name'] and j.get('conclusion') == 'success' for j in jobs):
                            blockers.append(kind + ' ' + platform + ': GitHub job mismatch: ' + item['name'])
                elif not any(j.get('name') == f'live ({platform})' and j.get('conclusion') == 'success' for j in jobs):
                    blockers.append(kind + ' ' + platform + ': successful authenticated job missing')
                if kind == 'live':
                    response = github_get(repo, f'actions/runs/{run_id}/artifacts?per_page=100', token)
                    found = [a for a in response.get('artifacts') or [] if a.get('name') == 'authenticated-' + platform and a.get('expired') is False]
                    if len(found) != 1 or github_artifact(repo, found[0]['id'], token) != (base / record['artifact']).read_bytes():
                        blockers.append(kind + ' ' + platform + ': uploaded acceptance receipt mismatch')
            except (OSError, ValueError, KeyError, TypeError, zipfile.BadZipFile, urllib.error.URLError) as error:
                blockers.append(kind + ' ' + platform + ': GitHub provenance unavailable: ' + str(error))
    if stage == '1.0':
        try:
            candidate = json.loads((base / evidence['releaseCandidate']['artifact']).read_text())
            release = github_get(repo, 'releases/tags/' + candidate['tag'], token)
            assets = {asset['name'] for asset in release.get('assets') or []}
            for target in TARGETS:
                stem = 'casimir-' + candidate['tag'][1:] + '-' + target
                archive = stem + ('.zip' if 'windows' in target else '.tar.gz')
                if not {archive, archive + '.sha256', stem + '.installation.json'}.issubset(assets):
                    blockers.append('release candidate: missing GitHub assets for ' + target)
            if release.get('draft') is True or release.get('prerelease') is not True or release.get('target_commitish') != candidate['commit']:
                blockers.append('release candidate: GitHub release state or commit mismatch')
            if not blockers:
                blockers += verify_candidate_assets(candidate, repo)
        except (OSError, ValueError, KeyError, TypeError, urllib.error.URLError) as error:
            blockers.append('release candidate: GitHub provenance unavailable: ' + str(error))
    return blockers


def verify_candidate_assets(candidate, repo):
    """Verify published bytes, checksum, installation receipt and signed provenance."""
    blockers = []
    version = candidate['tag'][1:]
    with tempfile.TemporaryDirectory() as temporary:
        root = pathlib.Path(temporary)
        for target in TARGETS:
            stem = 'casimir-' + version + '-' + target
            archive = stem + ('.zip' if 'windows' in target else '.tar.gz')
            result = subprocess.run(['gh', 'release', 'download', candidate['tag'], '--repo', repo,
                                     '--dir', temporary, '--pattern', stem + '*'],
                                    capture_output=True, text=True, check=False)
            if result.returncode != 0:
                blockers.append('release candidate: asset download failed for ' + target)
                continue
            try:
                payload = root / archive
                digest = hashlib.sha256(payload.read_bytes()).hexdigest()
                require_line = digest + '  ' + archive + '\n'
                if (root / (archive + '.sha256')).read_text() != require_line:
                    blockers.append('release candidate: checksum mismatch for ' + target)
                installation = json.loads((root / (stem + '.installation.json')).read_text())
                if installation.get('schemaVersion') != 1 or installation.get('status') != 'passed' or installation.get('archive') != archive or installation.get('sha256') != digest or installation.get('version') != 'casimir ' + version or installation.get('paidCalls') is not False:
                    blockers.append('release candidate: installation receipt mismatch for ' + target)
                verified = subprocess.run(['gh', 'attestation', 'verify', str(payload), '--repo', repo,
                                           '--signer-workflow', repo + '/.github/workflows/release-candidate.yml',
                                           '--source-digest', candidate['commit']],
                                          capture_output=True, text=True, check=False)
                if verified.returncode != 0:
                    blockers.append('release candidate: signed provenance unavailable for ' + target)
            except (OSError, ValueError, TypeError, KeyError):
                blockers.append('release candidate: assets invalid for ' + target)
    return blockers


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--evidence', required=True, type=pathlib.Path)
    parser.add_argument('--commit', required=True)
    parser.add_argument('--stage', choices=['rc', '1.0'], required=True)
    parser.add_argument('--verify-github', action='store_true', help='authenticate run, job and release origins before publishing')
    args = parser.parse_args()
    try:
        evidence = json.loads(args.evidence.read_text())
        blockers = validate(evidence, args.commit, args.stage, args.evidence.parent)
        if args.verify_github and not blockers:
            blockers += verify_github(evidence, args.commit, args.stage, args.evidence.parent, os.environ.get('GITHUB_REPOSITORY'), os.environ.get('GH_TOKEN'))
    except (OSError, ValueError, TypeError, KeyError) as error:
        blockers = ['invalid or unavailable evidence: ' + str(error)]
    print(json.dumps({'schemaVersion': 1, 'commit': args.commit, 'stage': args.stage, 'passed': not blockers, 'blockers': blockers}, indent=2))
    return bool(blockers)


if __name__ == '__main__':
    sys.exit(main())
