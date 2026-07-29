# Issue #28 — MLS in WASM: the collab-core surface for browser/Obsidian clients

Date: 2026-07-29
Branch: `feat/28-wasm-mls-surface`
Status: implemented

## Problem

The Obsidian/browser client talks to the relay through `collab-wasm`, which today
only exposes a standalone AES-256-GCM `CollabCore`. Real end-to-end encryption
lives in `collab-core::EncryptedDocument` (Yrs CRDT + MLS, RFC 9420) and has never
been reachable from JavaScript. Issue #28 is: expose the *already-tested* MLS engine
across the wasm-bindgen boundary so a browser client can create a group, invite
members, and exchange encrypted CRDT updates — without reimplementing any crypto.

## Architecture decision

Reuse, do not reimplement. `collab-core` already has a complete, tested MLS engine:

- `EncryptedDocument::create(doc_id, user_id)` — start a group as owner.
- `EncryptedDocument::join(&Invite, PendingMember)` — join via a Welcome (consumes
  the pending member by value).
- `create_invite(&mut self, key_package)` -> `Invite { doc_id, welcome, commit, epoch }`.
- `get_encrypted_update()` -> `EncryptedOp { ciphertext, epoch }`;
  `apply_encrypted_update(&EncryptedOp)`.
- `process_commit(&[u8])`, `insert`, `delete`, `get_content`, `epoch`.
- `MlsDocumentGroup::generate_key_package(user_id)` -> `PendingMember`;
  `PendingMember::key_package()` -> `&[u8]`.

`collab-wasm` gains a thin wasm-bindgen wrapper module (`mls.rs`) that owns the
collab-core value types and forwards calls. No new crypto, no new abstractions.

The existing AES `CollabCore` is left **untouched** — removing/replacing it is a
separate concern (issue #27 fail-closed key work / step 5). This PR does not touch
the relay wire (issue #51).

## Dependency wiring rationale (the crux)

`collab-core` depends on `openmls` with `default-features = false`. To build to
`wasm32-unknown-unknown`, openmls needs its `js` feature (its clock uses
`fluvio_wasm_timer` on wasm; without `js` the wasm build fails on a missing clock).
But we must NOT turn `openmls/js` on for **native** builds — the native relay/CLI
must keep the OS clock.

Cargo feature unification is global per build graph: if any crate reachable in the
native graph enables `openmls/js`, it turns on everywhere native. The fix:

1. `collab-core` exposes an opt-in feature:
   ```toml
   [features]
   wasm-clock = ["openmls/js"]
   ```
2. `collab-wasm` depends on collab-core **twice**:
   ```toml
   [dependencies]
   collab-core = { path = "../collab-core" }                       # base, no wasm-clock

   [target.'cfg(target_arch = "wasm32")'.dependencies]
   collab-core = { path = "../collab-core", features = ["wasm-clock"] }
   ```

Target-gated dependencies are excluded from the dependency graph for targets that
don't match the `cfg`. So on a native `cargo build --workspace`, the `wasm-clock`
edge simply does not exist, nothing enables `openmls/js`, and unification cannot
turn it on. On `wasm32`, the target-gated edge activates and `openmls/js` is present.
This is verified in STEP 3 with `cargo tree` on both targets.

`getrandom` (already `features = ["js"]`) and `aes-gcm` stay as-is for the AES path.

## The wasm-bindgen surface (`crates/collab-wasm/src/mls.rs`)

Thin owning wrappers; every error maps to a real JS `Error` via a `js_err` helper
(`JsError::new(&e.to_string())`) — unlike the AES path's `{type, message}` objects.

- `generate_key_package(user_id: &str) -> Result<WasmPendingMember, JsError>` — free fn.
- `WasmPendingMember(PendingMember)` with a `key_package` getter (-> `Vec<u8>`).
- `WasmInvite(Invite)` with getters `welcome`, `commit`, `doc_id`, `epoch`.
- `WasmEncryptedOp { ciphertext, epoch }` with getters.
- `WasmEncryptedDocument(EncryptedDocument)`:
  - `create(doc_id, user_id)` (static), `join(&WasmInvite, WasmPendingMember)` (static;
    takes the pending member **by value** so the moved Rust handle is consumed),
  - `create_invite(&mut, key_package)`, `process_commit(&mut, commit)`,
  - `insert(&mut, index, text)`, `delete(&mut, index, len)`, `get_content(&self)`,
  - `get_encrypted_update(&mut)`, `apply_encrypted_update(&mut, ciphertext, epoch)`,
  - `epoch` getter.

`lib.rs` gains `mod mls;` and re-exports the five symbols. The AES `CollabCore` is
not modified.

## Tests — RED-first, negative-path-heavy (real compiled WASM)

Tests run in the plugin's jest harness against the *real* committed `.wasm`
(`plugins/obsidian-ee/src/__tests__/helpers/load-real-wasm.ts`, extended to also
return the MLS symbols from the same generated module). `npm test`'s `pretest`
rebuilds the wasm, so RED (surface absent) precedes GREEN (surface present).

`plugins/obsidian-ee/src/__tests__/mls-wasm.test.ts`:

1. **Positive round-trip (in-process, no relay).** Alice creates `doc1`, invites Bob
   (via `bob.key_package`), Bob joins; Alice inserts "Hello", ships the encrypted op,
   Bob applies it. Assert Bob's content is "Hello", both epochs are 1, and the
   plaintext bytes of "Hello" are **not** a contiguous window of the ciphertext.
2. **NEGATIVE — cross-group ciphertext rejected.** An independent Carol in `doc2`
   (no shared MLS group) tries to apply Alice's op. Must throw; Carol's content stays
   "". (MLS authenticates the sender/group; a foreign ciphertext fails to decrypt.)
3. **NEGATIVE — wrong Welcome fails join.** Alice creates an invite sealed to Carol's
   init key; Bob tries to join with it. Must throw (the Welcome is not for Bob).
4. **NEGATIVE — consume-once.** After Bob's pending member is consumed by a successful
   `join`, accessing `bobPending.key_package` throws (wasm-bindgen nulls the moved
   handle → "null pointer passed to rust"). Proves `join` takes ownership.
5. **NEGATIVE — no key-injection path.** The MLS document exposes no
   `set_encryption_key` / `has_encryption_key`. There is no all-zeros/placeholder key
   entry point on the MLS surface (fail-closed by construction).

These encode the CLAUDE.md trust-boundary rule: every boundary gets a negative-path
test proving the attacker case is REJECTED, not just a positive round-trip.

## Native-build-safety argument (verification)

- STEP 3a: `cargo build --workspace` (native) is green, and
  `cargo tree -p collab-core -f "{p} {f}"` shows collab-core's openmls edge WITHOUT
  `js`.
- STEP 3b: `cargo tree --target wasm32-unknown-unknown -p openmls -f "{p} {f}"` shows
  `js` IS present for the wasm target.

## PR scope boundary

In scope: the wasm-bindgen MLS surface, the `wasm-clock` feature + target-gated dep,
the 5-test harness, the design doc.

Out of scope (explicitly untouched): the AES `CollabCore` (issue #27 / step 5), the
relay wire protocol (issue #51). No speculative public surface is added.
