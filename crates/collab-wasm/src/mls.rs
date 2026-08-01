//! wasm-bindgen surface exposing collab-core's MLS engine to JS (issue #28).
//!
//! Thin owning wrappers over `collab_core` value types; no crypto lives here.
//! Errors surface as real JS `Error`s via [`js_err`], unlike the removed AES
//! path which returned `{type, message}` objects.

use collab_core::{EncryptedDocument, EncryptedOp, Invite, MlsDocumentGroup, PendingMember};
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

    pub fn process_commit(&mut self, commit: &[u8]) -> Result<(), JsError> {
        self.0.process_commit(commit).map_err(js_err)
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
}
