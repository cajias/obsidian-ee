---
title: "Milestone: Local end-to-end verification backbone"
date: 2026-07-24
status: "approved — milestone #1 + issues #46–52 created"
related_issues: [22, 23, 25, 26, 27, 28, 46, 47, 48, 49, 50, 51, 52]
---

# Milestone: Local end-to-end verification backbone

## TL;DR

We do **not** need a cloud deploy to verify obsidian-ee end to end — the existing
`docker/docker-compose.yml` relay (or the in-process `TestServer`) is the verifier.
There are two verification tiers: Tier 1 (security *property*, isolated/unit) is
**already proven** this session — #27's fail-closed jest guards and #28's Stage-1
headless-browser openmls lifecycle — while Tier 2 (feature *behavior* over the wire,
multi-process) is the gap: no test today drives a real MLS handshake between two
independently-keyed clients through the real relay. This one milestone builds that
Tier-2 backbone, which simultaneously gives teeth to the currently-hollow E2E
fixtures (#22/#23/#25/#26) and becomes the acceptance harness for #28 (MLS-in-WASM).
Work is layered: **Layer A** (CLI/native + real docker relay + CI gate fix) is
buildable now; **Layer B** (WASM MLS surface + plugin + Playwright) lands with #28.

## Do we need to deploy?

No cloud deploy. The relay is a zero-knowledge, in-memory WebSocket router
(`crates/collab-relay/src/routing.rs`) that binds `RELAY_ADDR` (default
`0.0.0.0:8080`, `crates/collab-relay/src/main.rs`) and routes opaque encrypted
frames by doc subscription, queuing for briefly-offline subscribers. It decrypts
nothing and has no database (DynamoDB offline queue is planned behind a Cargo
feature, absent today). `docker/docker-compose.yml` defines exactly one `relay`
service built from `docker/Dockerfile.relay`, exposing `8080:8080`, with a raw-TCP
`nc -z localhost 8080` healthcheck; `RELAY_AUTH_TOKEN` is commented out, so it runs
open by default. "Verify with a local deployment" therefore means `docker compose up`
— or the in-process `TestServer::start()` (binds `127.0.0.1:0`) in
`tests/e2e-tests/tests/auto_connect.rs`. No cloud, no localstack.

| Tier | What it proves | Scope | Status |
|------|----------------|-------|--------|
| **1 — security property** | No session starts on a bad key; MLS lifecycle is cryptographically sound | Isolated / single-process | **Proven this session.** #27: `validateConfig` rejects all-zeros (`collab-client.ts:111`), `startSession` fails closed (`main.ts:185`). #28 Stage-1: full openmls lifecycle in real headless browser (epoch=1, zero clock errors). Local deploy not required — it only adds an over-the-wire confirmation. |
| **2 — feature behavior** | Two independently-keyed clients establish a real MLS group (KeyPackage→Welcome→Commit) via the relay and round-trip encrypted yrs updates; a wrong key fails closed over the wire | Multi-process | **The gap.** Cannot be verified in one process. This is what the milestone builds and what #28 needs. |

## Current state — why today's E2E is hollow

- **`scripts/e2e-test.sh` cannot fail (#22).** It brings up docker compose, then gates
  on `cargo test -p e2e-tests --test full_flow` **without** `--ignored` — so it runs
  only the in-process crypto/CRDT unit tests; the relay is never touched. The docker
  bring-up is decorative and infra failures are swallowed by design.
- **`cargo xtask e2e` runs 2 tests, not a suite (#22).** `xtask/src/main.rs` does pass
  `--ignored`, but the entire `e2e-tests` package has only two `#[ignore]`d tests
  (`test_two_users_collaborate`, `test_offline_message_delivery` in `full_flow.rs`),
  and it gates on a 3-second sleep rather than the healthcheck.
- **The one over-relay test fakes the handshake.** `full_flow.rs::test_two_users_collaborate`
  (~line 414) has Alice generate Bob's KeyPackage locally and Bob reconstruct the invite
  with hardcoded `commit: vec![]` and `epoch: 1`. KeyPackage→Welcome→Commit never
  actually crosses the wire between two independently-keyed clients — and it is
  `#[ignore]`d, so CI never runs it.
- **The only real two-client test uses a mock relay, not MLS.**
  `plugins/obsidian-ee/src/__tests__/two-user-integration.test.ts` drives two genuinely
  separate clients, but over a JS `IntegrationMockRelay` (`ws` broadcast loop, not the
  real collab-relay binary) with a shared AES-PSK key (`new Uint8Array(32).fill(42)`).
- **MLS is not exposed at the WASM boundary.** `crates/collab-wasm/src/lib.rs` `CollabCore`
  exports only `set_encryption_key`/`encrypt`/`decrypt`/`insert`/`delete`/`get_text`/
  `encode_state_encrypted`/`apply_update_encrypted` — no `create_invite`/`join`/
  `generate_key_package`/`process_commit`. The plugin path is AES-PSK; MLS lives only in
  native `collab-core`.
- **The headline Playwright spec is skipped (#25).** `plugins/obsidian-ee/playwright.config.ts`
  exists (testDir `./e2e`, no webServer) but the single spec
  `plugins/obsidian-ee/e2e/two-user-sync.spec.ts` is `test.skip(...)` (line 17) with a
  stub body. `package.json` has `"e2e": "playwright test"`, but CI never calls it.
- **CI runs none of the wire tests (#23).** `.github/workflows/ci.yml` runs
  `cargo test --workspace` (non-ignored only), an `e2e` job that shells `./scripts/e2e-test.sh`
  (so never the ignored wire tests), and `npm test` for jest — but not `npm run e2e`
  (Playwright). The plugin's 134 TS tests have no dedicated CI job.

## Proposed verification backbone

Stand up the real relay locally and drive it with two independent clients:

1. Bring up the relay via `docker compose -f docker/docker-compose.yml up -d` (or an
   in-process `TestServer`), and **gate on the healthcheck**, not a fixed sleep.
2. Two independently-keyed clients (Layer A: two `collab-cli` processes; Layer B: two
   WASM/plugin clients) each generate their own key material.
3. A real MLS handshake crosses the wire through the relay:
   - Client A calls `keygen` → publishes a KeyPackage.
   - Client B `invite`s using A's KeyPackage → produces a real Welcome + Commit.
   - Client A `join`s from the Welcome, applies the Commit → both reach the same epoch.
4. Client A edits the doc; the encrypted yrs update is relayed and **decrypts to the
   expected plaintext** on Client B (assert positive round-trip, not ciphertext≠utf8).
5. A control client with the **wrong key fails closed** over the wire — no session,
   no plaintext — confirming #27's guard end to end.

The assertion that matters: KeyPackage→Welcome→Commit actually transited the relay
between two separate key holders, and the encrypted CRDT update round-tripped. That is
the honest Tier-2 signal today's fixtures fake.

## Layered plan

### Layer A — buildable now (CLI/native + real relay + CI)

- **Compose CLI into a two-process session assertion.** `crates/collab-cli/src/main.rs`
  already has `keygen`/`invite`/`join`/`connect`; add a subcommand (or `connect` flag)
  that emits a machine-checkable "received text == expected" line a shell fixture can
  gate on. `connect` today only listens/collaborates. (Touches: `collab-cli/src/main.rs`.)
- **Write the real over-relay MLS test.** Replace the faked handshake in
  `tests/e2e-tests/tests/full_flow.rs::test_two_users_collaborate` with a genuine
  KeyPackage→Welcome→Commit across two independently-keyed clients via the relay; keep
  it `#[ignore]`d for the deploy-gated job but make it real. (Touches: `full_flow.rs`.)
- **Make the E2E gate fail on failure (#22).** Fix `scripts/e2e-test.sh` to run the
  `--ignored` wire tests and gate on the compose healthcheck; make `cargo xtask e2e`
  wait on the healthcheck rather than sleep 3s. (Touches: `scripts/e2e-test.sh`,
  `xtask/src/main.rs`.)
- **Over-relay fail-closed check (#27).** Add a wire assertion that a wrong-key client
  cannot start a session or observe plaintext through the relay. (Touches: `full_flow.rs`
  or a new `tests/e2e-tests/tests/fail_closed.rs`.)
- **CI: run it, and run the plugin (#23).** Add a CI job that boots the relay and runs
  the ignored wire tests; add a dedicated plugin job running the 134 TS tests +
  `tsc --noEmit`. (Touches: `.github/workflows/ci.yml`.)

### Layer B — lands with / after #28 (WASM MLS + plugin + Playwright)

- **Expose MLS at the WASM boundary.** Add `generate_key_package`/`create_invite`/`join`/
  `process_commit` to `crates/collab-wasm/src/lib.rs` `CollabCore` (this *is* #28's
  deliverable). Layer B's MLS assertions are gated on this landing.
- **Plugin JS test against the real relay.** Replace `IntegrationMockRelay` +
  AES-PSK-fill(42) in `two-user-integration.test.ts` with the real collab-relay binary
  and MLS via the WASM surface. Reuse this session's real-compiled-WASM harness pattern
  (#26) rather than re-mocking. (Touches: `plugins/obsidian-ee/src/__tests__/`.)
- **Un-skip the headline Playwright spec (#25).** Remove `test.skip` in
  `plugins/obsidian-ee/e2e/two-user-sync.spec.ts` and wire `package.json`'s `"e2e"`
  script into CI. **Until #28 lands**, the spec should assert the real behavior that
  exists — the AES-PSK sync path + fail-closed on a bad key — not ship a stub; swap in
  MLS assertions when the WASM surface is available. (Touches: `two-user-sync.spec.ts`,
  `.github/workflows/ci.yml`.)

## Fixture gap checklist

- [ ] `scripts/e2e-test.sh` runs `--ignored` wire tests and gates on the healthcheck (not decorative).
- [ ] `cargo xtask e2e` waits on the compose healthcheck instead of `sleep 3`.
- [ ] `full_flow.rs` drives a **real** KeyPackage→Welcome→Commit over the relay (no hardcoded `commit: vec![]`/`epoch: 1`).
- [ ] A wire fail-closed test proves a wrong-key client gets no session and no plaintext (#27 over the wire).
- [ ] `collab-cli` emits a machine-checkable received-text assertion for shell fixtures.
- [ ] Plugin two-client test uses the **real** relay binary + MLS (not `IntegrationMockRelay`/AES-PSK) — gated on #28.

## Relationship to existing issues

| Issue | How this milestone changes / absorbs it |
|-------|------------------------------------------|
| **#22** — `e2e-test.sh` / `xtask e2e` are hollow | **Absorbed (Layer A).** Gate on healthcheck + run `--ignored` wire tests so the script can actually fail. |
| **#23** — no CI job for the TS plugin | **Absorbed (Layer A).** Dedicated plugin CI job runs the 134 tests + `tsc --noEmit`. |
| **#25** — headline Playwright spec is skipped | **Absorbed (Layer B).** Un-skip; assert AES-PSK + fail-closed now, MLS when #28 lands. |
| **#26** — no jest exercises real compiled WASM | **Absorbed (Layer B).** Reuse this session's real-WASM harness for the plugin MLS test. |
| **#27** — fail-closed on bad key | **Extended.** Tier-1 guard already proven; milestone adds the over-relay (Tier-2) confirmation. |
| **#28** — MLS in WASM | **Acceptance harness.** This milestone's Tier-2 wire test is #28's acceptance criterion; Layer B's MLS assertions gate on #28's WASM surface. |

## GitHub artifacts (created)

Created on approval (2026-07-24). Milestone #1 holds #22/#23/#25/#26 as members and #27/#28 as related; the seven A/B items below were opened as #46–#52. #27 has since merged (PR #53).

**Milestone #1 (created)**
- **Title:** Local end-to-end verification backbone
- **Description:** Give the hollow E2E fixtures teeth using the local docker-compose
  relay (no cloud). Build the Tier-2 multi-process MLS-over-relay wire test that
  doubles as #28's acceptance harness. Layered: CLI/native + CI now (Layer A); WASM
  MLS + plugin + Playwright with #28 (Layer B).

**Existing issues added as members:** #22, #23, #25, #26 (members); #27, #28 (related).

**New issues created**
- **A1 (#46) — Real over-relay MLS handshake test.** Replace the faked handshake in
  `full_flow.rs::test_two_users_collaborate` with a genuine KeyPackage→Welcome→Commit
  across two independently-keyed clients via the real relay. *Accept:* no hardcoded
  `commit`/`epoch`; both clients reach the same epoch; encrypted yrs update decrypts to
  expected plaintext on the peer.
- **A2 (#47) — CLI machine-checkable session assertion.** Add a `collab-cli` mode that emits
  "received == expected" for shell fixtures, composing `keygen`/`invite`/`join`/`connect`
  into one two-process session. *Accept:* a shell script exits non-zero when the peer's
  received text differs from expected.
- **A3 (#48) — Over-relay fail-closed check.** Wire test proving a wrong-key client starts no
  session and observes no plaintext. *Accept:* the bad-key client never enters a session
  and the relay yields it nothing decryptable.
- **A4 (#49) — Deploy-gated CI job.** CI job boots the relay (healthcheck-gated) and runs the
  `--ignored` wire tests. *Accept:* CI fails if the relay is down or a wire test fails.
- **B1 (#50) — Plugin CI job (#23 impl).** Dedicated job runs the 134 plugin TS tests +
  `tsc --noEmit`. *Accept:* plugin type errors and test failures block CI.
- **B2 (#51) — Plugin two-client test on the real relay.** Replace `IntegrationMockRelay` +
  AES-PSK with the real relay binary + MLS via the WASM surface. *Accept:* two WASM
  clients complete an MLS handshake through the real relay (gated on #28).
- **B3 (#52) — Un-skip the Playwright headline spec.** Remove `test.skip`; assert AES-PSK sync
  + fail-closed now, MLS when #28 lands; wire `npm run e2e` into CI. *Accept:* spec runs
  green in CI asserting real behavior (no stub body).

## Deliberate non-goals

- No cloud deploy — local docker-compose relay (or in-process `TestServer`) only.
- No localstack / DynamoDB — the offline queue stays in-memory (planned feature, out of scope).
- No performance or load testing — this is correctness/behavior verification.
- No committing the compiled WASM binary — build on demand per #24.
- Layer B's MLS assertions are gated on #28; until then the plugin/Playwright fixtures
  assert the AES-PSK + fail-closed behavior that actually exists rather than shipping a stub.
