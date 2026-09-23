#!/usr/bin/env python3
"""Stand-in LLM for tests. Prompt on stdin; behaviour selected by CASIMIR_LLM_MODEL (e.g. "fake:judge-flip")."""
import json, os, sys, re, hashlib
prompt = sys.stdin.read()
mode = os.environ.get("CASIMIR_LLM_MODEL", "fake:default").split(":", 1)[-1]
system = os.environ.get("CASIMIR_LLM_SYSTEM", "")

def out(obj):
    print("Here you go:\n```json\n" + json.dumps(obj) + "\n```")

if "# Agent's final message in the original session" in prompt:  # brief drafting
    out({"objective": "Add a greet(name) function with a test", "constraints": ["keep changes in lib.py and test_lib.py"],
         "intervention_conditions": ["after the first implementation the user asked for a default argument (turn 2)"],
         "criteria": [{"id": "C1", "text": "greet(name) exists in lib.py and returns 'Hello, <name>!'", "must": True},
                      {"id": "C2", "text": "a test for greet exists", "must": True},
                      {"id": "C3", "text": "greet defaults to 'World' when called without a name", "must": False}],
         "intents": [{"id": "I1", "text": "add greet(name) to lib.py", "turn": 1}, {"id": "I2", "text": "add a test for greet", "turn": 1}, {"id": "I3", "text": "default name to World", "turn": 2}]})
elif "# Original intents" in prompt:  # intent coverage
    out({"covered": ["I3"], "in_scope": [0]})
elif "# User requests" in prompt:  # judge
    if mode == "judge-evidence":
        for label in ["## Run A", "## Run B"]:
            block = prompt.split(label)[1].split("## Run ")[0]
            assert "verification_evidence_marker" in block
            assert "verified content from actual tool result" in block
        out({"winner": "tie", "scoreA": 9, "scoreB": 9, "summary": "tool evidence present in both orders"})
    elif mode == "judge-malformed":
        out({"winner": "invalid", "scoreA": 99, "scoreB": 99})
    elif mode == "judge-both-pass":
        out({"winner": "tie", "scoreA": 9, "scoreB": 9, "summary": "both pass"})
    elif mode == "judge-invalid-patch":
        out({"winner": "tie", "scoreA": 9, "scoreB": 9, "invalidA": ["requirement_violation"], "invalidB": ["requirement_violation"]})
    elif mode == "judge-flip":  # always prefers whichever candidate is shown first
        out({"winner": "A", "scoreA": 8, "scoreB": 6, "summary": "first looked better", "differences": ["order"]})
    else:  # consistent: prefers the run whose block mentions the fake harness output
        a = prompt.split("## Run A")[1].split("## Run B")[0]
        winner = "A" if ("Working on" in a or "fake-model" in a) else "B"
        out({"winner": winner, "scoreA": 9 if winner == "A" else 4, "scoreB": 9 if winner == "B" else 4, "summary": "consistent", "differences": []})
elif "# Task" in prompt:  # user simulator
    m = re.search(r"Original text of that message:\n(.*?)(?:\n\n# Correction|$)", prompt, re.S)
    original = m.group(1).strip() if m else ""
    if mode == "sim-fail":
        print("deliberate simulator failure", file=sys.stderr)
        sys.exit(17)
    elif mode == "sim-malformed":
        out({})
    elif mode == "sim-false-verbatim":
        out({"action": "send", "message": "invented requirement", "verbatim": True, "grounded_in": [], "memory": "untrusted note"})
    elif mode == "sim-skip2":
        if "Produce the user's message for turn 2" in prompt:
            out({"action": "no_op", "reason": "already satisfied"})
        else:
            out({"action": "send", "message": "adapted third request", "grounded_in": [3], "verbatim": False, "kind": "redirect"})
    elif mode == "sim-noop":
        out({"action": "no_op", "kind": None, "message": "", "verbatim": False, "grounded_in": [], "reason": "already satisfied", "stop_reason": None, "memory": "skipped"})
    elif mode == "sim-adapt":
        out({"action": "send", "kind": "redirect", "message": "please use World as the default (see turn 2)", "verbatim": False, "grounded_in": [2], "reason": "adapted", "stop_reason": None, "memory": "asked for default"})
    elif mode in ("sim-stop", "sim-goals-met"):
        out({"action": "stop", "message": "", "verbatim": False, "grounded_in": [], "reason": "nothing left to ask", "stop_reason": "goals_met" if mode == "sim-goals-met" else "out_of_scope", "memory": "stopped"})
    elif mode == "sim-retry":
        # count calls via a file keyed by a stable prompt hash so parallel tests do not collide
        key = "/tmp/casimir-fake-llm-" + hashlib.md5(original.encode()).hexdigest()[:12] + ".count"
        n = int(open(key).read()) if os.path.exists(key) else 0
        if n < 2:
            open(key, "w").write(str(n + 1))
            out({"action": "send", "message": "adapted but ungrounded", "verbatim": False, "grounded_in": [], "reason": "oops", "memory": "note %d" % n})
        else:
            os.remove(key)
            out({"action": "send", "message": "adapted: " + original, "verbatim": False, "grounded_in": [2], "reason": "fits", "memory": "final note"})
    else:
        out({"action": "send", "message": original, "verbatim": True, "grounded_in": [2], "reason": "still applies", "memory": "sent verbatim"})
else:
    print("{}")
