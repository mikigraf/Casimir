#!/usr/bin/env python3
"""Stage only explicitly redacted JSON evidence from a protected runner for release gating."""
import argparse
import importlib.util
import json
import pathlib
import shutil

spec = importlib.util.spec_from_file_location('release_gates', pathlib.Path(__file__).with_name('release-gates.py'))
gates = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gates)


def prepare(source, destination, commit, stage):
    source = source.resolve()
    metadata = (source / 'release-evidence.json').read_bytes()
    evidence = json.loads(metadata)
    blockers = gates.validate(evidence, commit, stage, source)
    if blockers:
        raise ValueError('; '.join(blockers))
    if evidence.get('redacted') is not True:
        raise ValueError('release manifest must explicitly attest redacted: true')
    selected = set()

    def visit(value):
        if isinstance(value, dict):
            if 'artifact' in value:
                path = (source / value['artifact']).resolve()
                if not path.is_relative_to(source) or path.suffix != '.json':
                    raise ValueError('only JSON artifacts inside the evidence directory may be uploaded')
                document = json.loads(path.read_text(encoding='utf-8'))
                if document.get('redacted') is not True:
                    raise ValueError('artifact lacks explicit redaction attestation: ' + str(path.relative_to(source)))
                selected.add(path)
            for child in value.values():
                visit(child)
        elif isinstance(value, list):
            for child in value:
                visit(child)
    visit(evidence)
    if destination.exists():
        raise ValueError('evidence staging output must be fresh')
    destination.mkdir(parents=True)
    for path in selected:
        target = destination / path.relative_to(source)
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, target)
    (destination / 'release-evidence.json').write_bytes(metadata)
    blockers = gates.validate(evidence, commit, stage, destination)
    if blockers:
        raise ValueError('staged evidence failed verification: ' + '; '.join(blockers))


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--source', type=pathlib.Path, required=True)
    parser.add_argument('--output', type=pathlib.Path, required=True)
    parser.add_argument('--commit', required=True)
    parser.add_argument('--stage', choices=['rc', '1.0'], required=True)
    args = parser.parse_args()
    prepare(args.source, args.output, args.commit, args.stage)
