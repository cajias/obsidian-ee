//! wasm-bindgen surface exposing collab-core's MLS engine to JS (issue #28).
//!
//! Thin owning wrappers over `collab_core` value types; no crypto lives here.
//! Errors surface as real JS `Error`s via [`js_err`], unlike the AES `CollabCore`
//! path which returns `{type, message}` objects.

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

    #[wasm_bindgen(getter)]
    pub fn epoch(&self) -> u64 {
        self.0.epoch()
    }
}
