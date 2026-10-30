#!/usr/bin/env python3
"""Validate retained phase-2 result scope and success predicates, not full parity."""
import json
from pathlib import Path

directory = Path(__file__).parent / "evidence"
def read(name):
    return json.loads((directory / name).read_text())

for name, count in [("native-eight-clients.json", 15), ("native-bridge-core.json", 8), ("native-faults.json", 6)]:
    r = read(name)
    assert not r.get("exception"), (name, r.get("exception"))
    assert len(r["checks"]) == count, name
    assert not r.get("full_parity"), name
mutant = read("unsafe-admission-mutant.json")
assert mutant["unsafe_mutant"] and "concurrent fence cannot pass" in mutant["exception"]
stress = read("registry-stress.json")
assert len(stress["rounds"]) == 8 and all(x["winners"] == 1 and x["refusals"] == 7 for x in stress["rounds"])
defects = read("independent-defects.json")
assert defects["summary"]["historical_fork_misbindings"] == 8
assert all(v == "ok" for t in defects["database_trials"] for v in t["database_quick_checks"].values())
measurements = read("matched-costs.json")
assert len(measurements) == 9
for run in measurements:
    assert len(run["operations"]) == 8 and len({x["thread"] for x in run["operations"]}) == 8
    assert not any(m.startswith("account/") for m in run["actual_native_methods"])
assert read("source-applicability.json")["source_applicable"]
assert not read("source-applicability.json")["full_native_build_tested"]
assert read("source-revision-refusal.json")["revision_refusal_observed"]
assert read("linux-containment-blocker.json")["passed"] is False
print(json.dumps({"phase_2_evidence_consistent": True, "native_tui_built_and_tested": False,
                  "full_parity_demonstrated": False}, indent=2))
