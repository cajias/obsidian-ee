# Design: Unblock Clean Build (#24) + Real Compiled-WASM Behavior Tests (#26) — BDD-first

**Status:** Proposed (v3)
**Date:** 2026-07-23
**Commit:** b49b7ef
**Supersedes:** v2 (committed-artifact + babel). This version reflects two maintainer decisions:
**(A)** load real WASM under **ts-jest ESM mode, no babel**; **(B)** **build the WASM on demand** (do not commit the binary).
Both were validated empirically in a clean worktree at `b49b7ef` — see §5.2.

---

## 1. Problem

Two coupled gaps in `plugins/obsidian-ee`:

**#24 — the build is blocked from a clean checkout.** The wasm-bindgen artifact under `src/wasm/` is `.gitignored` and absent on a fresh clone, so the TypeScript imports (`main.ts:2`, `collab-client.ts:1`) cannot resolve `./wasm/collab_wasm` and every suite that imports it fails to type-check (TS2307) before its mock runs. Nothing regenerates it automatically: `package.json` has no prebuild step and `ci.yml` runs no Node/wasm step.

**#26 — the compiled artifact and the JS↔Rust boundary are untested.** The underlying logic *is* covered natively:

- `collab-core` (Rust): **111 tests** — MLS, CRDT, encryption, vault-sync.
- `collab-wasm` (Rust): **9 native `#[test]`s** exercising the `*_internal` methods.

What no test touches is the **compiled `.wasm` binary as JavaScript sees it**: the wasm-bindgen glue, `__wbg_ptr` handle ownership, `Vec<u8>` → fresh `Uint8Array` copy-out, `u32` argument coercion, `getrandom`→`crypto.getRandomValues` entropy, and the `From<CollabError> for JsValue` path that turns Rust errors into **thrown plain `{type, message}` objects** (not `Error` instances). All 5 WASM-touching plugin suites do `jest.mock('../wasm/collab_wasm')`, and `wasm-integration.test.ts` is **8 `it.todo()`** stubs — the intended real-WASM slot, currently empty. The gap is specifically the *compiled binary + the marshalling boundary*, nothing below it.

The plugin's own load path is already node-compatible — `main.ts:87-104` does `WebAssembly.compile(readFileSync(collab_wasm_bg.wasm))` then `init(compiledModule)`. A node test replicates this exactly. **No browser/Electron/jsdom harness is required** (verified, §5.2).

---

## 2. Why this scope, and the tradeoff decision (A)+(B) forces

- **#24 is a prerequisite for #26.** The real-WASM tests can only `import` the glue once the artifact exists on disk. Unblock first, then test.
- **Decision (B) reframes #24's acceptance — stated honestly.** #24's original text asks that `npm ci && npm test` work "with no manual build step / no Rust toolchain." **Keeping the binary out of git makes that impossible by construction:** the artifact must be built from source, which needs `rustc` + `wasm-pack` + the `wasm32-unknown-unknown` target. So under (B) the clean-checkout contract becomes **`rustup target add wasm32-unknown-unknown` → `./scripts/build-wasm.sh` → `npm ci` → `npm test`**, and the Rust/wasm toolchain is a **documented prerequisite**, not an eliminated one. This is the deliberate cost of honoring "don't commit binaries." The v2 alternative (commit the ~433 KB artifact) removed the toolchain requirement but put an un-reviewable binary in a crypto repo; the maintainer chose source-of-truth over convenience.
- **On-demand build eliminates the stale-binary problem entirely.** There is no committed binary to drift from source, so the v2 "drift-guard CI job" question is moot — the artifact is always freshly built from the audited Rust. The real-WASM behavior tests still add value beyond the 120 native tests: they pin the *JS↔Rust boundary* (copy-out semantics, thrown-object error contract, handle ownership) that `cargo test` cannot reach.

---

## 3. Goals / Non-goals

**Goals**
- G1 (#24): After `rustup target add wasm32-unknown-unknown && ./scripts/build-wasm.sh`, a fresh checkout runs `npm ci && npm test` green — no committed binary, no manual per-file steps.
- G2 (#24): CI regenerates the artifact from source on every run (`build-wasm.sh` before `npm test`), so tests always run against source-current wasm.
- G3 (#26): Every public `CollabCore` / `YDocument` method and every JS-observable error path is exercised against the **real compiled binary**.
- G4 (#26): CRDT convergence and encrypted cross-instance sync are pinned as behavioral (not spy) assertions.
- G5: `npm test` runs one lane — real-WASM suites + the migrated mocked suites — under ts-jest ESM.

**Non-goals**
- No committed wasm binary (decision B).
- No babel (decision A — ESM makes it unnecessary; see §5.2).
- No `greet` export (does not exist; see §10).
- No jsdom/Electron/browser runner.
- No `--target nodejs` test build (we test the one `--target web` artifact the plugin ships).
- No CI hash-diff/rebuild-compare job (no committed binary to compare).
- No perf/linear-memory-grow test; no JS-number-coercion fuzzing beyond one boundary case.

---

## 4. Design Part A — On-demand build + Rust-free-free CI (#24)

The artifact stays gitignored and is built from source. The work is (a) making the build reproducible in CI and (b) documenting the prerequisite.

| Change | File | Why |
|---|---|---|
| Add `rustup target add wasm32-unknown-unknown` before the build | `scripts/build-wasm.sh` (or CI setup) | **Verified gap:** the script installs `wasm-pack` if absent but assumes the wasm32 target exists; a clean box does not have it. Without this, `build-wasm.sh` fails on a fresh CI runner |
| New `plugin` CI job: `rustup target add wasm32-unknown-unknown` → `./scripts/build-wasm.sh` → `npm ci` → `NODE_OPTIONS=--experimental-vm-modules npm test` → `npm run build` | `.github/workflows/ci.yml` | Institutionalizes G1/G2 on a pristine `actions/checkout` tree; pre-satisfies most of #23. Needs the Rust toolchain (matching the repo MSRV `1.87.0`) since the build is from source |
| Document the prerequisite + regenerate step | `docs/development.md` | State that `src/wasm/` is built, not committed; `wasm-pack` + `wasm32-unknown-unknown` are required before `npm test`; rerun `build-wasm.sh` after any `collab-wasm` change |
| `.gitignore` unchanged | `plugins/obsidian-ee/.gitignore` | `src/wasm/` stays ignored (decision B) |

**Build cost (measured):** `build-wasm.sh` in a clean worktree ≈ **40 s** wall (release compile of the aes-gcm/yrs/wasm-bindgen tree + `wasm-opt`); cached rebuilds are faster. This is the per-clean-checkout / per-CI-run cost of decision (B).

---

## 5. Design Part B — Real-WASM loader harness under ts-jest ESM (#26)

### 5.1 Harness

One util, `src/__tests__/helpers/load-real-wasm.ts`, mirrors `main.ts:87-104` **exactly**: `readFileSync(collab_wasm_bg.wasm)` → `WebAssembly.compile(bytes)` → `init(module)`, importing the *same* glue module `main.ts` imports. Idempotent via a module-level `initialized` flag (each jest suite gets its own module registry, so `init` runs once per file).

**Entropy knob.** `getrandom` under `--target web` calls `crypto.getRandomValues`, not OS entropy. Node 20+ exposes `globalThis.crypto`, but the helper defensively assigns it from `node:crypto.webcrypto` if absent, so `encrypt` / `encode_state_encrypted` never hit a `getrandom` RuntimeError on a thin host.

```ts
// src/__tests__/helpers/load-real-wasm.ts
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { webcrypto } from 'node:crypto';
import init, { CollabCore } from '../../wasm/collab_wasm';

// getrandom (wasm-pack --target web) calls crypto.getRandomValues, not OS entropy.
// Guard thin hosts so encrypt() never surfaces a getrandom RuntimeError.
if (!(globalThis as { crypto?: Crypto }).crypto) {
    (globalThis as { crypto?: Crypto }).crypto = webcrypto as unknown as Crypto;
}

let initialized = false;

/**
 * Load and initialize the REAL built WASM artifact, mirroring main.ts:87-104.
 * Throws ENOENT if the artifact was not built (run scripts/build-wasm.sh first) —
 * which is exactly the clean-checkout precondition for decision (B).
 */
export async function loadRealWasm(): Promise<{ CollabCore: typeof CollabCore }> {
    if (!initialized) {
        const wasmPath = join(__dirname, '..', '..', 'wasm', 'collab_wasm_bg.wasm');
        const bytes = readFileSync(wasmPath);            // == main.ts:88
        const module = await WebAssembly.compile(bytes); // == main.ts:93
        await init(module);                              // == main.ts:104 (harmless deprecation warn, §5.2)
        initialized = true;
    }
    return { CollabCore };
}

/** Convenience: a fresh core per test (constructor is cheap once the module is loaded). */
export async function newCore(): Promise<InstanceType<typeof CollabCore>> {
    const { CollabCore: Core } = await loadRealWasm();
    return new Core();
}
```

### 5.2 Verified feasibility (ran real jest + real build in a clean worktree)

A feasibility spike executed both decisions end-to-end at `b49b7ef` (node v24.14.1, `rustc`/`cargo` 1.87.0, `wasm-pack` 0.15.0):

- **Decision (B) works.** `build-wasm.sh` produced all 5 artifacts in ~40 s after adding the wasm32 target. `wasm-pack` is present/installable.
- **Decision (A) works — real WASM loads under ts-jest ESM, no babel.** A smoke test mirroring `main.ts:87-104` (compile + `init(module)`, then key-set + encrypt/decrypt round-trip) passed:
  ```
  PASS src/__tests__/wasm-smoke.test.ts
    ✓ round-trips encrypt/decrypt with a real key (11 ms)
  ```
  Under genuine ESM, the glue's `import.meta.url` (line 492) **parses fine** and is never executed (the `init(module)` path skips the `fetch`/`import.meta.url` branch). The only noise is a non-fatal `using deprecated parameters` stderr warning from the single-arg `init(module)` call — optional cleanup: `init({ module_or_path: module })`.
- **`tsconfig.json` needs NO change** — it already sets `module: 'ESNext'` + `esModuleInterop: true`, exactly the ESM-friendly shape required.

**Working jest ESM config** (new file `plugins/obsidian-ee/jest.esm.config.mjs`; kept as a separate `.mjs` so the existing CJS `jest.config.js` is not forced to change form):

```js
export default {
  preset: 'ts-jest/presets/default-esm',
  testEnvironment: 'node',
  roots: ['<rootDir>/src'],
  testMatch: ['**/__tests__/**/*.test.ts', '**/*.test.ts'],
  extensionsToTreatAsEsm: ['.ts'],
  transform: { '^.+\\.tsx?$': ['ts-jest', { useESM: true, tsconfig: 'tsconfig.json' }] },
  moduleNameMapper: {
    '^@/(.*)$': '<rootDir>/src/$1',
    '^(\\.{1,2}/.*)\\.js$': '$1',   // map ESM .js specifiers back to source (defensive)
  },
  verbose: true,
};
```

Run: `NODE_OPTIONS=--experimental-vm-modules jest --config jest.esm.config.mjs` (wire into the `test` script).

### 5.3 Blast radius — migrating the existing suites to ESM (the real cost)

Baseline under the current CJS config: **137 tests, 6 suites, all green.** Under the ESM config the spike measured:

- **1 suite passes unchanged:** `wasm-integration.test.ts` (todo-only, no mocks).
- **5 suites break — all on one root cause:** `jest.mock` is **not hoisted under ESM** and the `jest` global is absent → `ReferenceError: jest is not defined` at the top-level `jest.mock(...)`. (`main.test.ts`'s secondary `Cannot find module 'obsidian'` is the same cause — its `jest.mock('obsidian')` never runs.)

| Suite | `jest.mock` sites | `require()` | Migration |
|---|---|---|---|
| `collab-client.test.ts` | 1 | 0 | mock → `unstable_mockModule` + dynamic import |
| `editor-sync.test.ts` | 1 | 0 | same |
| `encryption-integration.test.ts` | 1 | 0 | same |
| `two-user-integration.test.ts` | 1 | 0 | same |
| `main.test.ts` | 4 | 1 | 4 mocks → `unstable_mockModule`; `require('../collab-client')` (line 552) → `await import` |

**Total: 8 `jest.mock` → `unstable_mockModule` + 1 `require` → `await import`, across 5 files.** The ~330 `jest.fn`/`jest.spyOn`/`useFakeTimers` calls need no change once `jest` is imported from `@jest/globals`. Estimated ~half a day.

**Migration recipe (proven green on the editor-sync suite during the spike), 3 mechanical edits per file:**
1. Add `import { jest, describe, it, expect, beforeEach, afterEach } from '@jest/globals';`
2. `jest.mock('X', factory)` → `jest.unstable_mockModule('X', factory)`
3. Convert the static `import` of the module-under-test to a top-level `await import('X')` placed **after** the `unstable_mockModule` call (same for the one `require()` in `main.test.ts`).

### 5.4 Suite conversion plan (real-WASM vs stay-mocked)

Independent of the ESM migration above, which suites run against the **real** binary:

| Suite | Verdict | Why |
|---|---|---|
| `wasm-integration.test.ts` | **Convert** (fill all 8 todos) | The purpose-built real-WASM slot; every contract method + error path + CRDT convergence goes here |
| `two-user-integration.test.ts` | **Partial convert** | Drop the XOR-cipher mock; real `CollabCore` over the real `ws` relay it already runs. Relax key-mismatch assertion to `{type:'decryption'}` (real AEAD, not `'key mismatch'`). Replace `core2.apply_update_encrypted` spy with a `getText()` behavioral check. All 8 tests in the file flip to real WASM once the mock is deleted; the 6 non-enumerated ones (bidirectional, rapid-updates, late-joiner, disconnect/reconnect) must pass against real full-state yrs `encode_state` |
| `encryption-integration.test.ts` | **Partial convert** | Replace the XOR-marker mock with real `CollabCore`; keep `MockWebSocket`. Rewrite spy assertions as observable ciphertext/`getText` behavior. Keep the empty-key `ConfigValidationError` test (TS-only) |
| `collab-client.test.ts` | **Keep mocked** | TS state-machine/reconnect suite; assertions *are* jest spies on the core — real un-spied WASM removes exactly what they assert |
| `main.test.ts` | **Keep mocked** | Binds to the Obsidian `App`/editor API, unavailable in `testEnvironment: 'node'` |
| `editor-sync.test.ts` | **Keep mocked** | Mocks `../collab-client` (not WASM); pure Obsidian editor/EditorSync logic, no crypto |

---

## 6. BDD acceptance scenarios

Feature/Scenario/Given/When/Then. `realWASM` = runs against the built binary.

### 6.1 Build unblock (#24, under decision B)

| Scenario | Given | When | Then |
|---|---|---|---|
| Clean checkout builds wasm from source | a fresh clone at b49b7ef with `rustc`/`wasm-pack` installed, `src/wasm/` absent | `rustup target add wasm32-unknown-unknown && ./scripts/build-wasm.sh` | the 5 artifacts appear in `src/wasm/` (~40 s) |
| Fresh checkout type-checks + tests after build | the built artifact present | `npm ci && NODE_OPTIONS=--experimental-vm-modules npm test` | ESM glue loads (no `Unexpected token 'export'`, no `import.meta` error); suites run |
| Missing artifact fails loudly | `collab_wasm_bg.wasm` absent (build not run) | any real-WASM suite runs | `loadRealWasm()` throws `ENOENT`; suite errors immediately (never a silent pass) |
| CI runs the whole path on a pristine tree | the new `plugin` CI job | push/PR | build-from-source → `npm ci` → `npm test` → `npm run build` all green |

### 6.2 Module load / lifecycle

| Scenario | Given | When | Then | realWASM |
|---|---|---|---|---|
| Compiled artifact loads + exports a working constructor (replaces `greet` todo) | built `.wasm` + web glue | `loadRealWasm()` awaited | resolves; `CollabCore` is a callable ctor; a new instance has `get_text()===''` | ✓ |
| `loadRealWasm()` is idempotent within a suite | one suite calling it twice | second `loadRealWasm()` | resolves without re-`init`; same `CollabCore`; no crash | ✓ |
| Construct a live handle | initialized module | `new CollabCore()` | `get_text()===''` and `has_encryption_key()===false` (proves `__wbg_ptr` wired) | ✓ |
| Use-after-free throws | a core with `free()` called | `get_text()` on the freed handle | throws (`null pointer passed to rust`), not stale data | ✓ |

### 6.3 Basic CRDT operations

| Scenario | Given | When | Then | realWASM |
|---|---|---|---|---|
| Insert text | empty core | `insert(0,'Hello, World!')` then `insert(5,',X')` | `get_text()` reflects both at the right char offsets | ✓ |
| Out-of-range insert **clamps** | fresh core | `insert(100,'x')` on empty doc | **does NOT throw** — Yrs clamps; `get_text()==='x'`, instance stays usable | ✓ |
| Delete a range | core with `'Hello, World!'` | `delete(0,7)` | `get_text()==='World!'` | ✓ |
| Out-of-range delete **clamps** | core with `'abc'` | `delete(2,50)` past the end, then `delete(10,5)` | **does NOT throw** — `CollabCore::delete` clamps both bounds to the current text length (`get_text()==='ab'`); an index past the end is a no-op; instance stays usable, matching `insert` | ✓ |
| Unicode round-trips UTF-8→UTF-16 | empty core | `insert(0,'a😀b́')` and read back | `get_text()` equals the exact JS string (multi-byte/combining preserved across the copy) | ✓ |

### 6.4 State synchronization

| Scenario | Given | When | Then | realWASM |
|---|---|---|---|---|
| `encode_state` returns an owned snapshot | core with `'abc'` | `const s = encode_state();` then `insert(3,'d')` | `s` is a non-empty `Uint8Array` **unchanged** by the later insert (fresh copy) | ✓ |
| `apply_update` from another doc | doc A `'Hello from A'` + its `encode_state()` | doc B `apply_update(bytesFromA)` | `B.get_text()==='Hello from A'`; returns `undefined` (Ok) | ✓ |
| `apply_update` garbage → structured `sync_error` | fresh core | `apply_update(new Uint8Array([255,255,255,255]))` | throws a **plain object** `type==='sync_error'` (not `Error`); empty input same path | ✓ |
| `encode_state_vector` diff snapshot | core with `'hi'` | `encode_state_vector()` | returns a non-empty owned `Uint8Array` | ✓ |
| Two docs converge deterministically | A and B identical; A inserts `'X'` at 0, B inserts `'Y'` at end concurrently | each applies the other's `encode_state()`, either order | `A.get_text()===B.get_text()`, both contain `X` and `Y` | ✓ |

> **Not backed by current API:** a "state-vector drives a real diff-sync" scenario (B produces `sv`; A computes a *diff* for `sv`; B applies it) is intentionally omitted — the compiled WASM exposes no diff-from-state-vector encode method (only `encode_state` full-state, `encode_state_vector` vector-only, and `apply_update`). The full-state cross-apply row above covers §11 convergence instead.

### 6.5 Encryption round-trip + error paths

| Scenario | Given | When | Then | realWASM |
|---|---|---|---|---|
| Valid 32-byte key mutates THIS handle | fresh core | `set_encryption_key(new Uint8Array(32).fill(7))` | `has_encryption_key()===true` on the same instance (proves `&mut self` persisted) | ✓ |
| Wrong-length key → structured `key_error` | fresh core | `set_encryption_key(new Uint8Array(16))` | throws `{type:'key_error', message:'Key must be 32 bytes'}`; `has_encryption_key()` stays false | ✓ |
| Encrypt then decrypt round-trips | keyed core | `const c = encrypt(pt); decrypt(c)` | output equals `pt`; `c.length === 12 + pt.length + 16`; `c` differs from `pt` | ✓ |
| Encrypt with no key → `key_error` | core, no key | `encrypt(new Uint8Array([1,2,3]))` | throws `{type:'key_error', message:'No encryption key set'}` | ✓ |
| Decrypt too-short/empty → `decryption` | keyed core | `decrypt(new Uint8Array(0))` and `decrypt(new Uint8Array(8))` | both throw `{type:'decryption', message:'Ciphertext too short'}` | ✓ |
| Tampered ciphertext fails AEAD tag | keyed core + valid `c` | flip one byte past the nonce, `decrypt` | throws `{type:'decryption'}` (assert `type` only, never message — real AEAD emits `aead::Error`), not corrupted plaintext | ✓ |
| Encrypted state syncs across two instances | A and B share a 32-byte key; A has `'Encrypted sync!'` | `B.apply_update_encrypted(A.encode_state_encrypted())` | `B.get_text()==='Encrypted sync!'` | ✓ |
| `apply_update_encrypted` wrong key → `decryption` | A keyed `fill(1)` w/ text; B keyed `fill(2)` | `B.apply_update_encrypted(A.encode_state_encrypted())` | throws `{type:'decryption'}`; `B.get_text()===''` | ✓ |
| `apply_update_encrypted` no key → `key_error` | B, no key | `B.apply_update_encrypted(someBytes)` | throws `{type:'key_error'}` | ✓ |

### 6.6 Client integration (partial-convert suites)

| Scenario | Given | When | Then | realWASM |
|---|---|---|---|---|
| Real AES-GCM update over a real socket decrypts on the peer | two `CollabClient`s wrapping real `CollabCore`, shared 32-byte key, real `ws` relay | `client1.sendUpdate('Hello')`; relay broadcasts the encrypted `yrs_update` | `client2.getText()==='Hello'` (replaces the removed `apply_update_encrypted` spy) | ✓ |
| Mismatched keys fail closed | client1 key `fill(1)`, client2 key `fill(2)`, real relay | `client1.sendUpdate('Secret message')` | `client2` `onError` fires with `objectContaining({type:'decryption'})`; `receivedUpdates` empty; `client2.getText()===''` | ✓ |
| Outgoing update carries real ciphertext | `CollabClient` on real core + `MockWebSocket` | `client.sendUpdate('Hello')` | sent `yrs_update.encrypted` is a `number[]` of length `> 12`, NOT the plaintext bytes | ✓ |
| Incoming real ciphertext round-trips + fires `onUpdate` | client A produces `sentUpdate.encrypted`; client B on real core, same key | B receives `{type:'yrs_update', encrypted: sentUpdate.encrypted}` | B `onUpdate` fires; `B.getText()` equals A's plaintext | ✓ |
| Empty key rejected before WASM | a `CollabClientConfig` with a 0-byte `encryptionKey` | `new CollabClient(core, config)` | throws `ConfigValidationError` `'... exactly 32 bytes for AES-256, got 0 bytes'` | ✗ |

---

## 7. Implementation steps (RED-first TDD ordering)

1. **Unblock the build (#24).** Add `rustup target add wasm32-unknown-unknown` to `scripts/build-wasm.sh`; document the toolchain prerequisite in `docs/development.md`. RED: on a clean checkout `npm test` fails (artifact absent / TS2307). GREEN: after `build-wasm.sh`, the artifact resolves.
2. **Flip jest to ESM.** Add `jest.esm.config.mjs` (§5.2); point the `test` script at it with `NODE_OPTIONS=--experimental-vm-modules`. RED: 5 suites fail with `jest is not defined`.
3. **Migrate the 5 suites to ESM** per the §5.3 recipe (8 `jest.mock`→`unstable_mockModule` + 1 `require`→`await import`, `@jest/globals` import). GREEN: baseline 137 tests pass again under ESM.
4. **Add the loader harness (#26).** Write `helpers/load-real-wasm.ts`. RED: smoke test until the artifact is built; GREEN after step 1.
5. **Fill `wasm-integration.test.ts` (#26).** Replace all 8 `it.todo()` with §6.2–§6.5 scenarios, RED→GREEN per method. Repurpose the `greet` todo as the module-load smoke test.
6. **Convert `two-user-integration.test.ts`** — delete the XOR mock; real cores over the real `ws` relay; verify all 8 tests behaviorally.
7. **Convert `encryption-integration.test.ts`** — real core; rewrite spy assertions as observable ciphertext/`getText`; keep the empty-key config test.
8. **Wire CI** — the `plugin` job (§4): build-from-source → `npm ci` → `npm test` → `npm run build`.

---

## 8. Test plan

- One lane: `NODE_OPTIONS=--experimental-vm-modules npm test` runs migrated mocked suites + real-WASM suites together under ts-jest ESM.
- Every §6 `realWASM: ✓` scenario is a real `it()` against the built binary.
- CI: `rustup target add wasm32-unknown-unknown` → `build-wasm.sh` → `npm ci` → `npm test` → `npm run build`. Because the binary is built from source each run, tests always reflect current `collab-wasm` source.
- Coverage target ≥ 80% holds; the boundary is now genuinely exercised, not mocked.

---

## 9. Risks

| Risk | Mitigation / status |
|---|---|
| Decision (B) requires the Rust/wasm toolchain for `npm test` | **Accepted, explicit tradeoff** (§2). Documented prerequisite; CI installs it. This is the cost of not committing the binary |
| `build-wasm.sh` fails on a clean CI runner (missing wasm32 target) | **Verified gap** — add `rustup target add wasm32-unknown-unknown` to the script/CI (§4) |
| ESM flip breaks 5 of 6 suites (`jest.mock` not hoisted) | **Measured** — 8 mocks + 1 require across 5 files; mechanical recipe proven green on one suite (§5.3). ~half a day |
| `NODE_OPTIONS=--experimental-vm-modules` friction | Standard jest-ESM requirement; encapsulated in the `test` script |
| `getrandom` RuntimeError on a thin host | Harness assigns `globalThis.crypto` from `node:crypto.webcrypto` if absent (§5.1) |
| `init(module)` deprecation warning on stderr | Non-fatal; optional `init({ module_or_path })` cleanup. Only trips a suite asserting clean stderr |
| ~40 s build per clean checkout / CI run | Accepted cost of on-demand build; cached rebuilds faster |
| 6 carried-over `two-user-integration` tests unenumerated | Verify each against real full-state yrs `encode_state` during conversion (step 6), not implicitly |

---

## 10. Deliberate simplifications (YAGNI)

1. **No babel.** ESM makes `import.meta.url` valid and loads the `--target web` glue natively (§5.2) — the entire v2 babel toolchain (3 devDeps + `babel.config.js` + `--legacy-peer-deps`) is deleted. *Add only if* a future dep forces CJS-only jest.
2. **No committed binary / no `greet` export / no jsdom / no `--target nodejs` / no hash-diff CI job** — per §3 Non-goals. On-demand build removes the drift-guard question entirely (no committed binary to drift).
3. **`collab-client` / `main` / `editor-sync` stay mocked** — the first is a TS state-machine/spy suite where real un-spied WASM removes what it asserts; the latter two bind to the Obsidian `App`/editor API absent in node. Real WASM adds nothing.
4. **No oversized-input / linear-memory-grow perf test, no JS-number-coercion fuzzing.** The copy-vs-view snapshot and clamping scenarios already pin the risky boundary semantics. Add on a real bug.
5. **Both `insert` and `delete` clamp out-of-range input.** Out-of-range `insert` *clamps* (Yrs clamps the index; no throw). Out-of-range `delete` now clamps too: `CollabCore::delete` bounds-checks `index`/`length` against `self.text.len(&txn)` before calling `remove_range` (index past the end → no-op; over-length → clamped to the text end), so no throw and the instance stays usable. This bounds-check was **added in this change** — previously `delete` had none, so `yrs` `remove_range` panicked on a range past the end and surfaced across the wasm boundary as a raw `WebAssembly.RuntimeError` (`'unreachable'`) that poisoned the instance. That bug is now fixed in the crate. Use-after-free (covered) remains the other genuine cross-boundary throw.
6. **`jest.config.js` (CJS) is left in place**, ESM config added as a separate `.mjs`. *Collapse to one* only if all suites end up ESM and the CJS config is provably unused.

---

## 11. Acceptance criteria

**#24 (unblock clean build — under decision B)**
- [ ] `docs/development.md` documents the `wasm-pack` + `wasm32-unknown-unknown` prerequisite and the `build-wasm.sh`-before-`npm test` step.
- [ ] `scripts/build-wasm.sh` adds the wasm32 target (or CI does) so it succeeds on a clean runner.
- [ ] New `plugin` CI job: build-from-source → `npm ci` → `npm test` → `npm run build`, green on a pristine checkout.
- [ ] Artifact stays out of git (`src/wasm/` still ignored).

**#26 (real compiled-WASM behavior tests)**
- [ ] `helpers/load-real-wasm.ts` loads the built binary in `testEnvironment: 'node'` under ts-jest ESM, no babel.
- [ ] `wasm-integration.test.ts`: all 8 `it.todo()` replaced with real `it()` scenarios (§6.2–§6.5); every public method + error path covered against the real binary.
- [ ] `two-user-integration.test.ts` + `encryption-integration.test.ts` converted to real `CollabCore`; spy assertions rewritten as behavioral; key-mismatch relaxed to `{type:'decryption'}`.
- [ ] CRDT convergence + encrypted cross-instance sync pinned as behavioral assertions.
- [ ] Baseline 137 tests still green after the ESM migration (8 mocks + 1 require converted across 5 files).
- [ ] `npm test` passes the full lane under `NODE_OPTIONS=--experimental-vm-modules`.
