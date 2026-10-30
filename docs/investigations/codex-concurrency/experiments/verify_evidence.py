#!/usr/bin/env python3
"""Check narrow claims against retained structured experiment evidence, not product parity."""
import json
from pathlib import Path

evidence = Path(__file__).resolve().parent.parent / "evidence"
checks = []


def load(name):
    return json.loads((evidence / name).read_text())


def check(name, condition):
    assert condition, name
    checks.append(name)


for name in ["stdio-same-project.json", "stdio-across-projects.json"]:
    result = load(name)
    cases = {x["name"]: x["response"] for x in result["cases"]}
    check(name + ": real native roots differ", cases["A_start"]["result"]["thread"]["id"] !=
          cases["B_start"]["result"]["thread"]["id"])
    check(name + ": competing writer refused", "already has an active writer" in
          cases["B_resume_A_while_A_live"]["error"]["message"])
    check(name + ": independent actor can fork source", cases["B_fork_A"]["result"]["thread"]["forkedFromId"] ==
          cases["A_start"]["result"]["thread"]["id"])
    check(name + ": resume after owner crash works", "result" in cases["B_resume_A_after_A_SIGKILL"])
    check(name + ": neighbor remains readable", "result" in cases["B_read_B_after_A_SIGKILL"])

r = load("watcher-misattribution.json")
check("unchanged watcher misattributes a neighbor", r["outcomes"][1]["events"] ==
      ['ThreadTransitioned("thread-0")', "UsageLimitExceeded"])
r = load("watcher-historical-fork.json")
check("unchanged watcher adopts an unchanged historical fork", r["outcomes"][0]["events"] ==
      ['ThreadTransitioned("old-fork")'])
r = load("watcher-targeted-isolation.json")
check("cost prototype isolates exact supplied identity", r["outcomes"][0]["events"] ==
      ["UsageLimitExceeded"] and r["outcomes"][1]["events"] == [])

r = load("tui-embedded-transitions.json")
s = {x["label"]: x for x in r["snapshots"]}
check("no initial SessionStart before first prompt", s["startup_before_any_prompt"]["hooks"] == [])
check("fork start hook deferred until prompt", not any(x["event"].get("source") == "fork"
      for x in s["after_fork_before_prompt"]["hooks"]) and any(x["event"].get("source") == "fork"
      for x in s["after_fork_prompt"]["hooks"]))
check("resume start hook deferred until prompt", not any(x["event"].get("source") == "resume"
      for x in s["after_resume_before_prompt"]["hooks"]) and any(x["event"].get("source") == "resume"
      for x in s["after_resume_prompt"]["hooks"]))
check("embedded hook launch markers differ", {x["marker"] for x in r["hooks"]} == {"A", "B"})

r = load("shell-interruption-and-crash.json")
check("native exact-turn interrupt stops A", r["after_interrupt_1"]["A"] == r["after_interrupt_2"]["A"])
check("native interrupt leaves B progressing", r["after_interrupt_1"]["B"] != r["after_interrupt_2"]["B"])
check("backend SIGKILL does not prove B quiescence", r["after_kill_1"]["B"] != r["after_kill_2"]["B"])

r = load("bootstrap-race.json")
failures = sum("exception" in v or "error" in v.get("response", {}) for x in r for v in x["outcomes"])
check("first-use initialization failure retained", failures > 0)
print(json.dumps({"narrow_evidence_checks": len(checks), "checks": checks,
                  "zero_regression_certification": False}, indent=2))
