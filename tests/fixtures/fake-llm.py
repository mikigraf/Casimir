#!/usr/bin/env python3
"""Stand-in LLM for tests. Prompt on stdin; behaviour selected by CASIMIR_LLM_MODEL (e.g. "fake:judge-flip")."""
import json, os, sys, re, hashlib
prompt = sys.stdin.read()
mode = os.environ.get("CASIMIR_LLM_MODEL", "fake:default").split(":", 1)[-1]
system = os.environ.get("CASIMIR_LLM_SYSTEM", "")

def out(obj):
    print("Here you go:\n```json\n" + json.dumps(obj) + "\n```")

if "# User requests" in prompt:  # judge
    if mode == "judge-flip":  # always prefers whichever candidate is shown first
        out({"winner": "A", "scoreA": 8, "scoreB": 6, "summary": "first looked better", "differences": ["order"]})
    else:  # consistent: prefers the run whose block mentions the fake harness output
        a = prompt.split("## Run A")[1].split("## Run B")[0]
        winner = "A" if ("Working on" in a or "fake-model" in a) else "B"
        out({"winner": winner, "scoreA": 9 if winner == "A" else 4, "scoreB": 9 if winner == "B" else 4, "summary": "consistent", "differences": []})
elif "# Task" in prompt:  # user simulator
    m = re.search(r"Original text of that message:\n(.*?)(?:\n\n# Correction|$)", prompt, re.S)
    original = m.group(1).strip() if m else ""
    if mode == "sim-stop":
        out({"action": "stop", "message": "", "verbatim": False, "grounded_in": [], "reason": "nothing left to ask", "stop_reason": "out_of_scope", "memory": "stopped"})
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
