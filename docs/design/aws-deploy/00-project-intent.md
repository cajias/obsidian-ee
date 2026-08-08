# 00 — Project Intent

## Vision

People co-editing encrypted documents meet through an always-on rendezvous service that can never read what they share.

## Why this matters

The encrypted-collaboration relay service exists and works, but it has nowhere reliable to run. Today a collaboration session only succeeds while someone happens to keep a relay running by hand; the moment it stops, every session it carried goes dark. Collaborators need a rendezvous point that is simply always there, reachable at a stable published address at any hour. Hosting it must also preserve the property that makes the system worth using at all: the service forwards only ciphered material and can never read the content it carries, no matter who hosts it or where.

This effort gives the relay service a permanent, operated home: hosted environments to run in, a release process that versions every change, an automated path that carries an approved change into those environments, and routines that keep day-to-day operation cheap and unexciting.

## Who it's for

Three named human actors recur throughout this design set; later documents reuse these names exactly.

- **The maintainer** — builds the relay service and operates its hosted environments: provisions them, deploys releases to them, rotates credentials, and recovers them when something breaks.
- **The collaborator** — a person whose encrypted editing sessions relay through the service. Collaborators hold a per-environment credential, connect to a published address, and expect to be admitted promptly and served continuously.
- **The release manager** — the maintainer wearing the promotion-approval hat: the person who cuts a release and approves promotion of a change into shared environments. Keeping the role named separately keeps the approval decision a deliberate act even when one person plays both parts.

## What it is

- An always-on hosted home for the relay service, with three environments (development, staging, production), each reachable at its own stable published address.
- A release process in which every change to the service is versioned, and production runs only versions that were deliberately cut as releases.
- An automated delivery path that carries a change from approval to a running environment with no hand-copied artifacts and no standing credentials held by the automation.
- A credential scheme in which each environment admits only participants presenting that environment's valid credential.
- An operating practice in which promotion to shared environments is a visible human decision, recovery to a prior release is a fast routine drill, and emergency access for the maintainer exists without weakening day-to-day closure.
- A service whose total running cost is small enough that keeping it alive is never a question.

## Tenets

Immutable constraints on everything that follows. Every design choice in later documents must be defensible against these.

- **T1 — Zero-knowledge hosting.** Content confidentiality survives hosting: the service relays only ciphered material it cannot read, and each environment admits only participants presenting a valid credential. Neither the maintainer nor the hosting environment ever gains the ability to read collaborator content.
- **T2 — Provenance.** What runs is exactly what was released. Every version running in any shared environment traces to a deliberately cut release, and the artifact promoted forward is byte-identical to the one validated earlier.
- **T3 — No standing secrets.** Automation holds no long-lived credentials. Every credential the delivery path uses is issued for the occasion, scoped to the task, and expires on its own.
- **T4 — Human-gated promotion.** A human decision gates every promotion into a shared environment. Reaching production requires the release manager's deliberate act of cutting a release; reaching staging requires an explicit approval.
- **T5 — Boring recovery.** Recovery is fast and routine. Restoring a prior release, regaining emergency access, and rebuilding an environment from scratch are all rehearsed procedures measured in minutes, never heroics.
- **T6 — Existence-proof cost.** Running cost stays small enough that the service's existence is never questioned: a fixed, predictable monthly amount the maintainer pays without thinking.

## Success criteria

Each criterion is a specific, observable signal with a threshold and a check moment, so progress against it can be measured and the effort has a defined "done", per [[smart-goals-make-agent-objectives-measurable-and-bound-the-monitoring-loop]]. Each is tagged with the tenet(s) it serves.

- **SC1 (T1)** — A collaborator holding a valid credential can reach every environment at its published address, be admitted, and relay an editing session end to end. Checked after every deployment to that environment.
- **SC2 (T1)** — A party presenting no credential, or an invalid one, is refused participation in every environment. Checked after every deployment, as the proof that no environment ever operates as an open relay.
- **SC3 (T2)** — Every version observed running in production matches a cut release, and the artifact serving production is byte-identical to the one validated in earlier environments. Checked on every production change.
- **SC4 (T3)** — An inventory of the credentials available to the delivery automation finds zero long-lived secrets; everything it uses is per-occasion and expiring. Checked at initial go-live and on periodic review.
- **SC5 (T4)** — Every promotion into staging visibly pauses until a named human approves it, and production changes occur only following a published release. Checked on every promotion attempt.
- **SC6 (T5)** — A prior release can be restored to production in under fifteen minutes, without rebuilding anything. Checked by a rollback drill at go-live and rehearsed periodically thereafter.
- **SC7 (T6)** — Total monthly running cost across all environments stays under forty US dollars. Checked on each monthly statement.

## Scope (v1)

- Three hosted environments (development, staging, production), each running the single relay service at its own stable address.
- A versioned release process covering every change to the service.
- An automated delivery path from approved change to running environment, satisfying T2–T4.
- Per-environment collaborator credentials, issued at bootstrap and readable only where needed.
- Verification routines proving each environment healthy, admitting credentialed collaborators, and refusing everyone else (SC1, SC2).
- A one-time bootstrap runbook the maintainer can follow end to end, plus routine procedures for rollback and emergency access (T5).
- Operation within the fixed monthly budget (SC7).

## Future directions

- **Session continuity across deployments** — carrying queued-up material through a service restart so collaborators resume exactly where they left off.
- **Idle-friendly connections** — keeping quiet collaborators connected through long pauses instead of expecting them to reconnect.
- **A second approval gate for production** — adding an explicit named-human approval on production promotion, on top of the release cut.
- **Higher availability** — serving each environment from more than one relay instance so a deployment or failure is invisible to collaborators.

## Derivation & trace rule

This document deliberately states intent free of implementation detail, per [[hld-documents-separate-solution-intent-from-implementation-detail-via-a-fixed-7]]; the "how" lives in the documents that follow. Documents 01–04 of this design set derive from this one: every goal and milestone they introduce must cite the tenet (T#) or success criterion (SC#) it serves, and any item that cannot cite one does not belong in the set. Once execution begins, the tenets and success criteria above are locked; changing any of them is a formal scope change with a written justification and an updated plan, never a silent edit, per [[goals-must-be-immutable-during-execution]].
