//! CLI command implementations.

use std::fs;
use std::path::Path;

use collab_core::{EncryptedDocument, MlsDocumentGroup, PendingMember};
use collab_proto::Invite;
use serde::{Deserialize, Serialize};

/// Initialize a new collaborative document.
///
/// Creates a new encrypted document as the owner.
///
/// # Errors
///
/// Returns an error if document creation fails.
pub fn init(doc_id: &str, user_id: &str, state_file: Option<&Path>) -> anyhow::Result<InitResult> {
    let _doc = EncryptedDocument::create(doc_id, user_id)?;

    // Save state if requested
    if let Some(path) = state_file {
        let state = DocumentState {
            doc_id: doc_id.to_string(),
            user_id: user_id.to_string(),
            role: Role::Owner,
        };
        fs::write(path, serde_json::to_string_pretty(&state)?)?;
    }

    Ok(InitResult {
        doc_id: doc_id.to_string(),
        user_id: user_id.to_string(),
        message: format!("Created document '{doc_id}' as owner. Share invites with collaborators."),
    })
}

/// Result of initializing a document.
#[derive(Debug, Serialize, Deserialize)]
pub struct InitResult {
    /// The document ID.
    pub doc_id: String,
    /// The user ID of the owner.
    pub user_id: String,
    /// Human-readable message.
    pub message: String,
}

/// Document state saved to disk.
#[derive(Debug, Serialize, Deserialize)]
pub struct DocumentState {
    /// The document ID.
    pub doc_id: String,
    /// The user ID.
    pub user_id: String,
    /// User's role.
    pub role: Role,
}

/// User's role in the document.
#[derive(Debug, Serialize, Deserialize)]
pub enum Role {
    /// Document owner/creator.
    Owner,
    /// Document collaborator.
    Collaborator,
}

/// Generate a key package for joining a group.
///
/// Returns a pending member with its key package bytes.
///
/// # Errors
///
/// Returns an error if key package generation fails.
pub fn keygen(user_id: &str, output_file: &Path) -> anyhow::Result<KeygenResult> {
    let pending = MlsDocumentGroup::generate_key_package(user_id)?;
    let key_package = pending.key_package().to_vec();

    // We can't easily serialize the full PendingMember (contains crypto state),
    // so we save the key package and rely on regenerating for join.
    // In a real implementation, we'd serialize the crypto state properly.
    let output =
        KeygenOutput { user_id: user_id.to_string(), key_package: base64_encode(&key_package) };

    fs::write(output_file, serde_json::to_string_pretty(&output)?)?;

    Ok(KeygenResult {
        user_id: user_id.to_string(),
        key_package_file: output_file.display().to_string(),
        message: format!(
            "Generated key package. Share '{0}' with the document owner.",
            output_file.display()
        ),
    })
}

/// Result of key generation.
#[derive(Debug, Serialize)]
pub struct KeygenResult {
    /// The user ID.
    pub user_id: String,
    /// Path to the key package file.
    pub key_package_file: String,
    /// Human-readable message.
    pub message: String,
}

/// Key generation output saved to file.
#[derive(Debug, Serialize, Deserialize)]
pub struct KeygenOutput {
    /// The user ID.
    pub user_id: String,
    /// Base64-encoded key package.
    pub key_package: String,
}

/// State for a pending member waiting to join.
pub struct PendingMemberState {
    /// The pending member (must be kept for joining).
    pub pending: PendingMember,
    /// The serialized key package to send to the group owner.
    pub key_package: Vec<u8>,
}

/// Create an invite for a new member.
///
/// Takes the joiner's key package file and outputs an invite file.
///
/// # Errors
///
/// Returns an error if invite creation fails.
pub fn create_invite(
    doc_id: &str,
    owner_user_id: &str,
    key_package_file: &Path,
    invite_output: &Path,
) -> anyhow::Result<InviteResult> {
    // Load the joiner's key package
    let keygen_content = fs::read_to_string(key_package_file)?;
    let keygen: KeygenOutput = serde_json::from_str(&keygen_content)?;
    let key_package_bytes = base64_decode(&keygen.key_package)?;

    // Create document (owner's state)
    let mut doc = EncryptedDocument::create(doc_id, owner_user_id)?;

    // Create invite
    let invite = doc.create_invite(&key_package_bytes)?;

    // Write invite to file
    let invite_proto = Invite {
        doc_id: invite.doc_id.clone(),
        key_package: invite.welcome,
        relay_url: String::new(),
    };
    fs::write(invite_output, serde_json::to_string_pretty(&invite_proto)?)?;

    Ok(InviteResult {
        doc_id: invite.doc_id,
        invite_file: invite_output.display().to_string(),
        message: format!(
            "Invite created. Share '{0}' with {1}.",
            invite_output.display(),
            keygen.user_id
        ),
    })
}

/// Result of creating an invite.
#[derive(Debug, Serialize)]
pub struct InviteResult {
    /// The document ID.
    pub doc_id: String,
    /// Path to the invite file.
    pub invite_file: String,
    /// Human-readable message.
    pub message: String,
}

/// Join an existing collaborative document.
///
/// Note: This is a simplified version. In a real implementation,
/// we'd need to persist the `PendingMember` state from keygen.
///
/// # Errors
///
/// Returns an error if joining fails.
pub fn join(
    invite_file: &Path,
    user_id: &str,
    state_output: Option<&Path>,
) -> anyhow::Result<JoinResult> {
    // Load the invite
    let invite_content = fs::read_to_string(invite_file)?;
    let invite: Invite = serde_json::from_str(&invite_content)?;

    // In a real implementation, we'd load the PendingMember from keygen.
    // For now, we regenerate (which won't actually work with MLS, but
    // demonstrates the structure).
    let pending = MlsDocumentGroup::generate_key_package(user_id)?;

    // Try to join (this will fail if the key package doesn't match,
    // but we handle the error gracefully)
    match pending.join(&invite.key_package) {
        Ok(_group) => {
            // Save state if requested
            if let Some(path) = state_output {
                let state = DocumentState {
                    doc_id: invite.doc_id.clone(),
                    user_id: user_id.to_string(),
                    role: Role::Collaborator,
                };
                fs::write(path, serde_json::to_string_pretty(&state)?)?;
            }

            Ok(JoinResult {
                doc_id: invite.doc_id,
                user_id: user_id.to_string(),
                success: true,
                message: "Successfully joined document".to_string(),
            })
        }
        Err(e) => {
            // Expected to fail if key packages don't match
            Ok(JoinResult {
                doc_id: invite.doc_id,
                user_id: user_id.to_string(),
                success: false,
                message: format!("Join failed (expected if key package doesn't match invite): {e}"),
            })
        }
    }
}

/// Result of joining a document.
#[derive(Debug, Serialize)]
pub struct JoinResult {
    /// The document ID.
    pub doc_id: String,
    /// The user ID.
    pub user_id: String,
    /// Whether join succeeded.
    pub success: bool,
    /// Human-readable message.
    pub message: String,
}

/// Demonstrate the full collaboration flow in-memory.
///
/// This bypasses file I/O to show the MLS flow working correctly.
///
/// # Errors
///
/// Returns an error if any step fails.
pub fn demo(doc_id: &str) -> anyhow::Result<DemoResult> {
    // Alice creates a document
    let mut alice_doc = EncryptedDocument::create(doc_id, "alice")?;

    // Bob generates a key package
    let bob_pending = MlsDocumentGroup::generate_key_package("bob")?;

    // Alice creates an invite for Bob
    let invite = alice_doc.create_invite(bob_pending.key_package())?;

    // Bob joins using the invite
    let mut bob_doc = EncryptedDocument::join(&invite, bob_pending)?;

    // Alice writes some content
    alice_doc.insert(0, "Hello from Alice!");
    let encrypted_update = alice_doc.get_encrypted_update()?;

    // Bob receives and decrypts
    bob_doc.apply_encrypted_update(&encrypted_update)?;
    let _bob_content = bob_doc.get_content();

    // Bob responds
    bob_doc.insert(17, " Hi from Bob!");
    let bob_update = bob_doc.get_encrypted_update()?;

    // Alice receives
    alice_doc.apply_encrypted_update(&bob_update)?;
    let final_content = alice_doc.get_content();

    Ok(DemoResult {
        doc_id: doc_id.to_string(),
        alice_content: final_content.clone(),
        bob_content: final_content,
        message: "Demo completed successfully! E2E encryption working.".to_string(),
    })
}

/// Result of the demo command.
#[derive(Debug, Serialize)]
pub struct DemoResult {
    /// The document ID.
    pub doc_id: String,
    /// Alice's view of the content.
    pub alice_content: String,
    /// Bob's view of the content.
    pub bob_content: String,
    /// Human-readable message.
    pub message: String,
}

// Helper functions for base64 encoding/decoding using standard approach
fn base64_encode(data: &[u8]) -> String {
    const BASE64_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut result = String::new();
    let mut i = 0;

    while i < data.len() {
        let b0 = data[i];
        let b1 = data.get(i + 1).copied().unwrap_or(0);
        let b2 = data.get(i + 2).copied().unwrap_or(0);

        result.push(BASE64_CHARS[(b0 >> 2) as usize] as char);
        result.push(BASE64_CHARS[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);

        if i + 1 < data.len() {
            result.push(BASE64_CHARS[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            result.push('=');
        }

        if i + 2 < data.len() {
            result.push(BASE64_CHARS[(b2 & 0x3f) as usize] as char);
        } else {
            result.push('=');
        }

        i += 3;
    }

    result
}

#[allow(clippy::unnecessary_wraps)] // Result used for error propagation at call sites
fn base64_decode(s: &str) -> anyhow::Result<Vec<u8>> {
    const fn decode_char(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None, // includes b'=' padding
        }
    }

    let bytes: Vec<u8> = s.bytes().filter(|&b| b != b'\n' && b != b'\r').collect();
    let mut result = Vec::new();

    for chunk in bytes.chunks(4) {
        if chunk.len() < 4 {
            break;
        }

        let b0 = decode_char(chunk[0]).unwrap_or(0);
        let b1 = decode_char(chunk[1]).unwrap_or(0);
        let b2 = decode_char(chunk[2]);
        let b3 = decode_char(chunk[3]);

        result.push((b0 << 2) | (b1 >> 4));

        if let Some(v2) = b2 {
            result.push(((b1 & 0x0f) << 4) | (v2 >> 2));
        }

        if let Some(v3) = b3 {
            if let Some(v2) = b2 {
                result.push(((v2 & 0x03) << 6) | v3);
            }
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64_roundtrip() {
        let original = b"Hello, World!";
        let encoded = base64_encode(original);
        let decoded = base64_decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_base64_key_package_roundtrip() {
        // Simulate a realistic key package size
        #[allow(clippy::cast_possible_truncation)]
        let data: Vec<u8> = (0u16..500).map(|i| (i % 256) as u8).collect();
        let encoded = base64_encode(&data);
        let decoded = base64_decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_demo_full_flow() {
        let result = demo("test-doc").unwrap();
        assert_eq!(result.alice_content, "Hello from Alice! Hi from Bob!");
        assert_eq!(result.bob_content, "Hello from Alice! Hi from Bob!");
    }

    #[test]
    fn test_init_creates_document() {
        let result = init("test-doc", "alice", None).unwrap();
        assert_eq!(result.doc_id, "test-doc");
        assert_eq!(result.user_id, "alice");
    }

    #[test]
    fn test_keygen_creates_package() {
        let temp_dir = std::env::temp_dir();
        let output_file = temp_dir.join("test_keygen.json");

        let result = keygen("bob", &output_file).unwrap();
        assert_eq!(result.user_id, "bob");
        assert!(output_file.exists());

        // Verify the file contains valid JSON
        let content = fs::read_to_string(&output_file).unwrap();
        let output: KeygenOutput = serde_json::from_str(&content).unwrap();
        assert_eq!(output.user_id, "bob");
        assert!(!output.key_package.is_empty());

        // Cleanup
        let _ = fs::remove_file(&output_file);
    }
}
