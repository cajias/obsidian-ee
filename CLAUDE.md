# Obsidian E2E Collaborative Editing

End-to-end encrypted collaborative document editing using Yrs CRDT and MLS.

## Build & Test

```bash
# Run with clippy lints
cargo lint
```

## Development

### TDD Workflow

This project uses strict TDD:
1. **RED:** Write failing test first
2. **GREEN:** Minimal code to pass
3. **REFACTOR:** Clean up while tests stay green

### Local E2E Testing

```bash
# Start local environment
docker compose -f docker/docker-compose.yml up -d

# Run E2E tests
./scripts/e2e-test.sh

# Stop environment
docker compose -f docker/docker-compose.yml down
```

## Architecture

- **Offline queue**: In-memory today; DynamoDB-backed persistence is planned
  behind a Cargo feature

## Engineering rules (from audit RCA)

These encode failure classes found in past audits that automated linters do not catch.

### Trust-boundary & crypto invariants
Every trust boundary (inbound network message, decrypted payload, any peer- or
relay-supplied field) MUST have a NEGATIVE-path test asserting the attacker case is
REJECTED — not only a positive round-trip. A crypto test proving "same key/context
decrypts" is insufficient alone; add the sibling proving "wrong context FAILS" (a
ciphertext for doc A must be rejected under doc B, even with a shared key).

E2E-encrypted payloads MUST be AEAD-bound to their context via associated data
(document id today; document id + epoch once MLS lands). The relay is an untrusted
zero-knowledge router: a ciphertext valid for one document MUST fail authentication when
applied to another. Bind the LOCALLY-TRUSTED context (e.g. `config.docId`), NEVER a
value taken from the inbound frame.

Encryption is MLS-only: there is no user-supplied or configured key material, so
"reject a placeholder key" is not the guard anymore. The fail-closed invariant is that
NO update is encrypted, sent, or applied before the MLS group is established — an
owner must have created its group and a joiner must have consumed a Welcome. Keep the
guards that make `sendUpdate` refuse (return false, send nothing) without an MLS group,
and cover them with a negative-path test proving a pre-Welcome client emits no frame
and no plaintext ever leaves the client.

When a security audit CONFIRMS a trust-boundary finding, the fix MUST leave a
negative-path regression test behind that is RED before the fix and GREEN after — the
test is the durable artifact that proves the invariant and stops the class from
regressing. An audit that fixes code without adding such a test is not done.

AI security review runs LOCALLY only (`/security-review`, or `Workflow({name:
'security-audit'})`) on the Claude subscription plan — never as a CI action keyed on an
Anthropic API secret. CI gates stay deterministic (fmt, clippy, tests, cargo-deny,
gitleaks); the AI passes are a developer-run step, not a billed pipeline job.

### Filesystem-watcher tests
`notify_debouncer_mini` does NOT deliver a 1:1 filesystem-action→event mapping — a
create can be followed by a content `Modified` in a later debounce window. Tests that
observe watcher events MUST drain until the stream goes quiet and assert the expected
kind is *present* (`.any(|e| e.kind == X)`), never `recv()` exactly one event per action.
The crate's `drain_events`/`collect_events` helpers exist for this.

### Reconnect & connection lifecycle
- Every connect attempt MUST settle its promise/future exactly once — including a retry
  attempt whose socket fails *before* opening. A never-settled connect deadlocks the
  reconnect loop (a dedup guard then returns the stale pending promise forever).
- Session start/stop (and any resource-owning lifecycle command) MUST be idempotent:
  guard against a second start that would orphan the prior client/handle.
- The TS client's reconnect behavior must have state-machine tests mirroring the Rust
  CLI's — reconnect logic is duplicated across the two and has regressed on both sides.

### Resource bounds
Any collection fed by untrusted or network-sourced input MUST be bounded by BYTES, not
just by element count — a per-item count cap with MiB-scale items still permits OOM.
Charge/credit the byte counter on every add/remove path and keep it O(1).

### Partial-success state
A flag or handle that records "established" while only PART of a multi-step setup
succeeded is this codebase's most repeated defect — four instances in one audit
session, every one invisible to a fully green suite.

Set a completion flag only AFTER every step it claims completed has returned. A
teardown may undo only work whose side effect has not yet left the process: once
a frame is on the wire (`register_doc_key` above all, which the relay refuses to
accept twice for the same document), freeing the local state strands it and no
retry can recover.

Scope a teardown to the unit of work. One `try` around a loop over N
side-effecting units either under-cleans (leaks the failed unit) or over-cleans
(destroys units that succeeded); use a per-unit `try`. A safety comment written
in the singular about a plural operation is the tell.

A single boolean cannot honestly represent N independently-established resources.
Derive readiness from the resources themselves rather than latching a flag over
them.

Test the state, not the promise: asserting that a retry RESOLVES proves nothing,
because these failures resolve normally and go quiet. Assert the post-retry state
is USABLE — the handle exists, the registration was sent exactly once.

### Dead code / YAGNI
Keep internal-crate APIs `pub(crate)` (not `pub`) so `rustc`'s `dead_code` lint flags
unused items — `pub` items in a workspace-internal crate are never reported as dead.
Do not add speculative public surface "for later"; a test that exists only to exercise
otherwise-unused code is a signal to delete the code, not keep it.
