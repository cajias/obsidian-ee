# Ponytail debt ledger

Every deliberate shortcut in this repo carries a `ponytail:` comment naming its
ceiling and its upgrade trigger. This is the harvest of those comments, with a
disposition per row so a deferral cannot quietly become permanent.

Audited 2026-09-09. Branched from `main` @ `8db2779`; all line numbers below are against this branch's HEAD, after the comment rewrites this audit made.

**Disposition key**

| Code | Meaning |
| --- | --- |
| **DEFERRED** | Trigger is real and has not fired. Correct to leave. |
| **CORRECTED** | Still deferred, but the comment's stated reasoning was wrong or stale and has been rewritten. |
| **SHARPENED** | Still deferred; the comment named no revisit trigger and now does. |
| **MISFILED** | Not deferred work. Re-tagged out of the ledger. |
| **DECISION** | Deferred, but the upgrade needs a call the code cannot make alone. |

---

## crates/collab-relay/src/storage.rs

**`storage.rs:180` — no per-user byte cap on the offline queue. — CORRECTED**

- **Ceiling:** one recipient can hold the entire shared byte budget. A few
  hundred max-size frames fill `DEFAULT_MAX_TOTAL_BYTES` (128 MiB), still well
  under that user's 1000 count slots; every other user's `enqueue` is then
  refused, and those peers' CRDT replicas diverge — the exact outcome the queue
  exists to prevent.
- **Why the old comment was wrong:** it claimed "the per-user *count* cap
  already bounds a single user". It does not — the byte budget binds first, at
  roughly 450 frames, so the count cap never engages.
- **Arithmetic footnote:** the budget charges *decoded* `payload.len()`, not the
  wire size. Frames are JSON text (`Message::Text` + `serde_json`,
  `relay.rs:278`/`:300`/`:830`/`:838`) and `payload: Vec<u8>` carries no
  `serde_bytes`/base64 attribute, so it serializes as a number array at ~3.5
  chars per random byte. A 1 MiB `MAX_MESSAGE_SIZE` frame therefore charges only
  ~300 KiB. The `DEFAULT_MAX_TOTAL_BYTES` doc comment had inherited the same
  wire-size-equals-charged-size error and was corrected alongside the marker.
- **Why the fix is still NOT a per-user byte cap:** it would bound an *honest*
  heavy user but not an adversarial one, because puppet recipients are nearly
  free and need no group membership at all. A bare `Subscribe` with
  `authorized_epoch: None` requires no capability (`routing.rs:267-280`),
  handshake traffic is never content-gated (`routing.rs:394-397`), and
  subscriptions deliberately survive disconnect — so any client that can
  `Identify` can park N offline sockpuppets and fill the budget with ungated
  1 MiB `MlsHandshake` payloads. A per-user cap of X MiB just costs the attacker
  128/X puppets. A counter that claims to close starvation and does not is worse
  than an honest comment.
- **Upgrade:** per-user cap if an honest user is observed starving others;
  per-document or per-sender fair-share to close the adversarial case.
- **Second false premise, in the same block, now corrected:** `storage.rs:174-179`
  claimed a refused update "is simply resynced by this user on reconnect". It is
  not. `ClientMessage`/`ServerMessage` (`crates/collab-proto/src/lib.rs:38-141`)
  carry no state-vector, sync-step, or resync message in either direction — the
  variants are `Identify`, `Subscribe`, `RegisterDocKey`, `Unsubscribe`,
  `YrsUpdate`, `MlsHandshake` — so a dropped update is never recovered and the
  module doc at `storage.rs:4-5` is the accurate one. That makes this a
  **data-integrity** DoS, not merely an availability one, and raises the priority
  of the fair-share upgrade above.

## crates/collab-relay/src/routing.rs

**`routing.rs:195` — session takeover under a shared token. — CORRECTED**

- **Ceiling:** `RELAY_AUTH_TOKEN` is a single *shared* bearer token compared
  without reference to `user_id` (`relay.rs:370-371`), so any holder can claim
  and force-evict any `user_id`; `user_id` is otherwise self-asserted. A
  subscribe capability does not help — it proves group *membership*, not
  identity, and every member derives the same signing key (`mls.rs:640-645`).
  No liveness detection exists either: no `Ping`/`Pong` in the
  crate, no timeout arm on the connection `select!` (`relay.rs:238`), so a
  half-open session lingers until TCP reaps it.
- **Why the old comment was wrong:** it deferred "shared-token binding", but
  `with_auth_token` and the marker landed in the *same* commit (`15a64f8`, #38),
  so the token was never the thing being deferred. And the guard it protects is
  unreachable in production — `handle_identify` returns early on a bad token
  (`relay.rs:370-382`), so `allow_takeover` is always `true` by the time it calls
  `register_client` (`relay.rs:410`) and only a unit test (`routing.rs:800-814`)
  exercises the `false` branch. It is defense-in-depth against a future
  refactor, which the comment now says.
- **Upgrade:** bind `user_id` to a per-user credential, then reap dead sessions
  promptly. Revisit when the relay gains a second tenant.

**`routing.rs:363` — O(documents) scan per eviction. — SHARPENED**

- **Ceiling:** `drop_subscriptions` scans every document's subscriber set.
  Eviction only happens at capacity, so a reverse `user -> docs` index is not
  worth the extra state to keep in sync.
- **Upgrade (added):** add the index if eviction stops being rare — the relay
  routinely running at `DEFAULT_MAX_USERS`, or this scan appearing in a profile.

## crates/collab-cli/src/commands.rs

**`commands.rs:315` — fixed `PEER_RECV_TIMEOUT` (5s). — DEFERRED**
Upgrade: make configurable only if a slow relay ever needs it.

**`commands.rs:321` — fixed `CAPABILITY_TTL_SECS` (300). — DEFERRED**
Upgrade: make configurable only if a long-lived session needs it.

**`commands.rs:663` — fixed `MIN_STABLE_CONNECTION` (10s). — DEFERRED**
Upgrade: make adaptive if reconnect tuning ever matters.

**`commands.rs:894` — `min_epoch = 0` accepts a snapshot at any epoch. — DEFERRED**

- **Verified unfired.** #96 persisted MLS group state, but the relay still
  exposes no anchor-epoch query, so no rollback-resistant epoch source reached
  the client. The comment's reasoning holds: the state file cannot supply the
  epoch (a rollback restores an older file with an older claim), and the cost of
  accepting a stale snapshot is content delivery, not confidentiality — the
  relay's monotonic anchor check (`relay.rs:619-622`) refuses a stale-epoch
  capability, so the session falls back to receiving nothing.
- **Upgrade:** raise it once a client tracks the epoch somewhere an attacker
  cannot roll back.

**`commands.rs:914` — 0600 key file rather than an OS keychain. — DECISION**

- **Ceiling:** another process running as the same user can read the key file.
- **Upgrade path:** the `keyring` crate (macOS Keychain / libsecret) — not a
  dependency today, so adopting it is a new-dependency call.
- **Trigger (added):** the CLI running on a host with untrusted same-user code,
  or the Obsidian plugin gaining a keychain path that the two should match.

**`commands.rs:977` — no file mode bits off unix. — DEFERRED**
Upgrade: Windows ACL hardening if it ever ships there.

## plugins/obsidian-ee/src/main.ts

**`main.ts:415` — plain key file in the vault. — DECISION**

- **Ceiling:** Obsidian's `DataAdapter` cannot set file modes, so unlike the
  CLI's 0600 the key inherits vault permissions — vault read is key read.
- **Why the old upgrade path was not honest:** it named Electron `safeStorage`,
  which is a **main-process** API. Plugins run in the renderer with no bridge in
  — a plugin cannot ship native modules, and `@electron/remote` needs host-side
  initialization it cannot add — so `safeStorage` is unreachable, and this plugin
  requires `electron` nowhere.
- **Trigger (added):** Obsidian's own `SecretStorage` plugin API, added in
  v1.11.4. It is plaintext LocalStorage today, but Obsidian's lead has publicly
  committed to backing it with `safeStorage`. Upgrade once that migration ships,
  and call `SecretStorage` — never `safeStorage` directly.

**`main.ts:539` — fire-and-forget snapshot write on stop. — SHARPENED**

- **Ceiling:** Obsidian's `onunload` is synchronous and `saveData` is not, so
  `onunload` may exit before the write lands. This is a platform limit, not a
  shortcut — there is no awaitable unload hook. Losing the write costs one
  re-bootstrap on next start, not divergence.
- **Upgrade (added):** an awaitable unload if Obsidian gains one, or move the
  snapshot to a periodic autosave so the unload write is never the only one.

## plugins/obsidian-ee/src/collab-client.ts

**`collab-client.ts:363` — `min_epoch 0` on restore. — SHARPENED**

Mirrors `commands.rs:894`; same verification. The block previously only
cross-referenced the CLI note rather than stating a condition of its own, so it
now carries the trigger directly: raise it once the client can learn the current
anchor epoch from a source an attacker cannot roll back, which needs a protocol
message that does not exist.

**`collab-client.ts:458` — anchor-registration send result deliberately unchecked. — SHARPENED**

- **Ceiling:** `send` returning false means the frame was *queued*, not lost, so
  tearing the group down there would strand a frame still going out. The
  residual it cannot cover is a frame lost at TCP level, leaving a permanently
  content-blind session that fails closed.
- **Upgrade path:** a retry driven by the relay's `Unauthorized`, which needs
  `ServerMessage::Error` to carry a `doc_id`.
- **Trigger (added):** the block named that prerequisite but no condition for
  acting on it. Now: when a permanently content-blind session is reported in
  practice, or whenever `Error` gains a `doc_id` for another reason — the field
  is cheap to ride along.

**`collab-client.ts:576` — no mid-handshake resume state machine. — SHARPENED**
Fails closed (no plaintext); `armJoinTimer` is the cheap recovery. Explicit YAGNI.

- **Upgrade (added):** when joiners drop mid-handshake often enough that waiting
  out the join deadline is visibly slow.

**`collab-client.ts:1088` — a joiner with a lost Welcome stays stuck. — SHARPENED**

- **Ceiling:** the joiner already freed the key package its leaf's private key
  came from, so recovery needs an owner-side re-invite protocol that does not
  exist. Manual remove/re-add is the workaround. Deliberate: the two failure
  cases are indistinguishable from the client and this one fails closed.
- **Upgrade (added):** when retaining the key package private key across a
  failed join is cheaper than the manual remove/re-add — i.e. once users hit the
  stuck state often enough to report it.

**`collab-client.ts:1103` — admission by name, not public key. — DECISION**

- **Ceiling:** a `BasicCredential` identity is self-asserted, so knowing an
  allowlisted name is sufficient. This stops the stranger who knows only the doc
  id, not one who also knows who was invited.
- **Correction:** the old comment said pinning the key package's signature
  public key "needs an out-of-band exchange the plugin does not have yet". The
  channel *does* exist — joiner ids are already exchanged manually to populate
  the allowlist (`docs/security.md`, "Admission control"). The upgrade is to
  exchange a key-package fingerprint alongside the id and pin it.
- **Trigger (added):** an allowlisted name being guessable by a third party
  becoming worth the extra setup step.

---

## Re-tagged out of the ledger

**`scripts/e2e-test.sh:8` and `xtask/src/main.rs:89` — MISFILED.**
Both described a permanent cross-file sync rule (gate on the relay healthcheck,
pass `--include-ignored`), not deferred work. Re-prefixed `sync-invariant:`.
Verified nothing greps `ponytail:` in `.github/`, `.claude/`, or
`~/.claude/agents/drift-auditor.md`, so the rename breaks no tooling.

---

16 markers, 0 with no trigger.
