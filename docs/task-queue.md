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

## Claiming

Discovery is intentionally separate from claiming. Labels and comments are not the concurrency
primitive: GitHub's label endpoints have no compare-and-swap operation. Instead, the exclusive
ownership record is `refs/heads/relay/claims/<issue-number>`. A claimant re-reads
eligibility, resolves the default branch SHA, then creates that previously nonexistent ref without
force. GitHub documents ref creation as `201 Created`, with `409 Conflict` for contention; only
the `201` worker can proceed.

After it owns the ref, the worker re-reads the Issue, adds `relay:claimed`, removes
`relay:ready`, and writes a concise Relay-owned comment containing its safe session/worker identity
and claim ref. It then re-reads to verify the visible state. A failed visible mutation is a
**recoverable incomplete claim**, never permission to start work: the ref remains as the durable
serialization record until a recovery command can verify it and either finish the visibility write
or release exactly that ref. A claim never means an Issue is complete and this initial contract
never auto-closes an Issue.

Claim lifecycle is: `ready -> ref-created -> claimed-visible -> released | blocked | complete`.
Only a future explicit release/recovery command may delete a ref, after verifying the expected ref
target and its identity record. If visibility failed before the comment was written, Relay does not
guess the owner from a timestamp or ref target: it remains a manual recovery boundary. Staleness is
never inferred from elapsed time; automated stale-claim recovery requires an explicit durable
task-result/recovery proof, which is part of Issue #21.

## Explicit task results

`relay task result <issue> --claim-ref refs/heads/relay/claims/<issue> --session <relay-session>
--project <path> --result continuing|blocked|completed --summary <text>` records a bounded,
typed local attestation in that Relay Session's state directory. Before writing it reads GitHub to
prove that the exact deterministic ref and visible Relay claim still exist. A missing, stale,
ambiguous, corrupt, or mismatched binding is rejected; Relay never infers a result from a provider
exit, checkpoint, working-state text, or conversation content.

The record is evidence only. It does not delete the claim ref, edit labels/comments, close an
Issue, start a provider, advance the queue, or infer PR/review state. `continuing` is explicit
permission only to continue the same task; `blocked` and `completed` are evidence a later explicit
lifecycle command may consume after independently verifying the same claim.
