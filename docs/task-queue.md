# GitHub task queue (initial contract)

`relay task next` is a deterministic, read-only view of the Agent Relay GitHub task bucket. It
does not start a provider, write GitHub state, or ask an LLM to decide queue ownership.

## Labels and selection

Relay owns only these exact labels:

- `relay:ready` — an Issue is eligible for a Relay worker;
- `relay:claimed` — a worker has made an ownership claim; and
- `relay:blocked` — work needs an explicit human or external resolution.

The command reads open entries from GitHub's Issues API, rejects pull requests, then selects the
lowest issue number that has `relay:ready` and neither `relay:claimed` nor `relay:blocked`.
Other labels, including malformed label objects, are not Relay queue state. No eligible item is a
successful empty result, not a reason to guess at unrelated Issues.

## Claiming (next slice)

Discovery is intentionally separate from claiming. A claim will add `relay:claimed` and a
Relay-owned, identity-bearing GitHub comment, then re-read the Issue before a worker starts.
GitHub's label endpoints do not offer Relay a documented compare-and-swap primitive. Therefore a
claimant must fail closed if it cannot prove that its marker is the sole winning Relay claim;
local serialization is only an optimization and never proof across hosts. A claim never means an
Issue is complete and this initial contract never auto-closes an Issue.
