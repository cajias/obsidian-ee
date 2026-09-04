//! wasm-bindgen surface exposing collab-core's MLS engine to JS (issue #28).
//!
//! Thin owning wrappers over `collab_core` value types; no crypto lives here.
//! Errors surface as real JS `Error`s via [`js_err`], unlike the removed AES
//! path which returned `{type, message}` objects.

use collab_core::{
    AnchorRotation, EncryptedDocument, EncryptedOp, Invite, MlsDocumentGroup, PendingMember,
};
use wasm_bindgen::prelude::*;

fn js_err(e: collab_core::Error) -> JsError {
    JsError::new(&e.to_string())
}

/// A member awaiting a Welcome. Owns the collab-core `PendingMember`; consumed
/// by value on [`WasmEncryptedDocument::join`].
#[wasm_bindgen]
pub struct WasmPendingMember(PendingMember);

#[wasm_bindgen]
impl WasmPendingMember {
    #[wasm_bindgen(getter)]
    pub fn key_package(&self) -> Vec<u8> {
        self.0.key_package().to_vec()
    }
}

/// Generate a key package for `user_id` so an owner can invite this member.
#[wasm_bindgen]
pub fn generate_key_package(user_id: &str) -> Result<WasmPendingMember, JsError> {
    MlsDocumentGroup::generate_key_package(user_id).map(WasmPendingMember).map_err(js_err)
}

/// The `RegisterDocKey` payload that moves the relay's subscribe anchor to the
/// epoch a commit just created (issue #29). Send all four fields verbatim.
#[wasm_bindgen]
pub struct WasmAnchorRotation(AnchorRotation);

#[wasm_bindgen]
impl WasmAnchorRotation {
    #[wasm_bindgen(getter)]
    pub fn epoch(&self) -> u64 {
        self.0.epoch
    }
    #[wasm_bindgen(getter)]
    pub fn public_key(&self) -> Vec<u8> {
        self.0.public_key.to_vec()
    }
    #[wasm_bindgen(getter)]
    pub fn proof(&self) -> Vec<u8> {
        self.0.proof.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn rotation_proof(&self) -> Vec<u8> {
        self.0.rotation_proof.clone()
    }
}

/// A member removal: the commit existing members must process, plus the anchor
/// rotation the removal's rekey requires.
#[wasm_bindgen]
pub struct WasmRemoval {
    commit: Vec<u8>,
    rotation: AnchorRotation,
}

#[wasm_bindgen]
impl WasmRemoval {
    #[wasm_bindgen(getter)]
    pub fn commit(&self) -> Vec<u8> {
        self.commit.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn rotation(&self) -> WasmAnchorRotation {
        WasmAnchorRotation(self.rotation.clone())
    }
}

/// An invite: the Welcome for the new member plus the commit for existing members.
#[wasm_bindgen]
pub struct WasmInvite(Invite);

#[wasm_bindgen]
impl WasmInvite {
    /// Reconstruct an invite on the JOINER's side from the two things a joiner
    /// actually has: its LOCALLY-TRUSTED `doc_id` and the `welcome` bytes read
    /// off the wire. `create_invite` builds the full `WasmInvite` on the owner's
    /// side, but a joiner only receives raw Welcome bytes over the relay and has
    /// no way to obtain the owner's `WasmInvite` object. `join` reads only
    /// `doc_id` + `welcome` (commit/epoch are for existing members), so those are
    /// left empty here. `doc_id` MUST be the joiner's own trusted value, NEVER a
    /// field taken from the inbound frame.
    pub fn from_welcome(doc_id: &str, welcome: &[u8]) -> WasmInvite {
        WasmInvite(Invite {
            doc_id: doc_id.to_string(),
            welcome: welcome.to_vec(),
            commit: Vec::new(),
            epoch: 0,
            rotation: None,
        })
    }

    #[wasm_bindgen(getter)]
    pub fn welcome(&self) -> Vec<u8> {
        self.0.welcome.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn commit(&self) -> Vec<u8> {
        self.0.commit.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn doc_id(&self) -> String {
        self.0.doc_id.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn epoch(&self) -> u64 {
        self.0.epoch
    }
    /// The anchor rotation this invite's commit created — `undefined` on a
    /// [`WasmInvite::from_welcome`] reconstruction, which has no outgoing key.
    #[wasm_bindgen(getter)]
    pub fn rotation(&self) -> Option<WasmAnchorRotation> {
        self.0.rotation.clone().map(WasmAnchorRotation)
    }
}

/// An encrypted CRDT update to ship over the relay.
#[wasm_bindgen]
pub struct WasmEncryptedOp {
    ciphertext: Vec<u8>,
    epoch: u64,
}

#[wasm_bindgen]
impl WasmEncryptedOp {
    #[wasm_bindgen(getter)]
    pub fn ciphertext(&self) -> Vec<u8> {
        self.ciphertext.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
}

/// An end-to-end-encrypted collaborative document (Yrs CRDT + MLS).
#[wasm_bindgen]
pub struct WasmEncryptedDocument(EncryptedDocument);

#[wasm_bindgen]
impl WasmEncryptedDocument {
    pub fn create(doc_id: &str, user_id: &str) -> Result<WasmEncryptedDocument, JsError> {
        EncryptedDocument::create(doc_id, user_id).map(Self).map_err(js_err)
    }

    /// Join via a Welcome. Takes `pending` by value: the moved handle is consumed,
    /// so a second use from JS traps (null pointer) — key packages are single-use.
    pub fn join(
        invite: &WasmInvite,
        pending: WasmPendingMember,
    ) -> Result<WasmEncryptedDocument, JsError> {
        EncryptedDocument::join(&invite.0, pending.0).map(Self).map_err(js_err)
    }

    pub fn create_invite(&mut self, key_package: &[u8]) -> Result<WasmInvite, JsError> {
        self.0.create_invite(key_package).map(WasmInvite).map_err(js_err)
    }

    /// Process a peer's commit, returning the anchor rotation for the epoch it
    /// creates: the group has rekeyed, so the relay's anchor must move with it.
    pub fn process_commit(&mut self, commit: &[u8]) -> Result<WasmAnchorRotation, JsError> {
        self.0.process_commit(commit).map(WasmAnchorRotation).map_err(js_err)
    }

    /// Owner-only: remove `member_user_id`, returning the commit existing members
    /// must `process_commit` plus the resulting anchor rotation (issues #31, #29).
    /// Rejected if this client is not the owner.
    pub fn remove_member(&mut self, member_user_id: &str) -> Result<WasmRemoval, JsError> {
        self.0
            .remove_member(member_user_id)
            .map(|(commit, rotation)| WasmRemoval { commit, rotation })
            .map_err(js_err)
    }

    /// True iff this client created the document's group (is the owner).
    pub fn is_owner(&self) -> bool {
        self.0.is_owner()
    }

    pub fn insert(&mut self, index: u32, text: &str) {
        self.0.insert(index, text);
    }

    pub fn delete(&mut self, index: u32, len: u32) {
        self.0.delete(index, len);
    }

    pub fn get_content(&self) -> String {
        self.0.get_content()
    }

    pub fn get_encrypted_update(&mut self) -> Result<WasmEncryptedOp, JsError> {
        let op = self.0.get_encrypted_update().map_err(js_err)?;
        Ok(WasmEncryptedOp { ciphertext: op.ciphertext, epoch: op.epoch })
    }

    pub fn apply_encrypted_update(&mut self, ciphertext: &[u8], epoch: u64) -> Result<(), JsError> {
        let op = EncryptedOp { ciphertext: ciphertext.to_vec(), epoch };
        self.0.apply_encrypted_update(&op).map_err(js_err)
    }

    /// Encrypt caller-supplied bytes (the vault manifest CRDT) under this doc's
    /// MLS group WITHOUT touching the internal Yrs text. The manifest rides its
    /// own group on `manifest_doc_id`; cross-group ciphertext fails
    /// authentication in `decrypt_bytes`, giving doc-scoping/replay isolation.
    pub fn encrypt_bytes(&mut self, plaintext: &[u8]) -> Result<WasmEncryptedOp, JsError> {
        let op = self.0.encrypt_bytes(plaintext).map_err(js_err)?;
        Ok(WasmEncryptedOp { ciphertext: op.ciphertext, epoch: op.epoch })
    }

    /// Decrypt bytes produced by a peer's `encrypt_bytes`, returning the
    /// plaintext manifest update. Fails closed on any authentication error,
    /// including ciphertext bound to a different MLS group.
    pub fn decrypt_bytes(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, JsError> {
        self.0.decrypt_bytes(ciphertext).map_err(js_err)
    }

    #[wasm_bindgen(getter)]
    pub fn epoch(&self) -> u64 {
        self.0.epoch()
    }

    /// Mint a subscribe capability for this document, as the JSON the relay
    /// expects in `Subscribe.capability` (issues #29, #72).
    ///
    /// Returned as JSON rather than as a getter struct so the shape crossing to
    /// JS is produced by the very `Serialize` impl the relay deserializes with:
    /// the caller does `JSON.parse` and puts the object on the wire verbatim,
    /// and no hand-written field mapping can drift from the protocol.
    ///
    /// `doc_id` must be the caller's LOCALLY-TRUSTED document id, never one read
    /// off an inbound frame, and `user_id` must be the identity the caller used
    /// to `Identify` — the relay checks it against the presenting connection.
    ///
    /// # Errors
    ///
    /// Returns an error if no MLS group is established (nothing to mint from).
    pub fn mint_subscribe_capability(
        &self,
        user_id: &str,
        doc_id: &str,
        now_unix: u64,
        ttl_secs: u64,
    ) -> Result<String, JsError> {
        let cap = self
            .0
            .mint_subscribe_capability(user_id, doc_id, now_unix, ttl_secs)
            .map_err(js_err)?;
        serde_json::to_string(&cap).map_err(|e| JsError::new(&e.to_string()))
    }

    /// Sign the `RegisterDocKey` self-proof for this document's current epoch —
    /// the TOFU half of anchor registration, paired with
    /// [`Self::subscribe_verifying_key`] (issue #29).
    ///
    /// # Errors
    ///
    /// Returns an error if no MLS group is established.
    pub fn sign_doc_key_proof(&self, doc_id: &str) -> Result<Vec<u8>, JsError> {
        self.0.sign_doc_key_proof(doc_id).map_err(js_err)
    }

    /// The public anchor key for this document's current epoch: what the relay
    /// stores and verifies every presented capability against.
    ///
    /// # Errors
    ///
    /// Returns an error if no MLS group is established.
    pub fn subscribe_verifying_key(&self) -> Result<Vec<u8>, JsError> {
        self.0.subscribe_verifying_key().map(|k| k.to_vec()).map_err(js_err)
    }

    /// Snapshot + encrypt this document's MLS group state for at-rest
    /// persistence (issue #30). `key` MUST be exactly 32 bytes (all-zeros
    /// rejected). Returns `nonce || ciphertext`. The caller owns key provenance
    /// (OS keychain / passphrase) and where the blob is stored (plugin data dir).
    pub fn snapshot_encrypted(&self, key: &[u8]) -> Result<Vec<u8>, JsError> {
        let key = to_key(key)?;
        self.0.snapshot_encrypted(&key).map_err(js_err)
    }

    /// Restore an encrypted document from a snapshot, rebuilding a fresh CRDT doc
    /// under `doc_id`. `key` MUST be exactly 32 bytes. A stale snapshot
    /// (`epoch < min_epoch`) or one with no group surfaces as a JsError
    /// ("stale snapshot; re-join required") since JS cannot easily express
    /// `Option` — the caller must treat that error as "do a clean re-join".
    pub fn restore_encrypted(
        doc_id: &str,
        snapshot: &[u8],
        key: &[u8],
        min_epoch: u64,
    ) -> Result<WasmEncryptedDocument, JsError> {
        let key = to_key(key)?;
        match EncryptedDocument::restore_encrypted(doc_id, snapshot, &key, min_epoch) {
            Ok(Some(doc)) => Ok(Self(doc)),
            Ok(None) => Err(JsError::new("stale snapshot; re-join required")),
            Err(e) => Err(js_err(e)),
        }
    }
}

/// Convert a JS byte slice into a fixed 32-byte AEAD key, rejecting any other
/// length up front (a short/long key can never be a valid AES-256 key).
fn to_key(key: &[u8]) -> Result<[u8; 32], JsError> {
    key.try_into().map_err(|_| JsError::new("key must be exactly 32 bytes"))
}

#[cfg(test)]
mod tests {
    //! Native tests for the capability surface (issue #72). The point of these
    //! is the WIRE SHAPE: a capability minted here must deserialize and verify
    //! with the protocol crate's own code, which is what the relay runs.
    use super::*;

    #[test]
    fn minted_capability_json_verifies_under_the_registered_anchor() {
        let doc = WasmEncryptedDocument::create("doc-a", "alice").unwrap();
        let json = doc.mint_subscribe_capability("alice", "doc-a", 1_000, 300).unwrap();

        let cap: collab_proto::SubscribeCapability = serde_json::from_str(&json).unwrap();
        let key: [u8; 32] = doc.subscribe_verifying_key().unwrap().try_into().unwrap();
        collab_proto::verify_subscribe_capability(&cap, &key, "alice", "doc-a", doc.epoch(), 1_000)
            .unwrap();
        // The registration proof rides the same anchor key.
        assert!(collab_proto::verify_doc_key_proof(
            "doc-a",
            doc.epoch(),
            &key,
            &doc.sign_doc_key_proof("doc-a").unwrap(),
        )
        .is_ok());
    }

    #[test]
    fn the_subscribe_frame_a_client_builds_round_trips() {
        // The plugin does `JSON.parse(mint(...))` and drops the object straight
        // into a subscribe frame; JSON round-tripping is identity, so this is
        // the exact envelope the relay receives.
        let doc = WasmEncryptedDocument::create("doc-a", "alice").unwrap();
        let cap_json = doc.mint_subscribe_capability("alice", "doc-a", 1_000, 300).unwrap();
        let frame = format!(r#"{{"type":"subscribe","doc_id":"doc-a","capability":{cap_json}}}"#);

        let msg: collab_proto::ClientMessage = serde_json::from_str(&frame).unwrap();
        let collab_proto::ClientMessage::Subscribe { doc_id, capability } = msg else {
            panic!("frame did not decode as Subscribe");
        };
        assert_eq!(doc_id, "doc-a");
        assert_eq!(capability.expect("capability decoded").user_id, "alice");
    }

    #[test]
    fn a_capability_for_another_document_is_rejected() {
        let doc = WasmEncryptedDocument::create("doc-a", "alice").unwrap();
        let json = doc.mint_subscribe_capability("alice", "doc-a", 1_000, 300).unwrap();
        let cap: collab_proto::SubscribeCapability = serde_json::from_str(&json).unwrap();
        let key: [u8; 32] = doc.subscribe_verifying_key().unwrap().try_into().unwrap();

        // Presented against a different document (a misroute or a replay), the
        // capability must fail — it is bound to the doc id it was minted for.
        assert_eq!(
            collab_proto::verify_subscribe_capability(
                &cap,
                &key,
                "alice",
                "doc-b",
                doc.epoch(),
                1_000
            ),
            Err(collab_proto::CapabilityError::DocIdMismatch)
        );
        // And presented by another identity within its TTL.
        assert_eq!(
            collab_proto::verify_subscribe_capability(
                &cap,
                &key,
                "eve",
                "doc-a",
                doc.epoch(),
                1_000
            ),
            Err(collab_proto::CapabilityError::UserIdMismatch)
        );
    }
}
