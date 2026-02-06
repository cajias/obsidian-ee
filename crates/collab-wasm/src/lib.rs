use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use getrandom::getrandom;
use wasm_bindgen::prelude::*;
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Doc, GetString, ReadTxn, Text, TextRef, Transact};

/// Error types for WASM operations.
#[derive(Debug, Clone)]
pub enum CollabErrorType {
    Encryption,
    Decryption,
    KeyError,
    SyncError,
}

impl CollabErrorType {
    fn as_str(&self) -> &'static str {
        match self {
            CollabErrorType::Encryption => "encryption",
            CollabErrorType::Decryption => "decryption",
            CollabErrorType::KeyError => "key_error",
            CollabErrorType::SyncError => "sync_error",
        }
    }
}

/// Internal error type for WASM operations.
#[derive(Debug, Clone)]
pub struct CollabError {
    error_type: CollabErrorType,
    message: String,
}

impl CollabError {
    fn new(error_type: CollabErrorType, message: impl Into<String>) -> Self {
        Self { error_type, message: message.into() }
    }

    fn encryption(message: impl Into<String>) -> Self {
        Self::new(CollabErrorType::Encryption, message)
    }

    fn decryption(message: impl Into<String>) -> Self {
        Self::new(CollabErrorType::Decryption, message)
    }

    fn key_error(message: impl Into<String>) -> Self {
        Self::new(CollabErrorType::KeyError, message)
    }

    fn sync_error(message: impl Into<String>) -> Self {
        Self::new(CollabErrorType::SyncError, message)
    }
}

impl std::fmt::Display for CollabError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.error_type.as_str(), self.message)
    }
}

impl std::error::Error for CollabError {}

impl From<CollabError> for JsValue {
    fn from(err: CollabError) -> Self {
        // Create a structured JS object with type and message fields
        let obj = js_sys::Object::new();
        js_sys::Reflect::set(&obj, &"type".into(), &err.error_type.as_str().into()).unwrap();
        js_sys::Reflect::set(&obj, &"message".into(), &err.message.into()).unwrap();
        obj.into()
    }
}

#[wasm_bindgen]
pub struct CollabCore {
    doc: Doc,
    text: TextRef,
    encryption_key: Option<Vec<u8>>,
}

/// Internal implementation (testable without WASM)
impl CollabCore {
    /// Set the encryption key (32 bytes for AES-256).
    /// This is an MVP implementation - will be replaced with MLS later.
    pub fn set_encryption_key_internal(&mut self, key: &[u8]) -> Result<(), CollabError> {
        if key.len() != 32 {
            return Err(CollabError::key_error("Key must be 32 bytes"));
        }
        self.encryption_key = Some(key.to_vec());
        Ok(())
    }

    /// Encrypt data with the current key.
    /// Returns nonce (12 bytes) prepended to ciphertext.
    pub fn encrypt_internal(&self, plaintext: &[u8]) -> Result<Vec<u8>, CollabError> {
        let key = self
            .encryption_key
            .as_ref()
            .ok_or_else(|| CollabError::key_error("No encryption key set"))?;

        let cipher =
            Aes256Gcm::new_from_slice(key).map_err(|e| CollabError::encryption(e.to_string()))?;

        let mut nonce_bytes = [0u8; 12];
        getrandom(&mut nonce_bytes)
            .map_err(|e| CollabError::encryption(format!("Failed to generate nonce: {}", e)))?;
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext =
            cipher.encrypt(nonce, plaintext).map_err(|e| CollabError::encryption(e.to_string()))?;

        // Prepend nonce to ciphertext
        let mut result = nonce_bytes.to_vec();
        result.extend(ciphertext);
        Ok(result)
    }

    /// Decrypt data with the current key.
    /// Expects nonce (12 bytes) prepended to ciphertext.
    pub fn decrypt_internal(&self, ciphertext: &[u8]) -> Result<Vec<u8>, CollabError> {
        if ciphertext.len() < 12 {
            return Err(CollabError::decryption("Ciphertext too short"));
        }

        let key = self
            .encryption_key
            .as_ref()
            .ok_or_else(|| CollabError::key_error("No encryption key set"))?;

        let cipher =
            Aes256Gcm::new_from_slice(key).map_err(|e| CollabError::decryption(e.to_string()))?;

        let (nonce_bytes, encrypted) = ciphertext.split_at(12);
        let nonce = Nonce::from_slice(nonce_bytes);

        cipher.decrypt(nonce, encrypted).map_err(|e| CollabError::decryption(e.to_string()))
    }

    /// Apply an update from another document (internal).
    pub fn apply_update_internal(&mut self, update: &[u8]) -> Result<(), CollabError> {
        let update =
            yrs::Update::decode_v1(update).map_err(|e| CollabError::sync_error(e.to_string()))?;
        let mut txn = self.doc.transact_mut();
        txn.apply_update(update).map_err(|e| CollabError::sync_error(e.to_string()))?;
        Ok(())
    }

    /// Encode state and encrypt it (internal).
    pub fn encode_state_encrypted_internal(&self) -> Result<Vec<u8>, CollabError> {
        let state = self.encode_state();
        self.encrypt_internal(&state)
    }

    /// Decrypt and apply an update (internal).
    pub fn apply_update_encrypted_internal(&mut self, encrypted: &[u8]) -> Result<(), CollabError> {
        let decrypted = self.decrypt_internal(encrypted)?;
        self.apply_update_internal(&decrypted)
    }
}

#[wasm_bindgen]
impl CollabCore {
    /// Create a new CollabCore instance.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        let doc = Doc::new();
        let text = doc.get_or_insert_text("content");
        Self { doc, text, encryption_key: None }
    }

    /// Get the current text content.
    pub fn get_text(&self) -> String {
        let txn = self.doc.transact();
        self.text.get_string(&txn)
    }

    /// Insert text at the given position.
    pub fn insert(&mut self, index: u32, content: &str) {
        let mut txn = self.doc.transact_mut();
        self.text.insert(&mut txn, index, content);
    }

    /// Delete text from the given position.
    pub fn delete(&mut self, index: u32, length: u32) {
        let mut txn = self.doc.transact_mut();
        self.text.remove_range(&mut txn, index, length);
    }

    /// Get the document state as an update blob.
    pub fn encode_state(&self) -> Vec<u8> {
        let txn = self.doc.transact();
        txn.encode_state_as_update_v1(&yrs::StateVector::default())
    }

    /// Apply an update from another document.
    pub fn apply_update(&mut self, update: &[u8]) -> Result<(), JsValue> {
        self.apply_update_internal(update).map_err(Into::into)
    }

    /// Get the state vector for syncing.
    pub fn encode_state_vector(&self) -> Vec<u8> {
        let txn = self.doc.transact();
        txn.state_vector().encode_v1()
    }

    /// Set the encryption key (32 bytes for AES-256).
    /// This is an MVP implementation - will be replaced with MLS later.
    pub fn set_encryption_key(&mut self, key: &[u8]) -> Result<(), JsValue> {
        self.set_encryption_key_internal(key).map_err(Into::into)
    }

    /// Check if an encryption key is set.
    pub fn has_encryption_key(&self) -> bool {
        self.encryption_key.is_some()
    }

    /// Encrypt data with the current key.
    /// Returns nonce (12 bytes) prepended to ciphertext.
    pub fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>, JsValue> {
        self.encrypt_internal(plaintext).map_err(Into::into)
    }

    /// Decrypt data with the current key.
    /// Expects nonce (12 bytes) prepended to ciphertext.
    pub fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>, JsValue> {
        self.decrypt_internal(ciphertext).map_err(Into::into)
    }

    /// Encode state and encrypt it.
    pub fn encode_state_encrypted(&self) -> Result<Vec<u8>, JsValue> {
        self.encode_state_encrypted_internal().map_err(Into::into)
    }

    /// Decrypt and apply an update.
    pub fn apply_update_encrypted(&mut self, encrypted: &[u8]) -> Result<(), JsValue> {
        self.apply_update_encrypted_internal(encrypted).map_err(Into::into)
    }
}

impl Default for CollabCore {
    fn default() -> Self {
        Self::new()
    }
}

/// A simple greeting function to verify the WASM build works.
#[wasm_bindgen]
pub fn greet(name: &str) -> String {
    format!("Hello, {}! Welcome to collab-wasm.", name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_collab_core_insert_and_get() {
        let mut core = CollabCore::new();
        core.insert(0, "Hello, World!");
        assert_eq!(core.get_text(), "Hello, World!");
    }

    #[test]
    fn test_collab_core_delete() {
        let mut core = CollabCore::new();
        core.insert(0, "Hello, World!");
        core.delete(0, 7);
        assert_eq!(core.get_text(), "World!");
    }

    #[test]
    fn test_collab_core_sync() {
        let mut core1 = CollabCore::new();
        let mut core2 = CollabCore::new();

        core1.insert(0, "Hello from core1!");
        let update = core1.encode_state();

        core2.apply_update_internal(&update).unwrap();
        assert_eq!(core2.get_text(), "Hello from core1!");
    }

    #[test]
    fn test_greet() {
        assert_eq!(greet("Alice"), "Hello, Alice! Welcome to collab-wasm.");
    }

    #[test]
    fn test_set_encryption_key_valid() {
        let mut core = CollabCore::new();
        let key = [0u8; 32];
        assert!(core.set_encryption_key_internal(&key).is_ok());
        assert!(core.has_encryption_key());
    }

    #[test]
    fn test_set_encryption_key_invalid_length() {
        let mut core = CollabCore::new();
        let key = [0u8; 16]; // Too short
        assert!(core.set_encryption_key_internal(&key).is_err());
        assert!(!core.has_encryption_key());
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let mut core = CollabCore::new();
        let key = [0u8; 32]; // Test key
        core.set_encryption_key_internal(&key).unwrap();

        let plaintext = b"Hello, encrypted world!";
        let encrypted = core.encrypt_internal(plaintext).unwrap();
        let decrypted = core.decrypt_internal(&encrypted).unwrap();

        assert_eq!(plaintext.to_vec(), decrypted);
    }

    #[test]
    fn test_encrypt_without_key() {
        let core = CollabCore::new();
        let plaintext = b"Hello";
        assert!(core.encrypt_internal(plaintext).is_err());
    }

    #[test]
    fn test_decrypt_without_key() {
        let core = CollabCore::new();
        let ciphertext = [0u8; 32];
        assert!(core.decrypt_internal(&ciphertext).is_err());
    }

    #[test]
    fn test_decrypt_too_short() {
        let mut core = CollabCore::new();
        let key = [0u8; 32];
        core.set_encryption_key_internal(&key).unwrap();

        let short_ciphertext = [0u8; 8]; // Less than 12 bytes
        assert!(core.decrypt_internal(&short_ciphertext).is_err());
    }

    #[test]
    fn test_encrypted_state_sync() {
        let mut core1 = CollabCore::new();
        let mut core2 = CollabCore::new();
        let key = [0u8; 32];
        core1.set_encryption_key_internal(&key).unwrap();
        core2.set_encryption_key_internal(&key).unwrap();

        core1.insert(0, "Encrypted sync!");
        let encrypted = core1.encode_state_encrypted_internal().unwrap();

        core2.apply_update_encrypted_internal(&encrypted).unwrap();
        assert_eq!(core2.get_text(), "Encrypted sync!");
    }

    #[test]
    fn test_encrypted_sync_wrong_key() {
        let mut core1 = CollabCore::new();
        let mut core2 = CollabCore::new();
        let key1 = [0u8; 32];
        let key2 = [1u8; 32]; // Different key
        core1.set_encryption_key_internal(&key1).unwrap();
        core2.set_encryption_key_internal(&key2).unwrap();

        core1.insert(0, "Secret message");
        let encrypted = core1.encode_state_encrypted_internal().unwrap();

        // Should fail to decrypt with wrong key
        assert!(core2.apply_update_encrypted_internal(&encrypted).is_err());
    }

    #[test]
    fn test_nonces_are_unique() {
        // Verifies that the entropy source produces unique nonces
        // If entropy fails, we'd get repeated nonces which is catastrophic for AES-GCM
        let mut core = CollabCore::new();
        let key = [0u8; 32];
        core.set_encryption_key_internal(&key).unwrap();

        let plaintext = b"test data";
        let mut nonces = std::collections::HashSet::new();

        // Generate 100 encryptions and verify all nonces are unique
        for _ in 0..100 {
            let encrypted = core.encrypt_internal(plaintext).unwrap();
            // Nonce is the first 12 bytes
            let nonce: [u8; 12] = encrypted[..12].try_into().unwrap();
            assert!(
                nonces.insert(nonce),
                "Duplicate nonce detected! Entropy source may be broken."
            );
        }
    }
}
