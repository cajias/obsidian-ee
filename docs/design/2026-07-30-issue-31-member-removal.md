# Issue #31 — MLS member removal + key revocation

Date: 2026-07-30
Branch: `feat/31-member-removal` (off `feat/29-subscribe-authz`, so the
removed-member-capability-also-dies test can exist)
Status: design (implement per this)

## Problem

No way to remove a member. A departed member keeps decrypting everything that
follows, so forward secrecy is incomplete. #31 implements the MLS `Remove`
proposal+commit so removal advances the epoch and rekeys the group, cutting the
removed member off from subsequent messages — AND (coordinating with #29) their
subscribe capability stops working.

## Who-may-remove-whom policy (PICKED — documented in docs/security.md)

**The group creator (owner) may remove any member; a non-owner may remove no one
except themselves (leave).** Rationale:
- The codebase uses `BasicCredential` (a `user_id` byte string) with no role
  system and no stored ACL. A full any-member-can-remove-any-member model invites
  removal races and griefing (any member evicts any other); a designated-owner
  model is the simplest defensible policy for a small doc-sharing group and needs
  no new credential machinery.
- MLS itself does NOT enforce authorization — any member can craft a Remove commit
  and the protocol will accept it. So the policy is enforced at TWO layers:
  1. **Mint side:** only the owner's client exposes `remove_member`; other clients
     don't offer it (a non-owner leaves via self-removal, future work — not in
     this PR's scope; documented).
  2. **Receive side (the enforceable half):** when a member processes a commit that
     contains Remove proposals, it checks the committer is the group owner; a
     Remove commit from a non-owner is REJECTED (not merged). This is the durable
     guard — it doesn't rely on a well-behaved client.
- "Owner" = the `user_id` that created the group. Every member already learns the
  full member list on join; the owner is identifiable as the creator. For this
  first cut we identify the owner as a value the group carries (the creator's
  user_id), set at `create`/`join` time. (openmls exposes committer + removed
  members via the StagedCommit's remove_proposals + the sender's leaf credential.)

Residual (documented): owner succession / owner-removes-owner / a member leaving
voluntarily are future work. This PR implements owner-removes-member, which is what
the acceptance criteria require.

## collab-core API (crates/collab-core/src/mls.rs)

`MlsDocumentGroup` gains an `owner_id: String` field (the creator's user_id),
threaded through `create` (owner = user_id) and `join` (owner = the creator, which
the joiner learns — simplest: carry it in the Invite/Welcome path; if that's
awkward, derive it from the member list's creator or pass it explicitly — pick the
minimal wiring and document it).

```rust
impl MlsDocumentGroup {
    /// Owner-only: remove the member whose credential identity == `member_user_id`.
    /// Advances the epoch and rekeys. Returns the serialized commit that existing
    /// members must process. Errors if self is not the owner, or the target isn't
    /// a current member, or the target IS the owner.
    pub fn remove_member(&mut self, member_user_id: &str) -> Result<Vec<u8>>;
    // impl: guard self.user_id == self.owner_id (I am the owner);
    //   find the LeafNodeIndex whose Member.credential decodes to member_user_id
    //   via self.group.members(); reject if not found or == owner;
    //   self.group.remove_members(&crypto, &signature_keys, &[idx]) -> (commit, _welcome, _gi);
    //   merge_pending_commit; return commit_bytes.

    /// True iff this member created the group.
    pub fn is_owner(&self) -> bool; // self.user_id == self.owner_id
}
```

`process_commit` MUST enforce the receive-side policy: after
`process_message` yields a `StagedCommitMessage`, if the staged commit contains any
Remove proposal, verify the commit's SENDER (committer) credential identity ==
`self.owner_id`. If not, RETURN Err (do NOT merge) — a non-owner's removal is
rejected. (openmls: `staged_commit.remove_proposals()` for the removed leaves;
the committer is the processed message's sender — capture it before `into_content`,
or use the staged commit's update-path/sender. Read the openmls 0.7.4 API and use
whatever exposes the committer's credential; if the committer identity isn't
cleanly available from the StagedCommit, capture `processed.credential()` /
sender before consuming `processed`.)

The removed member's own `process_commit` of the removal commit: openmls returns a
commit that evicts self; after merge the group becomes inactive for them (they
cannot decrypt subsequent messages — `create_message`/`process_message` error with
group-not-active). That's the crypto guarantee. Ensure our wrapper surfaces this
as an error rather than a panic.

## wasm surface (crates/collab-wasm/src/mls.rs)

Add `WasmEncryptedDocument::remove_member(&mut self, member_user_id: &str) ->
Result<Vec<u8>, JsError>` and `is_owner(&self) -> bool`, delegating to
EncryptedDocument/MlsDocumentGroup. (EncryptedDocument in encryption.rs needs a
matching `remove_member` pass-through.) Keep the existing `process_commit` — it now
enforces the owner policy under the hood.

## #29 coordination (the cross-cutting requirement)

After a removal commit, the epoch advances and the exporter secret rotates →
`subscribe_verifying_key()` CHANGES. So:
- A removed member holds only the OLD epoch's subscribe key; any capability they
  mint is at the old epoch and fails `verify_subscribe_capability(expected_epoch =
  new_anchor_epoch)` once the owner re-registers the anchor at the new epoch.
- The remaining members re-register the doc anchor (`RegisterDocKey`) at the new
  epoch after removal (owner does it, same as after add_member).
This is already true by construction from #29's epoch-derived key — #31's test
just PROVES it end to end.

## BDD scenarios → RED-first tests (collab-core)

Build on the existing `two_member_group()` / three-member test helpers.
1. **Positive removal (epoch + rekey):** GIVEN a 3-member group (owner Alice, Bob,
   Carol) at epoch 2, WHEN Alice `remove_member("carol")` and Bob processes the
   commit, THEN Alice's & Bob's epoch advances (3), and Alice→Bob app messages
   still round-trip.
2. **NEGATIVE — removed member cannot decrypt post-removal (THE acceptance test):**
   after Carol processes the removal commit (evicting herself), an app message
   Alice sends at the new epoch CANNOT be decrypted by Carol — `carol.decrypt(msg)`
   returns Err (group inactive / wrong epoch). Assert Carol's content does not
   update. Mutation-check: if removal were a no-op (skip the commit), Carol WOULD
   decrypt → the test must go RED under that mutation.
3. **NEGATIVE — non-owner removal rejected (policy):** Bob (non-owner) crafts a
   Remove commit for Carol; when Alice/Carol `process_commit` it, it is REJECTED
   (Err, not merged) — epoch does NOT advance, Carol stays a member. (If a
   non-owner client can't even mint the commit because `remove_member` guards
   `is_owner`, ALSO assert `bob.remove_member("carol")` returns Err. Both the mint
   guard and the receive guard are tested.)
4. **NEGATIVE — #29 capability dies after removal (cross-cut):** GIVEN the anchor
   registered at the pre-removal epoch, WHEN Alice removes Carol and re-derives the
   new-epoch verifying key, THEN a capability Carol could mint (old epoch key)
   fails `verify_subscribe_capability` against the new epoch/key; a capability a
   remaining member mints at the new epoch verifies. (Pure collab-core +
   collab-proto test, no relay needed.)

## wire test (e2e-tests, #[ignore] Docker) — optional but strong

Extend the subscribe-authz wire test OR add one: over the real relay, owner removes
a member; the removed member, after the owner re-registers the anchor at the new
epoch, is REJECTED on a fresh Subscribe (its old-epoch capability no longer
verifies). Only add if it fits cleanly on top of the #29 wire harness; the
collab-core scenario 4 already proves the crypto. Count via `cargo xtask e2e`.

## docs/security.md

- "No revocation: Member removal and key revocation are not yet implemented." →
  REPLACE with: member removal is implemented via MLS Remove (epoch advance +
  rekey); document the who-may-remove-whom policy (owner-removes-member; receive-side
  owner check; self-leave + owner-succession are future work). Note that a removed
  member's #29 subscribe capability also stops working after the post-removal anchor
  rotation.
- MLS group lifecycle section: add the Remove flow alongside add.

## Ponytail

No new deps. `remove_member` mirrors `add_member`'s existing shape. The policy is a
one-value owner check + a receive-side guard — no role/ACL system. Don't build
self-leave or owner-succession (YAGNI; documented as future work). The #29-capability
death is free from the epoch-derived key — just test it, don't add mechanism.
