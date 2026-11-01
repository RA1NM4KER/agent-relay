# Native authority decisions

The initial key-only insertion is not sufficient: two live connections possessing
one owner key could both drive that owner's root. It must not be accepted as the
finished native gate. Native source review also found path/history selection
variants override supplied IDs, so those variants must refuse until actual source
identity can be proven. Fork registration must recheck its source after queueing.

Use three capability levels: enrollment, supervisor recovery, and frontend lease.
A profile policy stores the digest of an enrollment secret held only by the
trusted supervisor. Creating a new logical owner requires enrollment; inventing
a recovery secret on a raw connection cannot create execution authority.

The two per-owner capabilities have separate purposes. A stable supervisor recovery secret names the
durable logical owner and never goes to a frontend. A replaceable controller lease
is registered by that supervisor, has a durable generation and current host UUID,
and attaches one execution connection. A frontend cannot rotate its own lease.
A fresh supervisor recovery operation supplies a fresh lease secret, invalidates
old lease/connection generations and leaves other owners unchanged. Neither key
is a provider credential; only digests go into the native journal. The supervisor
must persist its proposed lease and operation token before registration.

Host restart invalidates volatile frontend attachment; recovery uses the stable
supervisor capability, not a public owner name, a guessed thread, or a process ID.
Committed native operations can replay under the owner's new lease without
execution. Unresolved operations cannot replay. Rotating a controller does not
reactivate a fenced root and does not constitute executor quiescence or handoff.

Approvals need fresh host-namespaced IDs so stale replies cannot alias requests
on a new host. The pending approval's root must still belong to the replying
controller. Native core registration and execution must ultimately retain exact
root/epoch/realm identity; task-local request context alone is not a complete
subagent/executor propagation architecture. Unknown paths fail closed.

A conversation's lease epoch and a controller's attachment generation are distinct.
Native ownership transfer remains unavailable until executor-realm authority has
issued a trustworthy quiescence receipt for the prior epoch.


The journal format is version 3, bound to the enrolled policy digest and physical
profile device/inode. Revocation fences the entire logical owner and all its
roots, including helpers and subsequently attempted `/new`; recovery cannot
reactivate it. Internal root propagation and execution sinks remain incomplete.
Known RPC parameters are canonicalized using native typed decoding before intent
fingerprinting. Commit validates response method and the actual native reserved
creation ID. Path/history overrides and unclassified mutation routes refuse.

Native thread/start runs in a tracked background task; task-local admission must
be explicitly scoped around that future. The first real positive-path test
caught this omission. Empty-thread startup also needs an actual native Persist
barrier before committing an authority result: ThreadPreparation only flushes an
existing writer and is insufficient for cold recovery. Neither fix manufactures
a user turn or promotes ephemeral helpers into persistent user conversations.


The native shell route now carries an immutable execution UUID. Scoped cancel
validates its durable intent against native thread/root, owner, epoch and host
generation, then publishes a per-execution marker. Root revocation remains a
separate owner-wide fence. The native task retains execution while its manager
obtains kernel-stop state; the experimental profile's task supervisor allows a
bounded six-second grace for the five-second drain. Ordinary profiles retain
100 ms. Exact stopped records cannot be supplied by a frontend boolean, and none
is a full handoff receipt. Controller generation at queue ingress is still missing.
