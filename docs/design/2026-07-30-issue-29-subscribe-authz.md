# Issue #29 — Per-document subscribe authorization

Date: 2026-07-30
Branch: `feat/29-subscribe-authz`
Status: design (implement per this)

## Problem

Any identified client can `Subscribe` to any `doc_id` (`relay.rs:403` checks only
that the connection is identified). The payload stays MLS-encrypted, but a
non-member observes metadata: epochs, sender ids, message sizes, timing. #29 makes
subscription require proof of current group membership, verified by the
zero-knowledge relay with a **public key alone**.

## Chosen design (per decision): per-doc public-key anchor

The relay stores, per doc, a small NON-SECRET anchor `{ epoch, verifying_key }`.
It does NOT run MLS, hold ratchet state, or derive secrets — it stores a public
key + an epoch counter and does Ed25519 verification. `doc_id` stays the file path
(no blast radius on CLI/plugin/manifest).

### The capability key is derived from the MLS exporter secret

Every current group member can derive the same per-epoch secret via
`group.export_secret(crypto, LABEL, b"", 32)` (RFC 9420 §8.5). A non-member cannot.
The secret changes every epoch (rekey → new exporter secret). We turn that 32-byte
secret into an Ed25519 signing keypair (deterministic seed → `SigningKey`), so:
- All current members derive the SAME keypair → any member can mint capabilities.
- Non-members cannot derive it → cannot mint a verifiable capability.
- A removed member (post-#31 rekey) holds only the OLD epoch's key → their
  capability no longer matches the rotated anchor.

`LABEL = "obsidian-ee/subscribe-capability/v1"`.

The relay only ever sees the PUBLIC verifying key (the anchor) and signed
capabilities — never the exporter secret. This satisfies "relay holds no group
state" (it holds a public verification anchor, not a group secret; documented as
such in docs/security.md).

## CRATE PLACEMENT (load-bearing — do not get this wrong)

`collab-relay` depends on `collab-proto` ONLY, NOT `collab-core` (verified:
relay/Cargo.toml has just collab-proto). collab-core pulls in openmls. Therefore:

- **`SubscribeCapability` (the wire struct) + the PURE `verify_subscribe_capability`
  function live in `collab-proto`** (which the relay already depends on). Verification
  is Ed25519-only — NO MLS, NO openmls — so collab-proto stays MLS-free. Add
  `ed25519-dalek` to collab-proto's Cargo.toml + workspace deps.
- **The MINTING side lives in `collab-core`** (`mls.rs`), because it needs
  `group.export_secret(...)`. collab-core already... has no collab-proto dep — ADD
  `collab-proto` as a collab-core dependency so `mint_subscribe_capability` can
  return the shared `collab_proto::SubscribeCapability` type (one definition, no
  duplication). Verify this adds no cycle (proto does not depend on core — safe).

## collab-proto API (crates/collab-proto/src/lib.rs, or a new capability.rs module)

New dep: `ed25519-dalek` (already in Cargo.lock via the tree). No other new deps.

```rust
// A subscription capability: proves current-epoch membership of doc_id's group.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscribeCapability {
    pub doc_id: String,
    pub epoch: u64,
    pub expiry_unix: u64,   // seconds; relay rejects if now > expiry
    pub signature: Vec<u8>, // Ed25519 over the signed bytes below
}
// canonical signed bytes = LABEL_MSG || doc_id.len()(le) || doc_id || epoch(le) || expiry(le)
// (length-prefix doc_id so (doc_id, epoch) can't be ambiguously reparsed)

// Pure verification (no MLS) — the relay is NOT a group member. Ed25519 only.
// Returns Ok(()) iff signature verifies AND doc_id matches AND not expired AND
// epoch == expected_epoch.
pub fn verify_subscribe_capability(
    cap: &SubscribeCapability,
    verifying_key: &[u8; 32],
    expected_doc_id: &str,
    expected_epoch: u64,
    now_unix: u64,
) -> Result<(), CapabilityError>;
```

## collab-core API (crates/collab-core/src/mls.rs)

```rust
impl MlsDocumentGroup {
    /// Ed25519 verifying key for THIS epoch, to register as the relay anchor.
    pub fn subscribe_verifying_key(&self) -> Result<[u8; 32]>;
    /// Mint a capability for this doc at the current epoch, valid until now+ttl.
    pub fn mint_subscribe_capability(&self, doc_id: &str, now_unix: u64, ttl_secs: u64)
        -> Result<collab_proto::SubscribeCapability>;
    // internal: derive_subscribe_keypair(&self) -> ed25519_dalek::SigningKey
    //   seed = group.export_secret(crypto, LABEL, b"", 32) -> [u8;32] -> SigningKey::from_bytes
}
```

`now_unix` is injected (not read from a global clock inside collab-core/proto) so
tests are deterministic and wasm stays clock-agnostic.

## Proto message changes (crates/collab-proto/src/lib.rs)

```rust
ClientMessage::Subscribe { doc_id, capability: Option<SubscribeCapability> }
// New: a member registers/rotates the doc's verify anchor.
ClientMessage::RegisterDocKey { doc_id, epoch, public_key: Vec<u8>, proof: Vec<u8> }
// proof = Ed25519 self-signature over (doc_id||epoch||public_key) with the
// epoch's key — proves the registrant holds the epoch's secret (is a member).
ErrorCode::Unauthorized reused for a rejected Subscribe/RegisterDocKey.
```
`Subscribe.capability` is `Option` for a clean migration, BUT the relay REJECTS a
`None` (or absent) capability once authz is on — fail closed. (Keeping it Option
avoids a hard proto break and lets the negative test send `None`.)

## Relay changes (crates/collab-relay/src/relay.rs + routing.rs)

- `MessageRouter` gains `anchors: Arc<RwLock<HashMap<DocumentId, DocAnchor>>>` where
  `DocAnchor { epoch: u64, verifying_key: [u8;32] }`. Public data, not a secret.
- `handle_register_doc_key(uid, doc_id, epoch, public_key, proof)`:
  - Verify `proof` is a valid self-signature of `(doc_id||epoch)` under
    `public_key`. This proves only **possession of the keypair being registered**
    — NOT group membership. The zero-knowledge relay has no group state and no
    identity system, so it *cannot* verify membership. Anchor trust is **TOFU**.
  - Accept iff no anchor yet (TOFU — like first-Identify-wins for user_id), OR
    `epoch > current.epoch` (strictly monotonic forward rotation). Store/replace.
  - A first (TOFU) registration is additionally bounded: rejected if the anchor
    map is at `max_documents` (resource bound — `handle_register_doc_key` runs
    regardless of the authz toggle, so an unbounded map would OOM) or if
    `epoch > MAX_INITIAL_ANCHOR_EPOCH` (blunts a `u64::MAX` first-anchor
    pre-emption lockout). Monotonic rotation of an existing anchor is unaffected.
  - Reject (Unauthorized) a stale/equal-or-lower epoch or a bad proof.
  - **Where #31 (removal) actually bites: the SUBSCRIBE path, not here.** After a
    rekey a removed member's stale-epoch *capability* no longer matches the
    rotated anchor, so it stops verifying at subscribe time. Requiring the epoch's
    own key for a rotation stops a stale-epoch key from forging a *higher*-epoch
    anchor, but the relay cannot stop a non-member from registering a *first*
    anchor for an unclaimed doc (TOFU). Do NOT claim the register-path proof
    proves membership.
- `handle_subscribe(uid, tx, doc_id, capability)`:
  - existing identified + validate_doc_id checks, THEN:
  - `anchor = anchors.get(doc_id)`; if `None` → reject Unauthorized (no proof
    possible → fail closed).
  - `capability` `None` → reject Unauthorized.
  - `verify_subscribe_capability(cap, anchor.verifying_key, uid, doc_id, anchor.epoch, now)`
    → on Err reject Unauthorized; on Ok proceed to `router.subscribe`.
  - Bind the LOCAL `uid` (the CONNECTION's identified user id), the LOCAL `doc_id`
    (the subscribe target) and the LOCALLY-stored `anchor.epoch`/`anchor.verifying_key`
    — NEVER trust `cap.user_id`/`cap.epoch`/a frame field as the source of truth
    (a capability minted for Alice presented by Eve's connection fails
    `UserIdMismatch`; `cap.epoch` must EQUAL `anchor.epoch` or verification fails).
- `now_unix`: the relay reads the OS clock (`SystemTime`) at verify time.

## CLI wiring (crates/collab-cli)

- Owner (group create) and any member: after establishing the group, send
  `RegisterDocKey { doc_id, epoch, public_key = subscribe_verifying_key(), proof }`.
  Re-register on epoch change (add member). (Whichever member is online can rotate;
  simplest: the actor who performed the add/remove re-registers.)
- Before `Subscribe`, mint a capability (`mint_subscribe_capability(doc_id, now,
  TTL=300s)`) and attach it.

## BDD scenarios → RED-first tests

1. **collab-core unit (mint/verify round-trip):** GIVEN a 2-member group, WHEN a
   member mints a capability for doc_id at epoch E, THEN
   `verify_subscribe_capability` with that member's verifying key, doc_id, E, and a
   now < expiry returns Ok.
2. **collab-core NEGATIVE (non-member key):** GIVEN member Alice's capability and a
   DIFFERENT group's (Eve's) verifying key, THEN verify returns Err. AND a
   capability verified with the right key but wrong `expected_doc_id` returns Err.
   AND expired (now > expiry) returns Err. AND wrong `expected_epoch` returns Err.
   (Mutation-check: flip one byte of the signature → Err.)
3. **relay unit NEGATIVE (the core of #29):** GIVEN a registered anchor for docA,
   WHEN a client Subscribes with (a) no capability, (b) a capability signed by a
   key that is NOT the anchor key (a non-member), (c) a capability for a different
   doc → THEN each is rejected with Unauthorized and the client is NOT added to the
   subscription set. AND with a valid member capability → subscribed.
4. **relay unit (anchor rotation):** a monotonic higher-epoch self-signed
   RegisterDocKey replaces the anchor; a lower/equal epoch or bad proof is rejected.
5. **wire test (over the relay binary, e2e-tests):** two real members register +
   subscribe successfully; a third identified-but-non-member client's Subscribe to
   the doc is REJECTED (no valid capability). This is the metric-relevant test.

## Trust boundary / documented residuals (docs/security.md)

- **Anchor bootstrap is TOFU:** the first `RegisterDocKey` for a doc_id wins, like
  the relay's existing first-Identify-wins for `user_id`. An attacker who races the
  real owner to register a doc_id's anchor could grant subscribe (metadata) access
  — but STILL cannot decrypt (MLS membership gates that). This is a metadata
  escalation requiring a race, inherent to a zero-knowledge relay that has no other
  crypto anchor; documented, hardening deferred. (Option B — doc_id commits to the
  key — was rejected for blast radius.)
- **Live subscriptions are checked at subscribe time only.** A removed member with
  an already-open subscription keeps receiving ciphertext (can't decrypt) until they
  disconnect; dropping live subscriptions on anchor rotation is #31 scope.
- **Capability TTL (300s)** bounds replay of a captured capability within an epoch;
  wss:// protects it in transit.

## PONYTAIL-DEBT.md

Issue #29 asks to clear the deferral entry there. The file is NOT in the tree
(it lived on the deleted tech-debt branch). Note in the PR that the deferral is now
resolved in code; nothing to delete.

## Ponytail

One new dep (`ed25519-dalek`, the standard Rust Ed25519 — not hand-rolled crypto).
Exporter-secret-derived key + signed capability is exactly what the issue specifies,
not speculative. Relay anchor is a HashMap of public keys — no MLS in the relay.
Keep `verify_subscribe_capability` a pure function so the relay never links MLS.
