# Ponytail debt ledger

Every deliberate shortcut in this repo carries a `ponytail:` comment naming its
ceiling and its upgrade trigger. This is the harvest of those comments, with a
disposition per row so a deferral cannot quietly become permanent.

Audited 2026-09-09. Branched from `main` @ `8db2779`. Line numbers are against
this branch's HEAD, after both the comment rewrites this audit made and the
three fixes it led to (`7f35087`, `445407b`, `f06d870`).

**Disposition key**

| Code | Meaning |
| --- | --- |
| **DEFERRED** | Trigger is real and has not fired. Correct to leave. |
| **CORRECTED** | Still deferred, but the comment's stated reasoning was wrong or stale and has been rewritten. |
| **SHARPENED** | Still deferred; the comment named no revisit trigger and now does. |
| **MISFILED** | Not deferred work. Re-tagged out of the ledger. |
| **DECISION** | Deferred, but the upgrade needs a call the code cannot make alone. |
| **PAID** | No longer deferred. The shortcut was removed and the marker with it. |

---

## crates/collab-relay/src/storage.rs

**`storage.rs:180` (removed) — no per-user byte cap on the offline queue. — PAID**

Fixed in `7f35087`, `445407b` and `f06d870`; the marker is gone. What it
described, and what closed it:

- **The starvation itself.** One recipient could hold the whole shared byte
  budget, after which every other user's `enqueue` was refused. The budget is now
  split by kind (112 MiB content / 16 MiB handshake, per-user caps 8 MiB / 1 MiB),
  and at budget the queue evicts from the LARGEST holder of that kind instead of
  refusing the newcomer — so a modest user is never the victim while a hog exists.
- **The aiming mechanism.** The marker's own analysis said a per-user cap would
  not help because puppet recipients were free. That was right, and the reason was
  worse than recorded: `handle_yrs_update` had no sender authorization at all, so
  any identified client could push content at any document. Publishing is now
  gated on the same predicate as receiving. The handshake channel must stay open
  to non-members, so it is bounded by size instead (256 KiB, ~10x a measured
  200-member Welcome).
- **The silence.** `enqueue` returned `Option<UserId>`, which conflated refused,
  queued and evicted — no caller could observe loss. It now returns
  `EnqueueOutcome`, and the router logs contention as `warn` and routine count-ring
  trims as `debug`.
- **A claim this ledger got wrong twice.** The row previously asserted that a
  dropped update diverges the replica permanently, then that it self-heals.
  Neither is right. Content frames are cumulative full state
  (`encryption.rs:71-72` -> `document.rs:64-66`), so a dropped one is carried by
  the next frame — but only if some peer edits again, so a quiet document stays
  diverged. A lost `Welcome` or `Commit` never heals at all, and that is the kind
  an outsider could send. The corrected reasoning now lives on the `Kind` enum.

Two NEW markers replace it, both recording ceilings of the fix rather than
deferred work on the old one:

**`storage.rs:190` — O(queue) walk to find the oldest message of one kind. — DEFERRED**
The single deque is what keeps drain FIFO across kinds, which the TS client
depends on. Runs only under budget pressure. Upgrade: a per-kind index of deque
positions if eviction stops being rare.

**`storage.rs:203` — O(users) scan per evicted message. — DEFERRED**
The happy path stays O(1). Upgrade: a per-kind max-heap keyed by bytes if the
relay routinely sits at its budget.

## crates/collab-relay/src/routing.rs

**`routing.rs:236` — session takeover under a shared token. — CORRECTED**

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

**`routing.rs:405` — O(documents) scan per eviction. — SHARPENED**

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

17 markers, 0 with no trigger. One deferral (offline-queue starvation) was paid
down rather than re-deferred; the two markers that replaced it record the cost of
the fix, not a reprieve from it.
