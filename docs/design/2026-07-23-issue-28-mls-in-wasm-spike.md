---
title: "Spike: MLS in WASM — openmls→wasm vs JS-MLS vs mls-rs (#28)"
date: 2026-07-23
status: "spike complete — Route 1 (openmls→wasm) confirmed end-to-end (Stage 0 compile + Stage 1 headless-browser run both GREEN, epoch=1, zero clock errors)"
issue: 28
supersedes_premise: "ISSUE-TRIAGE-2026-07-23 §3 wrongly claimed openmls was already a collab-wasm dep"
---

# Spike: Bringing real MLS to the WASM client (#28)

## TL;DR

Route 1 (compile the existing `openmls` stack to wasm) is recommended. The deciding reason is interop-by-construction with the native `openmls` clients — same library, wire format, and cipher suite on both sides, so the cross-implementation interop risk that weakens both other routes simply does not exist. It is pending exactly ONE experiment: a headless-Chrome run-test of a `create → KeyPackage → Welcome → StagedWelcome join → Commit → epoch-advance` flow, because openmls builds wasm in CI but does not run it in a browser. If that experiment fails, the fallback is Route 3 (AWS `mls-rs`), the only route whose CI already runs the full crypto stack in a browser.

## Verified local ground truth

Findings from this session's recon at HEAD ~`b49b7ef`:

- `crates/collab-wasm/Cargo.toml` depends ONLY on `aes-gcm = "0.10"` for crypto (no `openmls`). It carries the comment "For simplified encryption (MVP) - will be replaced with MLS later". This corrects the 2026-07-23 triage, which claimed `openmls` was already a `collab-wasm` dependency.
- `crates/collab-core/Cargo.toml` uses `openmls` 0.7 (`default-features = false`), `openmls_rust_crypto` 0.4, and `openmls_basic_credential` 0.4; `crates/collab-core/src/mls.rs:11` uses cipher suite 1 with `OpenMlsRustCrypto` + an in-memory `MemoryStorage`.
- The workspace root already forces `getrandom` with the `js` feature and sets `wasm32-unknown-unknown` rustflags (`.cargo/config.toml`), and the #24/#26 spike proved AES-256-GCM already round-trips in the compiled `.wasm` under `wasm-pack --target web` — so the web-target RNG/entropy question is already settled.
- `crates/collab-wasm/src/lib.rs` exposes an AES-GCM PSK surface (`set_encryption_key`, `has_encryption_key`, `encrypt`, `decrypt`, `encode_state_encrypted`, `apply_update_encrypted`); NO `init_mls`/`process_welcome`/`create_commit` exist.

## Adversarial verification note

This research was adversarially verified. One load-bearing claim was corrected:

- The finder's "`openmls_rust_crypto` is pure RustCrypto with no native C" claim was **refuted as stated**: the tree actually pulls the Cryspen `libcrux`/HACL* family via `hpke-rs`. However, the *safety conclusion* holds — `libcrux` is pure Rust (a HACL* port, not C FFI), carries its own `wasm32-unknown-unknown` CI (cryspen/libcrux `.github/workflows/wasm-and-no-std-checks.yml`), and this repo's `Cargo.lock` contains no `ring`/`openssl`/`cc`/`cmake`. The recommendation does not rest on the refuted "pure RustCrypto" label.
- Also verified against `Cargo.lock`: the pinned versions are `openmls_rust_crypto` 0.4.4 / `openmls` 0.7.4 today; the maintained wasm path is 0.8.1 / 0.5.x and needs MSRV 1.87→1.91.

## Options considered

### Route 1 — Compile openmls to wasm (openmls→wasm)
Build the browser MLS client on the same `openmls` + `openmls_rust_crypto` stack `collab-core` already uses, exposed through a project-authored `wasm-bindgen` surface modeled on openmls's own first-party `openmls-wasm` reference crate. Group state lives in the in-memory `MemoryStorage` provider; only TLS-serialized blobs (KeyPackage, Welcome, Commit) cross the JS boundary.

### Route 2 — A JavaScript/TypeScript MLS library (ts-mls)
Leave `collab-wasm` as CRDT-only and run real RFC 9420 MLS in TypeScript in the plugin via `ts-mls` (LukaJCB), a pure-TS, MIT-licensed implementation that covers the full group lifecycle, passes the official mlswg test vectors, and supports the same cipher suite the native clients use.

### Route 3 — Compile AWS mls-rs to wasm (mls-rs→wasm)
Adopt `awslabs/mls-rs` with the `mls-rs-crypto-rustcrypto` provider (`--features browser`) for the browser client. mls-rs has the strongest wasm story of the three — its CI runs headless-browser tests on the full stack — but the CI-proven path is async, and getting single-implementation interop realistically means also rewriting native `collab-core` onto mls-rs.

## Comparison

| Route | Compiles/runs in wasm | RFC 9420 completeness | Interop with native openmls clients | Maturity (2025-2026) | License | Migration cost | Verdict |
|---|---|---|---|---|---|---|---|
| **openmls→wasm** | Compiles: CI build-verified for `wasm32-unknown-unknown` + `wasm-pack build --target web`. **Run-tested in a browser: NOT in upstream CI** (openmls README lists wasm as "Unsupported, but built on CI") — must self-verify | Full lifecycle — reference crate exercises KeyPackage / Welcome / Commit / StagedWelcome / merge_pending_commit / process_message / epoch export | **By construction** — identical library, wire format and cipher suite (suite 1) on both sides; no cross-impl bet | Core mature (0.8.1, 2026-02); `openmls-wasm` binding is an explicit *experiment* (`publish=false`, v0.1.0) | MIT | **Medium** — reuse `collab-core` MLS logic, author a wasm-bindgen surface, bump 0.7→0.8 + MSRV 1.87→1.91, enable `js` feature | **RECOMMENDED** (pending one browser run-test spike) |
| **ts-mls** | Runs in browsers today (pure TS, no wasm compile question) | Full lifecycle + passes official mlswg RFC 9420 test vectors | Manual mlswg gRPC harness vs OpenMLS exists but is **NOT CI-gated** in ts-mls; native↔browser is a Rust↔TS cross-language seam neither project tests continuously | Young (repo 2025-04, ~105 stars, no security audit, v2.0 breaking release imminent) | MIT | **Medium-high** — a *second* MLS implementation in a second language + new TS handshake; revives the "logic duplicated across TS and Rust" failure class, now on crypto | Not recommended |
| **mls-rs→wasm** | **Strongest** — dedicated CI *runs* `wasm-pack test --headless --chrome` on the full stack incl. the rustcrypto provider. Caveat: proven path is **async** (`--cfg mls_build_async`) | 100% RFC 9420 conformance (self-claimed) | **Untested** — mls-rs CI only self-interops; realistic fix is rewriting native `collab-core` onto mls-rs for a single implementation | Mature, active (0.55.2, 2026-06), AWS-backed; wasm providers flagged "Experimental" | Apache-2.0 OR MIT | **High** — from-scratch rewrite of native `mls.rs` *and* the wasm client onto mls-rs, plus an async ripple through core | Fallback if the Route 1 spike fails |

## Recommendation

**Adopt Route 1: compile the existing `openmls` stack to wasm and give the browser client a project-owned `wasm-bindgen` surface.** Delete the AES-256-GCM pre-shared-key path outright — do not keep it as a fallback (closes #27 by construction).

The four strongest, CONFIRMED-backed reasons:

1. **Interop with the native clients is free, not a bet.** `collab-core` already runs `openmls` on cipher suite 1 (`crates/collab-core/src/mls.rs:11`), and openmls's own `openmls-wasm` crate runs the identical Welcome/Commit/epoch path on the same suite. A single implementation and wire format on both sides eliminates the cross-implementation interop risk that is the central, *untested* weakness of both Route 2 (ts-mls↔openmls) and Route 3 (mls-rs↔openmls). Verified: openmls-wasm `src/lib.rs` uses `StagedWelcome::new_from_welcome`, `merge_pending_commit`, `process_message`, `export_secret` (github.com/openmls/openmls/blob/main/openmls-wasm/src/lib.rs, fetched 2026-07-23).

2. **A first-party reference binding already exists to copy.** openmls ships `openmls-wasm` as a workspace member built via `wasm-pack build --target web`, exercising the complete RFC 9420 lifecycle. It is a reference to model on (not a dependency to import), which collapses the design work to "wrap logic we already have" (github.com/openmls/openmls/blob/main/openmls-wasm/, 2026-07-23).

3. **The historic wasm blocker does not apply — nothing links native C.** This repo's `Cargo.lock` contains no `ring`, no `openssl`/`openssl-sys`, and no `cc`/`cmake`/`bindgen` build-script crates. *Correction to the source research:* the tree is **not** "pure RustCrypto" — `hpke-rs` pulls in the Cryspen `libcrux` family — but libcrux is pure Rust (a HACL* port, not C FFI) and carries its own `wasm32-unknown-unknown` CI, so wasm linkage is not blocked. The load-bearing safety conclusion survives even though the "pure RustCrypto" label is refuted; the recommendation does not rest on that refuted label.

4. **openmls treats wasm as a maintained CI target and the RNG question is settled for the web target.** `build.yml` builds `-p openmls -F js` for `wasm32-unknown-unknown` (green on main, 2026-07-23) and `wasm-bench.yml` runs `wasm-pack build --target web` on `openmls-wasm`; the `js` feature wires `getrandom` → `crypto.getRandomValues` and `web-time` for clocks. AES-256-GCM already round-trips in the current compiled `.wasm`, confirming entropy is reachable on the web target.

**Honest limit on the confidence, and the one experiment that locks it.** openmls's CI *builds* the crypto provider to wasm but does **not run** it in a browser (its README calls wasm "Unsupported, but built on CI"), and there is an open wasm `SystemTime::now()` panic on the external-commit join path (issue #1983, OPEN 2026-03-26). mls-rs, by contrast, *runs* headless-Chrome tests. So before committing code, the first PR must run the decisive experiment:

> **Compile `openmls_rust_crypto` + a minimal `MlsGroup` create → KeyPackage → Welcome → `StagedWelcome` join → Commit → epoch-advance flow to `wasm32-unknown-unknown` with the `js` feature and execute it under `wasm-pack test --headless --chrome`.**

If it runs green on the **standard Welcome/StagedWelcome join path** (the path openmls-wasm itself uses, which sidesteps the #1983 external-commit `SystemTime` panic), Route 1 is confirmed end-to-end and interop-by-construction makes it the clear winner. If that browser run-test cannot be made to pass, fall back to **Route 3 (mls-rs)** — the only route whose CI already runs the full crypto stack in a browser — accepting its higher migration cost (native rewrite + async).

## Stage 0 result (2026-07-24) — pinned stack COMPILES to wasm

A throwaway compile probe (Rust 1.87, no installs, repo untouched) settled the cheapest question ahead of the browser run-test: **does the currently-pinned openmls stack compile to `wasm32-unknown-unknown`?**

**GREEN.** Resolved versions unchanged from `Cargo.lock` — openmls 0.7.4, openmls_rust_crypto 0.4.4, openmls_basic_credential 0.4.1. No `ring` in the tree (libcrux/RustCrypto, no native-C linkage), no MSRV error at 1.87. The native lifecycle round-trip (Alice create → Bob KeyPackage → `add_members`/Welcome → `StagedWelcome` join → encrypt→decrypt, epoch==1) passes, and both `cargo build --target wasm32-unknown-unknown --release` and `wasm-pack build --target web` finish clean.

**Correction to this doc's "next PR" prerequisites.** The compile does NOT require the openmls 0.7→0.8.1 bump or MSRV 1.87→1.91 that the Recommendation/Open-questions sections list. The only mandatory change is a **one-line feature flag**:

    openmls = { version = "0.7", default-features = false, features = ["js"] }

openmls 0.7.4 has its **own** `js` feature, distinct from `getrandom`'s. `getrandom/js` fixes entropy (already known-good — AES-GCM round-trips in the compiled `.wasm` today); openmls's `js` fixes the **clock** — KeyPackage *lifetime validation* uses `SystemTime`, pulled via `fluvio_wasm_timer` only when openmls's `js` is set. Without it the wasm build fails at `error[E0432]: unresolved import fluvio_wasm_timer`.

**This relocates the residual risk from RNG to the clock.** `fluvio_wasm_timer` reads wall-clock via JS `Date` — the same failure class as openmls #1983. The Stage-1 headless-Chrome run-test's sharpest target is therefore: does KeyPackage lifetime validation behave with a real browser clock? (The 0.8.1 + MSRV 1.91 move remains an optional, separate decision — worth it for the maintained wasm path, but NOT a blocker to prove the lifecycle runs.)

## Stage 1 result (2026-07-24) — MLS lifecycle RUNS in a real headless browser

The decisive experiment this spike hinged on. A `#[wasm_bindgen]`-exported `run_lifecycle_wasm()` (create → KeyPackage → `add_members`/Welcome → `StagedWelcome` join → encrypt "hello" → decrypt → epoch advance) was compiled with `wasm-pack build --target web` and executed in a real headless browser.

**GREEN.** Result: `{"ok":true,"epoch":1}`, console `PROBE_OK epoch=1`, in Chromium 147.0.7727.15. epoch=1 is exactly correct after the one join commit.

**The meaningful signal is the *absence* of clock errors.** No `SystemTime` / `fluvio_wasm_timer` / `Date` / KeyPackage-lifetime error appeared. openmls's KeyPackage lifetime validation requires a working wall-clock in wasm, and it passed against the browser's real clock. This means the openmls #1983 class of `SystemTime` panic does **not** bite on the standard Welcome/StagedWelcome join path (with the `js` feature) — which is the path this project would use. #1983 concerns the `external_commit` path, which we avoid.

**Two findings about the method, for whoever runs this next:**
1. The Stage-0 probe's entry point was a plain `pub fn` with no `#[wasm_bindgen]`, so `wasm-pack` dead-code-eliminated openmls entirely — a hollow 14 KB artifact with no export. Adding `#[wasm_bindgen] pub fn run_lifecycle_wasm() -> Result<u64, JsError>` retained and exposed it → a 1.27 MB linked artifact. **A wasm run-test must assert against a non-hollow artifact** (check exported symbol / byte size), or it is a false GREEN.
2. It ran via Playwright's **bundled** headless Chromium (`chrome-headless-shell`, cache build 1217) driven by a direct `chromium.launch()` script — no system Chrome, no chromedriver, no browser download, and **Rust 1.91 was not needed** (the pinned 0.7.4 stack ran as-is).

**Net verdict (both stages green): Route 1 is confirmed end-to-end** — the pinned openmls stack both compiles to wasm (Stage 0, one-line `js` feature) and runs the real MLS group lifecycle correctly in-browser (Stage 1). The first implementation PR can proceed with high confidence; the 0.7→0.8.1 / MSRV-1.91 move remains optional maintained-path hygiene, not a blocker.

## Open questions / next PR

**First PR (spike / de-risk — no production code):**
- Run the headless-browser lifecycle test above; capture the result in this spike doc.
- **(revised by Stage 0 — a bump is optional, not required to compile; see the Stage 0 result section above)** Confirm the version/toolchain move: `openmls` 0.7→0.8.1, `openmls_rust_crypto` 0.4→0.5.x, `getrandom` js/`wasm_js` wiring, and **MSRV 1.87→1.91** (openmls's declared MSRV; the repo just pinned 1.87 in b49b7ef, so this is a real gate).
- Measure the compiled `.wasm` size delta vs today's AES-GCM-only module (openmls runs a size CI check for a reason).

**Subsequent PRs (staged):** (2) refactor `collab-core` MLS into a reusable module so the wasm crate does not become a second copy; (3) add the wasm-bindgen MLS surface *alongside* the AES path and prove a real Welcome/Commit/epoch group with a real-compiled-wasm test (ties to #26 — do not trust green Jest unit tests per the "Jest-ESM-masks-typecheck" hazard); (4) move the key-package/Welcome/Commit exchange + real epoch onto the relay wire; (5) delete the AES-256-GCM path, `encryption_key`, `set/has/encrypt/decrypt`, the `encryptionKey` config, the all-zeros placeholder, and replace hardcoded `epoch:0` with `group.epoch()` (closes #27); (6) persistence #30 as its own issue.

**What remains unproven regardless of route (application-layer, and it is the real work):**
- **Epoch/commit coordination over a zero-knowledge relay.** RFC 9420 makes per-epoch secrets distinct; nothing today orders Commits ahead of the application messages that depend on the new epoch, and native `decrypt` never consults the `epoch` field it carries. This is unsolved *natively too*.
- **Cross-process MLS join is broken in the native code today** (`commands.rs:184-199`: file-based join "fails" because key-package state isn't persisted across processes). The browser case is inherently cross-process, so this must be built, not ported.
- **Persistence (#30)** must be *snapshot/restore* of the synchronous in-memory `MemoryStorage` into async IndexedDB (encrypted at rest), **not** an IndexedDB-backed `StorageProvider` — the trait is synchronous (53 sync methods) and IndexedDB is async.

## Evidence appendix

- **openmls wasm CI (build):** github.com/openmls/openmls/blob/main/.github/workflows/build.yml — `wasm32-unknown-unknown` in matrix, builds `-p openmls -F js`; green on main (fetched 2026-07-23).
- **openmls-wasm reference crate:** github.com/openmls/openmls/blob/main/openmls-wasm/{Cargo.toml,build.sh,src/lib.rs} — full lifecycle via `wasm-pack build --target web`; `publish=false`, v0.1.0, README labels it an experiment (2026-07-23).
- **openmls README wasm status:** raw.githubusercontent.com/openmls/openmls/main/README.md — `wasm32-unknown-unknown` under "Unsupported, but built on CI" (built ≠ run-tested) (2026-07-23).
- **openmls issue #1983 (OPEN):** github.com/openmls/openmls/issues/1983 — `SystemTime::now()` panics on wasm via the external-commit / `MessageSecretsStore` path; avoidable via standard Welcome join + `js`/`web-time` (updated 2026-03-26).
- **openmls versions / MSRV:** crates.io max_stable 0.8.1 (2026-02-13); workspace `rust-version = "1.91.0"`. Repo pins `openmls "0.7"` / `openmls_rust_crypto "0.4"` and MSRV 1.87 (commit b49b7ef).
- **Crypto tree (no ring/C; libcrux is Rust):** local `Cargo.lock` — no `ring`/`openssl`/`cc`/`cmake`; `hpke-rs`→`hpke-rs-libcrux`+`libcrux-*`. libcrux wasm CI: cryspen/libcrux `.github/workflows/wasm-and-no-std-checks.yml` (PR #1088). *"Pure RustCrypto" claim is inaccurate; "no native C, wasm not blocked" holds.*
- **ts-mls:** registry.npmjs.org/ts-mls — v1.6.2, MIT, single dep `@hpke/core`; github.com/LukaJCB/ts-mls — full lifecycle, official mlswg vectors, `interop/` harness vs OpenMLS **not** referenced in `ci.yml`; README "not undergone a formal security audit"; supports suite 1 (all fetched 2026-07-23). v2.0.0-rc line active (breaking).
- **@wireapp/core-crypto (openmls-in-wasm, GPL-3.0):** npm v10.1.1, license GPL-3.0 — copyleft likely disqualifying (2026-07-21).
- **mls-rs:** github.com/awslabs/mls-rs — Apache-2.0 OR MIT, "100% RFC 9420 conformance" (self-claimed), no 3rd-party audit; crates.io 0.55.2 (2026-06-17). `wasm_build.yml` runs `wasm-pack test --headless --chrome` on the full stack with `RUSTFLAGS=--cfg mls_build_async` (async path). `mls-rs-crypto-webcrypto` supports only suites 2/5/7 (no suite 1) → must use `mls-rs-crypto-rustcrypto --features browser`. `interop_tests.yml` self-interops only (fetched 2026-07-23).
- **MLS registry:** raw.githubusercontent.com/mlswg/mls-implementations/master/implementation_list.md — both OpenMLS and mls-rs listed at RFC status; neither CI cross-tests the other.
- **RFC 9420:** rfc-editor.org/rfc/rfc9420.html §2/§3/§6 — untrusted Delivery Service; application data as `PrivateMessage`; per-epoch secret trees. Relay-as-zero-knowledge is correct for any route.
- **Local ground truth:** `collab-wasm/src/lib.rs` (AES-256-GCM PSK, `encryption_key`, epoch semantics absent); `collab-core/src/mls.rs:11` (suite 1); `collab-core/src/encryption.rs:70-85` (MLS-encrypts `encode_state()` bytes — the template, though it sends full state, not deltas); `collab-core/src/commands.rs:184-199` (cross-process join unimplemented) — all read 2026-07-23.

## Relationship to #27

#27 (replace the all-zeros placeholder key) is only PARTLY blocked by this spike. #27's fail-closed guard — rejecting the all-zeros key — is independent of MLS and lives at the root-cause site `validateConfig()` in `plugins/obsidian-ee/src/collab-client.ts:80-108` (which today enforces 32-byte length but ACCEPTS all-zeros; every caller routes through it). Only #27's real key-exchange half is superseded by this MLS work — when Route 1 lands and the AES PSK path is deleted, the key-exchange becomes MLS Welcome/Commit and #27 closes by construction. So a fail-closed hardening of #27 can ship independently ahead of #28.
