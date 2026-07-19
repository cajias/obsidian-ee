# Tech-Debt Audit Ledger — obsidian-ee

Produced by a fan-out audit (5 discovery dimensions — reachable panics,
security/zero-knowledge invariants, error handling, concurrency, deps/config —
each finding adversarially verified by an independent skeptic, then
deduplicated and synthesized). 28 confirmed findings → 25 distinct after dedup:
**4 high, 4 medium, ~17 low.**

## Resolution status

**All 25 findings below have been addressed on branch
`claude/ponytail-tech-debt-f4xdy5`.** Summary:

- **4 high** — resolved. Relay `Identify` now supports optional bearer-token
  authentication (`RELAY_AUTH_TOKEN`); sessions are connection-id-scoped with
  compare-and-remove teardown and explicit takeover on duplicate `Identify`
  (fixes the impersonation/overwrite and reconnect-TOCTOU cluster); resource
  bounds added (bounded per-client channels with slow-consumer disconnect,
  1 MiB frame cap, global connection cap, subscription caps); and `OfflineQueue`
  is wired into routing (subscriptions retained across disconnect, queued for
  offline subscribers, drained on reconnect).
- **4 medium** — resolved, with one documented residual. Unused AWS deps
  removed; `cargo deny` runs as a real gate (all sections, no
  `continue-on-error`, on PRs) with `deny.toml` migrated to schema v2;
  `unregister_client` no longer creates empty subscription sets.
  **Residual (tracked):** the `Subscribe` authorization finding is *mitigated*
  (all operations require an authenticated identity behind the optional relay
  token, and subscriptions are capped), but fine-grained per-document MLS
  membership enforcement at a zero-knowledge relay requires a signed
  subscription-capability scheme and is tracked in `PONYTAIL-DEBT.md`. Note
  that MLS already prevents non-members from decrypting anything, so the residual
  exposure is limited to metadata (which is explicitly out of scope, see
  `docs/security.md`) plus frame injection (bounded by the size/rate caps).
- **~17 low** — resolved. CLI base64 now errors on invalid input (via the
  `base64` crate); the file-based invite carries `welcome`/`commit`/`epoch` and
  `join` propagates real MLS failures; `connect` distinguishes graceful shutdown
  from error; the dead `RegistryError::IsEncrypted` variant is removed; unused
  deps (`bincode`, `serde_json`, `aws-sdk-lambda`) dropped and `tokio` features
  narrowed; the real MSRV (1.87) is declared and CI-enforced; `deny.toml`
  modernized; Docker images pinned, `redis`/`localstack` (now unused) removed,
  healthcheck + resource limits added, `version:` key dropped; and the
  nonexistent `infra/` CDK references were removed from the docs.

The original discovery ledger follows. (The separate `PONYTAIL-DEBT.md` ledger
tracks the one residual above; the replay-protection work is done.)

**Overview:** The dominant risk cluster is the `collab-relay` server's session
and resource handling. Because `Identify` is completely unauthenticated, an
attacker can impersonate any user, hijack their message routing, and knock them
offline — and the same `user_id`-keyed session model produces a silent
self-inflicted failure on the ordinary reconnect path. Compounding this, the
relay imposes no resource bounds anywhere (unbounded per-client channels,
uncapped subscriptions, 64 MiB frames, no connection limit), so any single
client can OOM the whole zero-knowledge relay, and the advertised
offline-message queue is dead code, meaning updates to briefly-disconnected
peers are lost forever and CRDT replicas silently diverge. Remaining findings
are lower-severity but real: an open `Subscribe` authorization gap, two
unbounded-map memory leaks, an inert CI/`cargo-deny` gate, several
non-functional CLI crypto paths that swallow errors, and dependency/toolchain/
Docker hygiene debt. **Fixing the four `high` items addresses the bulk of the
actual risk.**

| Severity | Area | Issue | File | Suggested fix |
|---|---|---|---|---|
| high | Auth / session | Unauthenticated `Identify` accepts any self-asserted `user_id`, overwriting existing handles — enables impersonation, routing hijack, sender-identity spoofing, metadata capture, and DoS-on-disconnect | `crates/collab-relay/src/relay.rs:212` | Require an authenticated identity (bearer token / signed challenge) bound to a `user_id` before registering; reject `Identify` for a `user_id` that already has a live connection, or scope handles by connection id |
| high | Concurrency / session | Reconnect/duplicate-`Identify` TOCTOU: a stale connection's unconditional cleanup (`clients.remove` + `unregister_client`) evicts the live newer connection's handle and strips all its subscriptions, silently freezing the live client (dedupes two findings) | `crates/collab-relay/src/relay.rs:163` | Key sessions by a unique per-connection id, or on cleanup only remove if the stored handle still points at *this* connection; on duplicate `Identify`, explicitly evict the prior session |
| high | DoS / resources | No application resource bounds: unbounded per-client `mpsc` channel (slow reader OOMs relay), uncapped subscriptions, 64 MiB default frame size fanned out ×N subscribers, no connection cap or rate limiting (dedupes two findings) | `crates/collab-relay/src/relay.rs:115` | Use bounded channels and drop/disconnect slow consumers; set `WebSocketConfig` max message/frame (~1 MiB); cap subscriptions per connection and `doc_id` length; add per-connection rate limiting and a global connection cap |
| high | Data loss | `OfflineQueue` is exported but never instantiated; messages to subscribed-but-disconnected peers are silently dropped and never resynced, causing permanent CRDT divergence (contradicts lib.rs / CLAUDE.md offline-persistence claims) | `crates/collab-relay/src/storage.rs:41` | Wire `OfflineQueue` into `route_message` (enqueue for offline subscribers) and drain on `Identify`; add resync-on-subscribe for late joiners — or remove the module and drop the persistence claim |
| medium | Authorization | `Subscribe` only checks "identified", never MLS group membership; any client can join any doc stream to harvest metadata (epochs, sender ids, sizes, timing) and inject frames forcing member CPU work | `crates/collab-relay/src/relay.rs:237` | Gate `Subscribe` on a relay-checkable membership proof (signed capability/subscription token scoped to `doc_id`+epoch); reject subscriptions lacking one |
| medium | DoS / memory | `unregister_client` removes users from each doc set but never prunes now-empty sets, so the `subscriptions` map grows unbounded under attacker-chosen `doc_id` churn | `crates/collab-relay/src/routing.rs:35` | Mirror `unsubscribe`: `subs.retain(\|_, set\| { set.remove(user_id); !set.is_empty() })` |
| medium | Dependencies | Unused `aws-config` / `aws-sdk-dynamodb` (storage is in-memory) drag in a duplicated legacy+modern TLS/HTTP stack (rustls 0.21+0.23, hyper 0.14+1.10, h2, http), inflating compile time and binary size | `crates/collab-relay/Cargo.toml:24` | Remove the AWS deps until DynamoDB is implemented; reintroduce behind a `dynamodb` Cargo feature — this also clears the duplicate-version warnings |
| medium | CI / tooling | Security audit runs `cargo deny check advisories` with `continue-on-error: true`, only checks `advisories`, and skips PRs; E2E test step also `continue-on-error` — the deny policy and E2E gate are effectively inert | `.github/workflows/ci.yml:100` | Run `cargo deny check` (all sections) without `continue-on-error`, on `pull_request`; remove `continue-on-error` from the E2E test step |
| low | Session / leak | Re-`Identify` on one connection overwrites local `user_id` without unregistering the prior identity, leaking stale `clients`/`subscriptions` entries | `crates/collab-relay/src/relay.rs:202` | On re-`Identify`, unregister the previous `user_id` first, or forbid re-`Identify` |
| low | DoS / memory | `OfflineQueue` caps messages per user but never bounds the number of user keys or adds TTL (latent — also dead code) | `crates/collab-relay/src/storage.rs:41` | Bound tracked users and/or add per-user TTL before backing with DynamoDB, or remove the dead export |
| low | CLI / crypto logic | `create_invite` builds the wire `Invite` from a throwaway per-call MLS group and drops `commit`/`epoch`, so the file-based invite/join path can never establish a working shared group | `crates/collab-cli/src/commands.rs:138` | Persist/reload the owner's `MlsDocumentGroup`, add `commit`/`epoch` to the proto `Invite` — or mark these commands as non-functional scaffolding |
| low | CLI / error handling | `join` regenerates a fresh key package then collapses the resulting MLS failure into `Ok(success:false, "...expected...")`, exiting 0 and masking genuine crypto errors | `crates/collab-cli/src/commands.rs:207` | Load the actual `PendingMember` from keygen; propagate real failures as `Err`/non-zero |
| low | CLI / correctness | Hand-rolled `base64_decode` maps invalid chars to 0 via `unwrap_or(0)` and drops short trailing chunks; its `Result` can never `Err`, so corrupt input yields garbage bytes | `crates/collab-cli/src/commands.rs:498` | Return `Err` on invalid input, or replace with the `base64` crate |
| low | CLI / correctness | `connect` maps a graceful `Ok(())` session end to `on_disconnected()`, so a clean shutdown is indistinguishable from failure and always exits non-zero | `crates/collab-cli/src/commands.rs:434` | Signal graceful-shutdown vs error and exit `Ok` on the former |
| low | Dead code / API | `RegistryError::IsEncrypted` is only ever constructed in a test; plain-vs-encrypted misuse returns `None`, so the variant implies a guard that doesn't exist | `crates/collab-core/src/registry.rs:27` | Remove the variant, or actually return it from plain accessors on encrypted docs |
| low | Dependencies | `bincode.workspace = true` declared in collab-core but referenced nowhere | `crates/collab-core/Cargo.toml:17` | Delete the line |
| low | Dependencies | `serde_json.workspace = true` declared in collab-proto, which does no JSON (de)serialization | `crates/collab-proto/Cargo.toml:12` | Remove the line |
| low | Dependencies | `aws-sdk-lambda = "1.97"` declared in `[workspace.dependencies]` with no consumer | `Cargo.toml:61` | Delete the declaration |
| low | Dependencies | `tokio = { features = ["full"] }` workspace-wide pulls unused subsystems | `Cargo.toml:41` | Replace `"full"` with the explicit used feature set |
| low | Toolchain | Declared `rust-version = "1.75"` is false and unenforced — pinned AWS deps require ~1.94; the edition comment ("Rust 2024") also conflicts with `edition = "2021"` | `Cargo.toml:19` | Add an MSRV CI job and set the real minimum, or drop the promise; fix the comment |
| low | Tooling config | `deny.toml` uses deprecated keys current `cargo-deny` rejects as parse errors — combined with `continue-on-error`, the audit silently no-ops | `deny.toml:6` | Migrate to the current schema (`version = 2`) and pin `cargo-deny` in CI |
| low | Docker config | Base images unpinned (`rust:latest`, `localstack:latest`) — non-reproducible builds and MSRV drift | `docker/Dockerfile.relay:2` | Pin explicit versions/digests |
| low | Dead config | docker-compose defines a `redis` service + `REDIS_URL` but no crate uses redis and the relay never reads it | `docker/docker-compose.yml:22` | Remove the redis service/env until presence/pubsub is implemented |
| low | Docker config | No service resource limits, no relay healthcheck, obsolete `version: '3.8'`, hardcoded dev creds | `docker/docker-compose.yml:1` | Drop `version`; add `mem_limit`/`cpus` + healthcheck; move env to `.env` |
| low | Config drift | CLAUDE.md documents an `infra/` CDK directory that doesn't exist, so the IAM/limit review has no committed IaC | `CLAUDE.md:1` | Remove the `infra/` references or commit the CDK stack |
