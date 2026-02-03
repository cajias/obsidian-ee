# E2E Collaborative Editing - Completion Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Complete Phase 3 of the obsidian-ee project: fix CLI, implement real E2E tests with Docker, create a recorded demo.

**Architecture:** CLI commands enable MLS key exchange via files. E2E tests spin up Docker Compose (LocalStack + Redis + relay), run actual collaboration scenarios, and record terminal sessions for verification.

**Tech Stack:** Rust (cargo workspace), MLS/OpenMLS, Yrs CRDT, Docker Compose, asciinema for recordings

---

## Task Dependency Graph

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                        PHASE 3A: FIX & VERIFY (Sequential)                  │
│                        Must complete before parallel work                    │
├─────────────────────────────────────────────────────────────────────────────┤
│  T1: Fix CLI base64 compilation errors                                      │
│        │                                                                     │
│        ▼                                                                     │
│  T2: Verify all tests pass (cargo test --workspace)                         │
│        │                                                                     │
│        ▼                                                                     │
│  T3: Verify pre-commit hooks work                                           │
└─────────────────────────────────────────────────────────────────────────────┘
                                      │
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                     PHASE 3B: PARALLEL WORKTREE TASKS                        │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  ┌────────────────────┐    ┌────────────────────┐    ┌─────────────────┐   │
│  │ WORKTREE: e2e-test │    │ WORKTREE: cli-enh  │    │ WORKTREE: infra │   │
│  ├────────────────────┤    ├────────────────────┤    ├─────────────────┤   │
│  │ T4: E2E test       │    │ T7: CLI connect    │    │ T10: CDK review │   │
│  │    infrastructure  │    │    command impl    │    │     & deploy    │   │
│  │        │           │    │        │           │    │                 │   │
│  │        ▼           │    │        ▼           │    │                 │   │
│  │ T5: Full collab    │    │ T8: CLI TUI/       │    │                 │   │
│  │    flow test       │    │    interactive     │    │                 │   │
│  │        │           │    │                    │    │                 │   │
│  │        ▼           │    │                    │    │                 │   │
│  │ T6: Offline sync   │    │                    │    │                 │   │
│  │    test            │    │                    │    │                 │   │
│  └────────────────────┘    └────────────────────┘    └─────────────────┘   │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
                                      │
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                     PHASE 3C: INTEGRATION & DEMO (Sequential)               │
├─────────────────────────────────────────────────────────────────────────────┤
│  T11: Merge worktrees, resolve conflicts                                    │
│         │                                                                    │
│         ▼                                                                    │
│  T12: Full CI run verification                                              │
│         │                                                                    │
│         ▼                                                                    │
│  T13: Record asciinema demo                                                 │
│         │                                                                    │
│         ▼                                                                    │
│  T14: Create PR with all changes                                            │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## PHASE 3A: Fix & Verify (Sequential - Main Branch)

### Task 1: Fix CLI Base64 Compilation Errors

**Files:**
- Modify: `crates/collab-cli/src/commands.rs:314-385`

**Problem:** The `Base64Writer` struct has a `Drop` impl that requires `W: std::io::Write` but the struct definition doesn't specify this bound.

**Step 1: Fix the Base64Writer struct definition**

Replace the manual base64 implementation with a simpler direct approach:

```rust
// Replace lines 295-455 with:

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

fn base64_decode(s: &str) -> anyhow::Result<Vec<u8>> {
    fn decode_char(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            b'=' => None,
            _ => None,
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
```

**Step 2: Run tests to verify fix**

```bash
cargo test -p collab-cli
```

Expected: All tests pass

**Step 3: Commit the fix**

```bash
git add crates/collab-cli/src/commands.rs
git commit -m "fix(cli): replace complex base64 impl with simple direct approach"
```

---

### Task 2: Verify All Tests Pass

**Step 1: Run full test suite**

```bash
cargo test --workspace
```

Expected: 33+ tests pass (collab-core: 12, collab-relay: 21, collab-cli: 4+)

**Step 2: Run clippy**

```bash
cargo lint
```

Expected: No errors (warnings OK)

**Step 3: Check formatting**

```bash
cargo fmt --all -- --check
```

Expected: No formatting issues

---

### Task 3: Verify Pre-commit Hooks

**Step 1: Install pre-commit if needed**

```bash
which pre-commit || pip install pre-commit
pre-commit install
```

**Step 2: Run hooks manually**

```bash
pre-commit run --all-files
```

Expected: All hooks pass

**Step 3: Make a test commit to verify**

```bash
echo "# Test" >> README.md
git add README.md
git commit -m "test: verify pre-commit hooks"
git reset --soft HEAD~1
git checkout README.md
```

---

## PHASE 3B: Parallel Worktree Tasks

### Worktree Setup

Create dedicated branches for parallel work:

```bash
cd /Users/rc/Projects/workspace/obsidian-ee

# Create branches from main
git checkout main
git pull origin main 2>/dev/null || true

# E2E tests worktree
git branch -D feature/e2e-tests 2>/dev/null || true
git checkout -b feature/e2e-tests
git checkout main
git worktree add ../obsidian-ee-e2e feature/e2e-tests

# CLI enhancements worktree
git branch -D feature/cli-connect 2>/dev/null || true
git checkout -b feature/cli-connect
git checkout main
git worktree add ../obsidian-ee-cli feature/cli-connect
```

---

### Worktree A: E2E Tests (`obsidian-ee-e2e`)

#### Task 4: E2E Test Infrastructure

**Files:**
- Create: `tests/e2e-tests/tests/full_flow.rs`
- Modify: `tests/e2e-tests/src/helpers.rs`

**Step 1: Write TestClient helper**

Add to `tests/e2e-tests/src/helpers.rs`:

```rust
use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tokio_tungstenite::tungstenite::Message;
use collab_proto::{ClientMessage, ServerMessage};

/// Test client for E2E testing.
pub struct TestClient {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    pub user_id: String,
}

impl TestClient {
    /// Connect to the relay server.
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        let (ws, _) = connect_async(url).await?;
        Ok(Self {
            ws,
            user_id: String::new(),
        })
    }

    /// Connect and identify as a user.
    pub async fn connect_as(url: &str, user_id: &str) -> anyhow::Result<Self> {
        let mut client = Self::connect(url).await?;
        client.user_id = user_id.to_string();

        // Send identify message
        let msg = ClientMessage::Identify { user_id: user_id.to_string() };
        client.send(&msg).await?;

        // Wait for identified response
        let response = client.recv().await?;
        if !matches!(response, ServerMessage::Identified { .. }) {
            anyhow::bail!("Expected Identified response");
        }

        Ok(client)
    }

    /// Send a message.
    pub async fn send(&mut self, msg: &ClientMessage) -> anyhow::Result<()> {
        let json = serde_json::to_string(msg)?;
        self.ws.send(Message::Text(json)).await?;
        Ok(())
    }

    /// Receive a message with timeout.
    pub async fn recv(&mut self) -> anyhow::Result<ServerMessage> {
        let result = timeout(Duration::from_secs(10), self.ws.next()).await?;
        match result {
            Some(Ok(Message::Text(text))) => Ok(serde_json::from_str(&text)?),
            Some(Ok(Message::Close(_))) => anyhow::bail!("Connection closed"),
            Some(Err(e)) => Err(e.into()),
            None => anyhow::bail!("Stream ended"),
            _ => anyhow::bail!("Unexpected message type"),
        }
    }

    /// Try to receive with short timeout (for checking no message case).
    pub async fn recv_timeout(&mut self, ms: u64) -> Option<ServerMessage> {
        timeout(Duration::from_millis(ms), self.ws.next())
            .await
            .ok()
            .and_then(|r| r)
            .and_then(|r| r.ok())
            .and_then(|msg| {
                if let Message::Text(text) = msg {
                    serde_json::from_str(&text).ok()
                } else {
                    None
                }
            })
    }
}
```

**Step 2: Run to verify it compiles**

```bash
cargo build -p e2e-tests
```

**Step 3: Commit**

```bash
git add tests/e2e-tests/src/helpers.rs
git commit -m "feat(e2e): add TestClient helper for WebSocket testing"
```

---

#### Task 5: Full Collaboration Flow Test

**Files:**
- Create: `tests/e2e-tests/tests/full_flow.rs`

**Step 1: Write the full flow test**

```rust
//! Full end-to-end collaboration flow test.
//!
//! Requires Docker Compose to be running:
//! ```
//! docker compose -f docker/docker-compose.yml up -d
//! ```

use collab_core::{EncryptedDocument, MlsDocumentGroup};
use e2e_tests::helpers::TestClient;
use collab_proto::{ClientMessage, ServerMessage};

/// Test that two users can collaborate on a document with E2E encryption.
#[tokio::test]
#[ignore] // Run with: cargo test -p e2e-tests --test full_flow -- --ignored
async fn test_two_users_collaborate() {
    let relay_url = std::env::var("RELAY_URL")
        .unwrap_or_else(|_| "ws://localhost:8080".to_string());

    // === Step 1: Create encrypted documents ===
    let mut alice_doc = EncryptedDocument::create("test-doc", "alice")
        .expect("Alice creates document");

    let bob_pending = MlsDocumentGroup::generate_key_package("bob")
        .expect("Bob generates key package");

    let invite = alice_doc.create_invite(bob_pending.key_package())
        .expect("Alice creates invite for Bob");

    let mut bob_doc = EncryptedDocument::join(&invite, bob_pending)
        .expect("Bob joins document");

    // === Step 2: Connect to relay ===
    let mut alice = TestClient::connect_as(&relay_url, "alice").await
        .expect("Alice connects");
    let mut bob = TestClient::connect_as(&relay_url, "bob").await
        .expect("Bob connects");

    // === Step 3: Subscribe to document ===
    alice.send(&ClientMessage::Subscribe { doc_id: "test-doc".into() }).await.unwrap();
    bob.send(&ClientMessage::Subscribe { doc_id: "test-doc".into() }).await.unwrap();

    // Wait for subscription confirmations
    let _ = alice.recv().await;
    let _ = bob.recv().await;

    // === Step 4: Alice makes an edit ===
    alice_doc.insert(0, "Hello from Alice!");
    let alice_update = alice_doc.get_encrypted_update()
        .expect("Alice gets encrypted update");

    alice.send(&ClientMessage::YrsUpdate {
        doc_id: "test-doc".into(),
        data: alice_update.ciphertext.clone(),
        epoch: alice_update.epoch,
    }).await.unwrap();

    // === Step 5: Bob receives the update ===
    let msg = bob.recv().await.expect("Bob receives message");

    if let ServerMessage::YrsUpdate { data, .. } = msg {
        // Verify ciphertext doesn't contain plaintext
        let plaintext_check = String::from_utf8_lossy(&data);
        assert!(!plaintext_check.contains("Hello"), "Ciphertext should not contain plaintext");

        // Bob decrypts and applies
        let decrypted_op = collab_core::EncryptedOp {
            ciphertext: data,
            epoch: alice_update.epoch,
        };
        bob_doc.apply_encrypted_update(&decrypted_op)
            .expect("Bob applies update");

        assert_eq!(bob_doc.get_content(), "Hello from Alice!");
    } else {
        panic!("Expected YrsUpdate, got {:?}", msg);
    }

    // === Step 6: Bob responds ===
    bob_doc.insert(17, " Hi from Bob!");
    let bob_update = bob_doc.get_encrypted_update()
        .expect("Bob gets encrypted update");

    bob.send(&ClientMessage::YrsUpdate {
        doc_id: "test-doc".into(),
        data: bob_update.ciphertext.clone(),
        epoch: bob_update.epoch,
    }).await.unwrap();

    // === Step 7: Alice receives Bob's update ===
    let msg = alice.recv().await.expect("Alice receives message");

    if let ServerMessage::YrsUpdate { data, .. } = msg {
        let decrypted_op = collab_core::EncryptedOp {
            ciphertext: data,
            epoch: bob_update.epoch,
        };
        alice_doc.apply_encrypted_update(&decrypted_op)
            .expect("Alice applies update");
    }

    // === Step 8: Verify convergence ===
    let alice_content = alice_doc.get_content();
    let bob_content = bob_doc.get_content();

    assert_eq!(alice_content, bob_content, "Documents should converge");
    assert!(alice_content.contains("Hello from Alice!"), "Should contain Alice's text");
    assert!(alice_content.contains("Hi from Bob!"), "Should contain Bob's text");

    println!("✓ E2E test passed! Final content: {}", alice_content);
}

/// Test that encrypted messages don't leak plaintext.
#[tokio::test]
async fn test_encryption_hides_content() {
    let mut alice_doc = EncryptedDocument::create("secret-doc", "alice")
        .expect("Alice creates document");

    // Add Bob so we have a group to encrypt for
    let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
    let _invite = alice_doc.create_invite(bob_pending.key_package()).unwrap();

    alice_doc.insert(0, "TOP SECRET: nuclear codes are 12345");
    let encrypted = alice_doc.get_encrypted_update().unwrap();

    // Check ciphertext
    let ciphertext_str = String::from_utf8_lossy(&encrypted.ciphertext);
    assert!(!ciphertext_str.contains("SECRET"), "Should not contain SECRET");
    assert!(!ciphertext_str.contains("nuclear"), "Should not contain nuclear");
    assert!(!ciphertext_str.contains("12345"), "Should not contain 12345");

    println!("✓ Encryption test passed - plaintext not visible in ciphertext");
}
```

**Step 2: Add collab_core::EncryptedOp to exports if needed**

Check `crates/collab-core/src/lib.rs` and add export:

```rust
pub use encryption::{EncryptedDocument, EncryptedOp, Invite};
```

**Step 3: Run the non-docker test**

```bash
cargo test -p e2e-tests test_encryption_hides_content
```

**Step 4: Commit**

```bash
git add tests/e2e-tests/tests/full_flow.rs crates/collab-core/src/lib.rs
git commit -m "feat(e2e): add full collaboration flow test with encryption verification"
```

---

#### Task 6: Offline Sync Test

**Files:**
- Add to: `tests/e2e-tests/tests/full_flow.rs`

**Step 1: Add offline test**

```rust
/// Test that offline messages are queued and delivered on reconnect.
#[tokio::test]
#[ignore] // Requires Docker
async fn test_offline_message_delivery() {
    let relay_url = std::env::var("RELAY_URL")
        .unwrap_or_else(|_| "ws://localhost:8080".to_string());

    // Alice and Bob set up encryption
    let mut alice_doc = EncryptedDocument::create("offline-doc", "alice").unwrap();
    let bob_pending = MlsDocumentGroup::generate_key_package("bob").unwrap();
    let invite = alice_doc.create_invite(bob_pending.key_package()).unwrap();
    let mut bob_doc = EncryptedDocument::join(&invite, bob_pending).unwrap();

    // Alice connects and subscribes
    let mut alice = TestClient::connect_as(&relay_url, "alice").await.unwrap();
    alice.send(&ClientMessage::Subscribe { doc_id: "offline-doc".into() }).await.unwrap();
    let _ = alice.recv().await; // Subscription confirmation

    // Bob connects, subscribes, then disconnects
    {
        let mut bob = TestClient::connect_as(&relay_url, "bob").await.unwrap();
        bob.send(&ClientMessage::Subscribe { doc_id: "offline-doc".into() }).await.unwrap();
        let _ = bob.recv().await;
        // Bob disconnects when dropped
    }

    // Alice sends while Bob is offline
    alice_doc.insert(0, "Sent while you were away");
    let update = alice_doc.get_encrypted_update().unwrap();
    alice.send(&ClientMessage::YrsUpdate {
        doc_id: "offline-doc".into(),
        data: update.ciphertext.clone(),
        epoch: update.epoch,
    }).await.unwrap();

    // Small delay for message to be queued
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Bob reconnects
    let mut bob2 = TestClient::connect_as(&relay_url, "bob").await.unwrap();
    bob2.send(&ClientMessage::Subscribe { doc_id: "offline-doc".into() }).await.unwrap();

    // Bob should receive the queued message
    let msg = bob2.recv().await.expect("Bob should receive queued message");

    if let ServerMessage::YrsUpdate { data, .. } = msg {
        let op = collab_core::EncryptedOp {
            ciphertext: data,
            epoch: update.epoch,
        };
        bob_doc.apply_encrypted_update(&op).unwrap();
        assert_eq!(bob_doc.get_content(), "Sent while you were away");
        println!("✓ Offline sync test passed!");
    } else {
        panic!("Expected queued YrsUpdate, got {:?}", msg);
    }
}
```

**Step 2: Commit**

```bash
git add tests/e2e-tests/tests/full_flow.rs
git commit -m "feat(e2e): add offline message delivery test"
```

---

### Worktree B: CLI Connect (`obsidian-ee-cli`)

#### Task 7: CLI Connect Command Implementation

**Files:**
- Modify: `crates/collab-cli/src/main.rs`
- Modify: `crates/collab-cli/src/commands.rs`

**Step 1: Add connect command implementation**

Add to `commands.rs`:

```rust
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use futures::{SinkExt, StreamExt};

/// Connect to a relay server and collaborate.
pub async fn connect(
    relay_url: &str,
    user_id: &str,
    doc_id: &str,
    state_file: Option<&Path>,
) -> anyhow::Result<()> {
    use collab_proto::{ClientMessage, ServerMessage};
    use tokio_tungstenite::tungstenite::Message;

    println!("Connecting to {} as {} for document {}...", relay_url, user_id, doc_id);

    // Connect to relay
    let (ws, _) = connect_async(relay_url).await?;
    let (mut write, mut read) = ws.split();

    // Identify
    let identify = ClientMessage::Identify { user_id: user_id.to_string() };
    write.send(Message::Text(serde_json::to_string(&identify)?)).await?;

    // Subscribe to document
    let subscribe = ClientMessage::Subscribe { doc_id: doc_id.to_string() };
    write.send(Message::Text(serde_json::to_string(&subscribe)?)).await?;

    println!("Connected! Listening for updates...");
    println!("(Press Ctrl+C to exit)");

    // Simple message loop
    while let Some(msg) = read.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                let server_msg: ServerMessage = serde_json::from_str(&text)?;
                match server_msg {
                    ServerMessage::Identified { user_id } => {
                        println!("✓ Identified as {}", user_id);
                    }
                    ServerMessage::Subscribed { doc_id } => {
                        println!("✓ Subscribed to {}", doc_id);
                    }
                    ServerMessage::YrsUpdate { from, doc_id, data, .. } => {
                        println!("← Update from {} for {} ({} bytes)", from, doc_id, data.len());
                    }
                    ServerMessage::Error { message } => {
                        eprintln!("✗ Error: {}", message);
                    }
                    _ => {
                        println!("← {:?}", server_msg);
                    }
                }
            }
            Ok(Message::Close(_)) => {
                println!("Connection closed");
                break;
            }
            Err(e) => {
                eprintln!("Error: {}", e);
                break;
            }
            _ => {}
        }
    }

    Ok(())
}
```

**Step 2: Update main.rs to use async**

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(EnvFilter::from_default_env()).init();

    let cli = Cli::parse();

    match cli.command {
        // ... existing commands unchanged ...
        Commands::Connect { relay_url, user, doc } => {
            collab_cli::commands::connect(&relay_url, &user, &doc, None).await?;
        }
        // ... rest unchanged ...
    }

    Ok(())
}
```

**Step 3: Add tokio dependency to collab-cli**

In `crates/collab-cli/Cargo.toml`:

```toml
[dependencies]
tokio = { workspace = true, features = ["rt-multi-thread", "macros"] }
tokio-tungstenite.workspace = true
futures.workspace = true
```

**Step 4: Test compilation**

```bash
cargo build -p collab-cli
```

**Step 5: Commit**

```bash
git add crates/collab-cli/
git commit -m "feat(cli): implement connect command for relay communication"
```

---

#### Task 8: CLI Interactive Mode (Optional Enhancement)

**Files:**
- Modify: `crates/collab-cli/src/commands.rs`

**Step 1: Add simple stdin reading for sending**

```rust
use std::io::{self, BufRead};

/// Interactive connect that can send messages.
pub async fn connect_interactive(
    relay_url: &str,
    user_id: &str,
    doc_id: &str,
) -> anyhow::Result<()> {
    // ... setup same as connect() ...

    let (tx, mut rx) = mpsc::channel::<String>(100);

    // Spawn stdin reader
    let tx_clone = tx.clone();
    std::thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            if let Ok(line) = line {
                if tx_clone.blocking_send(line).is_err() {
                    break;
                }
            }
        }
    });

    // Main loop handles both stdin and websocket
    loop {
        tokio::select! {
            Some(input) = rx.recv() => {
                // Send as YrsUpdate (simplified - real impl would encrypt)
                println!("→ Sending: {}", input);
            }
            Some(msg) = read.next() => {
                // Handle incoming messages
            }
        }
    }
}
```

**Step 2: Commit**

```bash
git add crates/collab-cli/
git commit -m "feat(cli): add interactive mode for connect command"
```

---

## PHASE 3C: Integration & Demo (Sequential)

### Task 11: Merge Worktrees

**Step 1: Ensure all worktrees have clean state**

```bash
cd /Users/rc/Projects/workspace/obsidian-ee-e2e
git status
git push origin feature/e2e-tests

cd /Users/rc/Projects/workspace/obsidian-ee-cli
git status
git push origin feature/cli-connect
```

**Step 2: Return to main and merge**

```bash
cd /Users/rc/Projects/workspace/obsidian-ee
git checkout main

# Merge E2E tests
git merge feature/e2e-tests --no-ff -m "feat: merge E2E test suite"

# Merge CLI enhancements
git merge feature/cli-connect --no-ff -m "feat: merge CLI connect command"
```

**Step 3: Resolve any conflicts and commit**

```bash
cargo test --workspace
cargo lint
git add .
git commit -m "chore: resolve merge conflicts"
```

---

### Task 12: Full CI Verification

**Step 1: Start Docker environment**

```bash
docker compose -f docker/docker-compose.yml up -d
docker compose -f docker/docker-compose.yml ps
```

**Step 2: Wait for healthy services**

```bash
./scripts/e2e-test.sh
```

**Step 3: Run full test suite including ignored tests**

```bash
cargo test --workspace
RELAY_URL=ws://localhost:8080 cargo test -p e2e-tests -- --ignored
```

**Step 4: Stop Docker**

```bash
docker compose -f docker/docker-compose.yml down
```

---

### Task 13: Record Asciinema Demo

**Files:**
- Create: `scripts/demo-scenario.sh`
- Create: `scripts/record-demo.sh`

**Step 1: Create demo scenario script**

```bash
#!/bin/bash
# scripts/demo-scenario.sh
set -e

echo "=== Obsidian E2E Collaborative Editing Demo ==="
echo ""

# Clean up any previous demo files
rm -f /tmp/demo-*.json

echo "Step 1: Alice creates a new encrypted document"
echo "$ cargo run -p collab-cli -- init demo-doc --user alice"
cargo run -p collab-cli -- init demo-doc --user alice
echo ""

echo "Step 2: Bob generates a key package to join"
echo "$ cargo run -p collab-cli -- keygen --user bob --output /tmp/demo-bob-key.json"
cargo run -p collab-cli -- keygen --user bob --output /tmp/demo-bob-key.json
echo ""

echo "Step 3: Alice creates an invite for Bob"
echo "$ cargo run -p collab-cli -- invite demo-doc --user alice --keypackage /tmp/demo-bob-key.json --output /tmp/demo-invite.json"
cargo run -p collab-cli -- invite demo-doc --user alice --keypackage /tmp/demo-bob-key.json --output /tmp/demo-invite.json
echo ""

echo "Step 4: Bob joins using the invite"
echo "$ cargo run -p collab-cli -- join /tmp/demo-invite.json --user bob"
cargo run -p collab-cli -- join /tmp/demo-invite.json --user bob
echo ""

echo "Step 5: Running the full in-memory demo"
echo "$ cargo run -p collab-cli -- demo"
cargo run -p collab-cli -- demo
echo ""

echo "=== Demo Complete! ==="
echo "The demo shows:"
echo "  ✓ MLS key exchange between Alice and Bob"
echo "  ✓ End-to-end encrypted document collaboration"
echo "  ✓ CRDT-based conflict resolution"
```

**Step 2: Create recording script**

```bash
#!/bin/bash
# scripts/record-demo.sh
set -e

# Check for asciinema
if ! command -v asciinema &> /dev/null; then
    echo "Installing asciinema..."
    pip install asciinema
fi

echo "Recording demo..."
asciinema rec demo.cast \
    --title "Obsidian E2E Collaborative Editing" \
    --command "bash scripts/demo-scenario.sh" \
    --overwrite

echo "Demo recorded to demo.cast"
echo ""
echo "To play: asciinema play demo.cast"
echo "To upload: asciinema upload demo.cast"
echo ""
echo "To convert to GIF (requires agg):"
echo "  agg demo.cast demo.gif --cols 100 --rows 30"
```

**Step 3: Make scripts executable and test**

```bash
chmod +x scripts/demo-scenario.sh scripts/record-demo.sh
./scripts/demo-scenario.sh
```

**Step 4: Record the demo**

```bash
./scripts/record-demo.sh
```

**Step 5: Commit**

```bash
git add scripts/demo-scenario.sh scripts/record-demo.sh demo.cast
git commit -m "feat: add asciinema demo recording"
```

---

### Task 14: Create PR

**Step 1: Push all changes**

```bash
git push origin main
```

**Step 2: Create PR if on feature branch**

```bash
gh pr create \
    --title "feat: Complete Phase 3 - E2E tests, CLI, and demo recording" \
    --body "$(cat <<'EOF'
## Summary

- Fixed CLI base64 compilation errors
- Added full E2E test suite with Docker Compose
- Implemented CLI connect command
- Added asciinema demo recording

## Test Plan

- [x] `cargo test --workspace` passes
- [x] `cargo lint` passes
- [x] E2E tests pass with Docker
- [x] Demo recording works

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

## Execution Summary

| Phase | Tasks | Parallelizable | Estimated Steps |
|-------|-------|----------------|-----------------|
| 3A | T1-T3 | No (sequential) | 9 |
| 3B | T4-T10 | Yes (3 worktrees) | 18 |
| 3C | T11-T14 | No (sequential) | 12 |

**Total Steps:** ~39 atomic steps

**Parallel Execution Plan:**

```
Main Session:
  → T1-T3 (fix CLI, verify)
  → Create worktrees
  → Dispatch 2 parallel agents:
      Agent 1: T4-T6 (E2E tests)
      Agent 2: T7-T8 (CLI connect)
  → Wait for completion
  → T11-T14 (merge, verify, record, PR)
```
