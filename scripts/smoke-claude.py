#!/usr/bin/env python3
"""Opt-in live verification. Requires Claude login; makes billable model calls.

Run after cargo build --release. All repositories, worktrees, and reports are kept
under --output for inspection. Native transcripts remain in Claude's log store.
"""
import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import subprocess
import uuid


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/release/casimir"))
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--model", default="sonnet")
    parser.add_argument("--resume", action="store_true", help="reuse completed artifacts and continue missing stages")
    args = parser.parse_args()
    binary = args.binary.resolve()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=args.resume)
    repo = output / "repo"
    repo.mkdir(exist_ok=args.resume)
    env = dict(os.environ, CASIMIR_HOME=str(output / "casimir"))

    def git(*argv):
        return subprocess.check_output(["git", "-C", str(repo), "-c", "user.name=Casimir",
                                        "-c", "user.email=casimir@example.invalid", *argv], text=True).strip()

    if not args.resume:
        git("init", "-q")
        (repo / "README.md").write_text("Live smoke test\n")
        git("add", "README.md")
        git("commit", "-qm", "Seed repository")
    now = datetime.now(timezone.utc).isoformat()
    prompts = [
        'Work only in the current working directory. Create greeting.txt containing exactly hello '
        'followed by a newline. Stage and commit only that file with git -c user.name=Casimir '
        '-c user.email=casimir@example.invalid commit -m "Create greeting". Reply done.',
        'Work only in the current working directory. Append world followed by a newline to greeting.txt, '
        'preserving the existing hello line. Verify it contains exactly hello and world on two lines. '
        'Do not commit the second change or modify other files. Reply done.',
    ]
    source = output / "source.json"
    if args.resume:
        assert json.loads(source.read_text())["model"] == args.model, "resume must use the same model"
    else:
        source.write_text(json.dumps(dict(id=str(uuid.uuid4()), harness="claude-code", model=args.model,
            cwd=str(repo), gitCommit=git("rev-parse", "HEAD"), startedAt=now,
            events=[dict(kind="user", turn=i, ts=now, text=p) for i, p in enumerate(prompts, 1)])))
    harness_args = ["--", "--max-turns", "12", "--tools", "Read,Write,Edit,Bash", "--setting-sources", ""]

    def run(name, *argv):
        dest = output / name
        if args.resume and any((dest / f).exists() for f in ["report.json", "replicates.json", "attribution.json"]):
            print(f"Reusing completed {name}: {dest}", flush=True)
            return dest
        print(f"Running {name}; log: {output / (name + '.log')}", flush=True)
        with (output / (name + ".log")).open("w") as log:
            subprocess.run([str(binary), *map(str, argv), "--replicates", "1", "--quiet", "-o", str(dest),
                            *harness_args], env=env, stdout=log, stderr=subprocess.STDOUT,
                           check=True, timeout=240)
        return dest

    def load(path):
        return json.loads(path.read_text())

    def verify_run(path, expected, preserved=0):
        session = load(path / "session.json")
        execution = session["execution"]
        assert execution["failedTurns"] == 0, execution
        assert execution["preservedTurns"] == preserved, execution
        assert execution["completedTurns"] == 2 - preserved, execution
        assert (Path(session["cwd"]) / "greeting.txt").read_text() == expected, path
        assert session.get("harnessLogPath"), "native log must be found"
        for artifact in ["record.jsonl", "raw.jsonl", "diff.patch", "report.json", "report.md"]:
            assert (path / artifact).is_file(), artifact
        return session

    original = run("original", "rerun", source, "--model", args.model)
    verify_run(original, "hello\nworld\n")
    assert "+hello\n+world\n" in (original / "diff.patch").read_text()
    fork = run("fork", "fork", original, "--at-turn", "2", "--message",
        'Work only in the current working directory. Verify greeting.txt already contains exactly hello '
        'followed by a newline; if it does not, report failure and stop. Append forked followed by a '
        'newline, preserving hello. Verify the two lines. Do not commit or modify other files. Reply done.')
    verify_run(fork, "hello\nforked\n", preserved=1)
    assert load(fork / "meta.json")["workspace"]["restored"] == "commits"
    verify_run(original, "hello\nworld\n")
    assert (fork / "original.patch").read_bytes() == (original / "diff.patch").read_bytes()

    judged = ["--judge", "--judge-model", args.model, "--judge-llm", "claude-cli"]
    controls = run("controls", "rerun", original, "--control", *judged)
    matrix = load(controls / "replicates.json")
    assert len(matrix["entries"]) == 2
    for entry in matrix["entries"]:
        assert entry["errors"] == 0 and entry["completedTurns"] == 2 and entry["controlModelVerified"]
        judgement = load(controls / entry["label"] / "report.json")["judge"]
        assert len(judgement["passes"]) == 2
        # A correct experiment may find that a model missed a requirement. Check that the
        # reported outcome matches its evidence, rather than requiring all models to pass.
        expected_pass = judgement["scoreB"] >= 7 and not judgement["invalidB"]
        assert entry["pass"] == expected_pass
    for name in ["control-r1", "r1"]:
        verify_run(controls / name, "hello\nworld\n")
    simulated = run("simulated", "rerun", original, "--user", "simulate", "--sim-model", args.model,
                    "--sim-llm", "claude-cli", "--brief", controls / "brief.json", *judged)
    verify_run(simulated, "hello\nworld\n")
    report = load(simulated / "report.json")
    assert len(report["judge"]["passes"]) == 2 and 0 <= report["judge"]["scoreB"] <= 10
    assert report["intentCoverage"] is not None

    attribution = run("attribution", "attribute", original, "--turns-at", "2",
                      "--brief", controls / "brief.json", *judged)
    if load(attribution / "turn2" / "report.json")["judge"]["scoreA"] >= 7:
        assert load(attribution / "attribution.json")["pointOfCommitment"] is None, "successful original is not a rescue"
    verify_run(attribution / "turn2", "hello\nworld\n", preserved=1)
    (output / "verified.json").write_text(json.dumps(dict(model=args.model, passed=True,
        model_outcomes=[dict(label=e["label"], passed=e["pass"], score=e["judgeScore"]) for e in matrix["entries"]],
        checks=["resume", "committed fork", "source unchanged", "saved reference", "controls",
                "AB/BA judge", "simulator", "intent coverage", "attribution withholding"]), indent=2))
    print(f"Live checks passed. Artifacts: {output}")


if __name__ == "__main__":
    main()
