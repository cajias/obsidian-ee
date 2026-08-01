# Issue #28 FINAL — Remove the AES-256-GCM PSK path; MLS is the sole crypto

Date: 2026-07-30
Branch: `feat/28-remove-aes-mls-only`
Status: design

## Goal

Remove the AES-256-GCM pre-shared-key (PSK) path entirely so MLS (RFC 9420, via
`collab-core`) is the ONLY crypto reachable from the WASM/plugin client. The
native Rust CLI/relay/collab-core MLS path is already correct and out of scope.

Issue #28 acceptance (the two still-open boxes):
- The AES-256-GCM pre-shared-key path is removed, not left as a fallback.
- `docs/security.md` and `docs/crate-guide.md` updated to match reality.

## Ground truth (verified against source)

- The relay wire is crypto-agnostic and MLS-complete: `ClientMessage::MlsHandshake`
  / `ServerMessage::MlsHandshake` with `MlsMessageType::{KeyPackage,Welcome,Commit,
  Application}` (`crates/collab-proto/src/lib.rs:62-112`), routed opaquely by
  `handle_mls_handshake` (`crates/collab-relay/src/relay.rs:469-493`). No relay
  changes needed for #28.
- The WASM MLS surface is complete and exported (`crates/collab-wasm/src/mls.rs`):
  `generate_key_package`, `WasmPendingMember`, `WasmInvite` (+`from_welcome`),
  `WasmEncryptedOp`, `WasmEncryptedDocument` (create/join/create_invite/
  process_commit/insert/delete/get_content/get_encrypted_update/
  apply_encrypted_update/epoch).
- The MLS-over-real-relay flow is already proven from TypeScript:
  `plugins/obsidian-ee/src/__tests__/two-user-mls-integration.test.ts` drives
  identify → subscribe → `mls_handshake` (KeyPackage→Welcome) → join →
  MLS-encrypted `yrs_update` round-trip, plus a cross-group rejection negative
  test, against `cargo run -p collab-relay`. This is the choreography the plugin
  runtime will adopt.
- The gap: the plugin RUNTIME (`main.ts`, `collab-client.ts`) uses ONLY
  `CollabCore` (AES). Zero MLS symbols in non-test plugin `src/`. So removing AES
  forces the plugin rewire to `WasmEncryptedDocument`; the two land together.

## Decision 1 — delete the whole AES `CollabCore`

`crates/collab-wasm/src/lib.rs` is entirely the AES path plus a plaintext Yrs
CRDT wrapper. After AES removal, the plaintext `CollabCore` has NO legitimate
caller: `WasmEncryptedDocument` (Yrs + MLS) already provides insert/delete/
get_content and encrypted sync. Per ponytail/YAGNI + CLAUDE.md's dead-code rule,
delete the entire struct rather than keep a plaintext-CRDT shell "for later."

**Delete from `crates/collab-wasm/src/lib.rs`:**
- AES imports (`aes_gcm::*`, `getrandom::getrandom`) — lines ~6-10.
- The entire `CollabCore` struct + `encryption_key` field — ~86-91.
- The internal `impl CollabCore` block (set_encryption_key_internal, encrypt_internal,
  decrypt_internal, apply_update_internal, encode_state*, apply_update_encrypted*) — ~94-193.
- The `#[wasm_bindgen] impl CollabCore` block (new, get_text, insert, delete,
  encode_state*, apply_update*, set/has_encryption_key, encrypt, decrypt) — ~195-301.
- `impl Default for CollabCore` — ~303-307.
- The AES `#[cfg(test)] mod tests` — ~309-497. (All CollabCore tests die with it;
  the MLS surface is covered by the plugin jest suite against real WASM.)
- `CollabError`/`CollabErrorType` and the `From<CollabError> for JsValue` impl
  (~16-84) exist ONLY for the AES surface (mls.rs uses its own `js_err` → `JsError`).
  Delete them too, unless the compiler shows a live use.

**Result:** `lib.rs` becomes essentially `mod mls; pub use mls::{...};` plus
whatever (if anything) is still referenced. Let the compiler prove what stays.

## Decision 2 — Cargo.toml deps (empirical, not guessed)

`crates/collab-wasm/Cargo.toml`:
- **Remove** `aes-gcm = "0.10"` (line 18) and its MVP comment (17). Not needed
  by MLS.
- `getrandom`, `js-sys`, `web-sys` (Crypto/SubtleCrypto): the direct `use
  getrandom::getrandom` (AES nonce) is deleted, and `js-sys` was used only by the
  deleted `From<CollabError>` impl. BUT the openmls crypto provider needs a wasm
  RNG. **Method:** after deleting AES, run `cargo build -p collab-wasm --target
  wasm32-unknown-unknown` (and native). Remove ONLY the dep entries the build
  proves unused; keep whatever the wasm RNG/entropy path still requires. Do not
  guess — the build is the oracle. `collab-core` deps (base + wasm-clock target
  edge) are unchanged.

## Decision 3 — plugin rewire to MLS (the substantive change)

`collab-client.ts` becomes MLS-backed, adopting the proven #51 choreography.
`CollabCore` import → removed; drive `WasmEncryptedDocument`.

**Config (`CollabClientConfig`):** drop `encryptionKey: Uint8Array`. Add an
explicit role so there is no race-prone auto-negotiation (YAGNI on auto-detect):
`role: 'owner' | 'joiner'` (owner creates the group; joiner publishes a key
package and waits for the Welcome). `userId`/`docId`/`relayUrl` unchanged.

**Client state:** owns `doc: WasmEncryptedDocument | null` and, for a joiner,
`pending: WasmPendingMember | null`. On connect (after `identified` +
`subscribed`):
- **owner:** `doc = WasmEncryptedDocument.create(docId, userId)`. On an inbound
  `mls_handshake` `key_package` frame, call `doc.create_invite(kp)` and send the
  `welcome` back as an `mls_handshake` `welcome` frame. (Commit fan-out to
  existing members via `process_commit` is wired for N>2 but the first cut is
  correct for the 2-party case the tests cover.)
- **joiner:** `pending = generate_key_package(userId)`; send its `key_package`
  as an `mls_handshake` frame. On the inbound `welcome` frame, `doc =
  WasmEncryptedDocument.join(WasmInvite.from_welcome(docId, welcome), pending)`.
  Bind the LOCAL `docId` (never the frame's) into `from_welcome` — matches the
  CLAUDE.md "bind local context" invariant and the mls.rs doc comment.

**handleMessage:** add an `mls_handshake` case dispatching by `message_type`.
Keep `yrs_update` but route through MLS: send via `doc.get_encrypted_update()`
(→ `{ciphertext, epoch}`), apply via `doc.apply_encrypted_update(ciphertext,
epoch)`. The AES `apply_update_encrypted`/`encode_state_encrypted(docId)` calls
are replaced. Note: MLS binds each message to the group via its internal
GroupContext, so no docId-AAD is passed here (the AAD binding was AES-path
specific; the frame-level `doc_id !== config.docId` early-reject stays as
defense-in-depth).

**`sendUpdate` / editor-sync:** unchanged shape (`applyTextDiff` → insert/delete
on `doc`), but a NO-OP fail-closed guard: if `doc` is null (group not yet
established) `sendUpdate` returns false and emits no frame — never encrypts to
nobody, never leaks plaintext.

**`main.ts`:** remove `encryptionKey` setting + `decodeBase64Key`/`encodeBase64Key`
+ the all-zeros/32-byte `startSession` guard + the "Generate random key" UI. Add
the role choice (owner/join) to settings or the start command. `initWasm` /
`stopSession` free the `WasmEncryptedDocument` instead of `CollabCore`.

## Decision 4 — the new fail-closed invariant

Old invariant (PSK): reject empty/short/all-zeros key in `validateConfig` /
`startSession`. Under MLS-only there is NO key input, so "all-zeros key rejected"
is moot BY CONSTRUCTION — you cannot hand the client a placeholder secret.

New invariant, satisfying CLAUDE.md's "never ship a real encryption path that
accepts a placeholder/all-zeros/hardcoded key; keep the fail-closed guards and
cover them with a test":
- **No plaintext path exists.** There is no unencrypted send/apply on the client.
- **No emit before an established group.** `sendUpdate`/`get_encrypted_update`
  before `doc` exists is refused (returns false / throws), so a session never
  produces or consumes ciphertext outside an MLS group.
- **Decryption is gated by MLS membership** (proven by the existing cross-group
  negative test): a non-member cannot decrypt, regardless of relay access.

Lives in: `collab-client.ts` (the null-`doc` guard) and `main.ts` (startSession
refuses to bind the editor until `connect()` resolves with an established group).

## Decision 5 — RED-first tests (assert AES GONE / MLS-only)

RED before the deletion, GREEN after:
1. **Rust — AES symbols absent.** A `collab-wasm` compile-level assertion that
   `CollabCore` and `set_encryption_key`/`encrypt`/`decrypt` no longer exist.
   Simplest durable form: a test in `crates/collab-wasm/src/mls.rs` (or a doc
   grep in CI-free form) — but the strongest RED-first signal is that the AES unit
   tests are DELETED and the crate still builds with only the MLS surface. Add one
   positive `#[test]`-free wasm-bindgen-independent check is not possible for
   `#[wasm_bindgen]` types; rely on: (a) build succeeds, (b) grep guard test below.
2. **TS — no AES API on the WASM module.** In a plugin jest test
   (`mls-wasm.test.ts` already has "no key-injection path" — extend it): assert
   the real loaded WASM module exposes NO `CollabCore`, no `set_encryption_key`,
   no `has_encryption_key`. RED today (CollabCore exists), GREEN after removal.
3. **TS — no `encryptionKey` config / no PSK.** A test asserting
   `CollabClientConfig` has no `encryptionKey` and that constructing a client
   never calls `set_encryption_key`. Assert the MLS handshake path is used
   (owner creates a group; joiner joins; round-trip works) — reuse the
   two-user-mls-integration choreography but through `CollabClient`.
4. **TS — fail-closed.** A test that `sendUpdate` before the group is established
   emits no frame / returns false (no plaintext, no encrypt-to-nobody).
5. **Grep guard (cheapest durable regression guard):** a test (or the existing
   lint step) asserting `grep -r 'aes' crates/collab-wasm/src` and
   `grep -ri 'encryptionKey\|set_encryption_key' plugins/obsidian-ee/src` (minus
   .d.ts) return nothing. Add as a small jest/unit assertion so the class can't
   regress silently.

## Decision 6 — docs

`docs/security.md`:
- Remove the AES-256-GCM collab-wasm (MVP) section (~20-31) and the WASM
  AES data-flow diagram (~138-145). The WASM client now uses MLS (same section as
  collab-core).
- Threat-model table (~40-56): "Message tampering | AEAD (AES-GCM)" → MLS AEAD
  (the ciphersuite already uses AES-128-GCM *inside* MLS; the point is there is no
  standalone PSK AES path).
- Security-layers row (~154): "MLS / AES-GCM" → "MLS".
- Known limitations (~167-170): delete "WASM uses static shared key" and
  "Placeholder encryption key" (both resolved by #28). Keep the persistence
  (#30) and BasicCredential notes. Revocation (#31) stays.
- Roadmap (~175-180): check off "Implement MLS in WASM".

`docs/crate-guide.md`:
- Rewrite the collab-wasm section (~295-348): it currently documents ONLY the AES
  surface and omits the MLS types. New text documents `WasmEncryptedDocument` &
  co. as the sole crypto surface; remove `set_encryption_key` API row and the
  "uses AES-256-GCM (not MLS) as an MVP" line; remove `aes-gcm` from the dep list.

## Decision 7 — plugin tests that break (rewrite or delete)

- `encryption-integration.test.ts`, `wasm-integration.test.ts` (AES CollabCore
  encrypt/decrypt): delete — the behavior no longer exists.
- `two-user-integration.test.ts` (AES mock-relay PSK): delete or rewrite to MLS.
  The MLS analog already exists (`two-user-mls-integration.test.ts`); prefer
  DELETE over duplicate.
- `collab-client.test.ts`, `main.test.ts`: rewrite the crypto-related cases to
  the MLS client (drop `encryptionKey`, drop `set_encryption_key`); keep the
  transport/reconnect/lifecycle cases (they are crypto-agnostic and valuable).
- `two-user-sync.spec.ts` (Playwright, AES PSK end-to-end): rewrite to the MLS
  owner/joiner flow, or mark clearly and rewrite. `e2e/mock-relay.ts` may need
  MLS-frame passthrough (it already forwards opaque frames).
- `mls-wasm.test.ts`: keep + extend (add the "no AES symbols" assertion).

## Residuals (explicitly out of scope for #28, and correctly sequenced)

- Robust async join when the owner is offline; invite-link/key-package
  distribution UX. First cut assumes both online (the #51 choreography).
- Persisting the MLS group across restart → **#30**.
- Member removal / revocation → **#31**.
- Per-document subscribe capability → **#29**.

These are the remaining milestone issues; #28 deliberately stops at "AES removed,
MLS is the only crypto, plugin has no PSK path."
