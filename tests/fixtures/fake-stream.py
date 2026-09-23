#!/usr/bin/env python3
"""Headless Copilot/Gemini protocol fixture, including empty/truncated responses."""
import json
from pathlib import Path
import sys

args = sys.argv[1:]
def arg(name, default=None):
    return args[args.index(name) + 1] if name in args else default

def emit(kind, **fields):
    print(json.dumps(dict(type=kind, **fields)), flush=True)

mode = arg("--fake-mode", "success")
if mode == "empty":
    sys.exit(0)
if mode == "bare-result":
    emit("result", is_error=False)
    sys.exit(0)
if mode == "failed":
    emit("error", message="fixture failure")
    sys.exit(17)
if mode == "truncated":
    emit("unknown", message="partial output")
    sys.exit(0)

gemini = arg("--output-format") == "stream-json"
state = Path(".fake-session")
sid = arg("--resume") if gemini else arg("--session-id")
if state.exists():
    assert sid == state.read_text(), "second turn must resume the original session"
else:
    sid = sid or "fake-gemini-session"
    state.write_text(sid)
prompt = arg("-p", "")
Path("out.txt").write_text(prompt)
if gemini:
    emit("init", session_id=sid, model="fake-gemini")
    emit("message", role="assistant", content="Handled: " + prompt, delta=True)
    emit("result", status="error" if mode == "result-error" else "success",
         stats=dict(input=100, output_tokens=30, cached=40))
else:
    emit("session.start", data=dict(sessionId=sid, selectedModel="fake-copilot"))
    emit("user.message", data=dict(content=prompt))
    emit("assistant.message", data=dict(content="Handled: " + prompt))
    emit("session.shutdown", data=dict(modelMetrics={"fake-copilot":dict(inputTokens=100, outputTokens=30)}))
