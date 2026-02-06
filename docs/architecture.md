# Architecture Overview

Obsidian E2E is an end-to-end encrypted collaborative document editing system. It combines **Yrs CRDT** for conflict-free real-time editing with **MLS (RFC 9420)** for group encryption, routed through a **zero-knowledge WebSocket relay**.

## System Context

```
+-------------------+          +-------------------+
|   Obsidian App    |          |   Obsidian App    |
|  (Plugin + WASM)  |          |  (Plugin + WASM)  |
|                   |          |                   |
|  CollabCore(WASM) |          |  CollabCore(WASM) |
|  CollabClient(TS) |          |  CollabClient(TS) |
+--------+----------+          +--------+----------+
         |                              |
         |  Encrypted WebSocket (wss://)
         |                              |
    +----v------------------------------v----+
    |         WebSocket Relay Server         |
    |  (Zero-Knowledge: cannot read content) |
    +---------+-------------------+----------+
              |                   |
     +--------v--------+  +------v--------+
     |    DynamoDB      |  |    Redis      |
     | (Offline Queue)  |  |  (Presence)   |
     +-----------------+  +---------------+
```

## Core Principles

1. **Zero-Knowledge Relay**: The relay server routes encrypted messages without access to plaintext content, encryption keys, or document state.
2. **CRDT Convergence**: All replicas eventually converge to identical content regardless of message ordering, using Yrs conflict-free replicated data types.
3. **Forward Secrecy**: MLS epoch-based key ratcheting ensures past messages remain secure even if current keys are compromised.
4. **Minimal Trust**: Clients perform all encryption/decryption locally. The server is untrusted infrastructure.

## Workspace Crates

| Crate | Role | Key Dependencies |
|-------|------|-----------------|
| `collab-core` | CRDT engine + MLS encryption | `yrs`, `openmls` |
| `collab-relay` | WebSocket relay server | `tokio`, `tokio-tungstenite` |
| `collab-proto` | Protocol message types | `serde`, `serde_json` |
| `collab-cli` | Reference CLI client | `clap`, `collab-core` |
| `collab-wasm` | WASM bindings for browser | `wasm-bindgen`, `yrs`, `aes-gcm` |
| `collab-watcher` | File system watcher | `notify`, `tokio` |
| `e2e-tests` | Integration test suite | All crates |

Additionally:
- `xtask` - Development task runner (`cargo xtask lint`, `cargo xtask e2e`)
- `plugins/obsidian-ee` - TypeScript Obsidian plugin

## Data Flow

### Collaborative Edit (Happy Path)

```
Alice types "Hello"
    |
    v
1. CollabDocument (Yrs CRDT)
   - Text inserted at position via Yrs transaction
   - State vector updated
   - Incremental update encoded (Yrs V1 format)
    |
    v
2. EncryptedDocument (MLS Layer)
   - Yrs update bytes encrypted with MLS group key
   - Produces EncryptedOp { ciphertext, epoch }
    |
    v
3. WebSocket Transport
   - Serialized as ClientMessage::YrsUpdate (JSON)
   - Contains: doc_id, encrypted (opaque bytes), epoch, signature
    |
    v
4. Relay Server
   - Deserializes message header (doc_id, from)
   - Looks up subscribers for doc_id
   - Forwards to all subscribers EXCEPT sender
   - Never inspects encrypted payload
    |
    v
5. Bob's Client Receives ServerMessage::YrsUpdate
   - MLS decryption with group key
   - Yrs update applied to local document
   - CRDT conflict resolution (automatic)
   - Editor updated with new content
    |
    v
Both Alice and Bob have identical document state
```

### MLS Group Formation

```
1. Alice creates document
   MlsDocumentGroup::create("alice")
   -> MLS group with single member
   -> Ciphersuite: X25519 + AES-128-GCM + SHA-256 + Ed25519

2. Bob generates key package
   PendingMember::new("bob")
   -> Key package containing Bob's public keys

3. Alice invites Bob
   alice_group.add_member(bob_key_package)
   -> commit (for existing members to process)
   -> welcome (for Bob to join)
   -> Epoch incremented

4. Bob joins
   bob_pending.join(welcome_bytes)
   -> Bob now has MLS group state
   -> Can encrypt/decrypt messages

5. Alice processes commit
   alice_group.process_commit(commit_bytes)
   -> Both at same epoch
   -> Bidirectional encryption now works
```

## Layer Architecture

```
+--------------------------------------------------+
|              Obsidian Plugin (TypeScript)          |
|  main.ts -> CollabClient -> EditorSync            |
+--------------------------------------------------+
|              WASM Bridge (collab-wasm)             |
|  CollabCore: Yrs CRDT + AES-256-GCM              |
+--------------------------------------------------+
|              Protocol (collab-proto)               |
|  ClientMessage | ServerMessage | MlsMessageType   |
+--------------------------------------------------+
|              Core Library (collab-core)            |
|  CollabDocument | MlsDocumentGroup | Registry     |
|  EncryptedDocument | ConnectionStateMachine       |
+--------------------------------------------------+
|              Relay Server (collab-relay)           |
|  RelayServer | MessageRouter | OfflineQueue       |
+--------------------------------------------------+
|              Infrastructure                        |
|  DynamoDB | Redis | Docker | AWS CDK              |
+--------------------------------------------------+
```

## Module Dependency Graph

```
collab-proto (shared types)
    ^           ^
    |           |
collab-core    collab-relay
    ^               ^
    |               |
collab-cli     (standalone server)

collab-wasm (independent, uses yrs + aes-gcm directly)
    ^
    |
plugins/obsidian-ee (TypeScript)

collab-watcher (independent, file system events)
```

Key design decisions:
- `collab-proto` has zero business logic; it's a pure type definition crate
- `collab-core` and `collab-relay` depend on `collab-proto` but not on each other
- `collab-wasm` uses a simplified encryption model (AES-256-GCM) as an MVP, with MLS planned for future integration
- `collab-watcher` is fully independent and communicates via async channels

## Connection State Machine

The `ConnectionStateMachine` in `collab-core` manages WebSocket lifecycle:

```
Disconnected ──(connect/auto_connect)──> Connecting
Connecting ──(on_connected)──> Connected
Connecting ──(on_error)──> Reconnecting | Failed
Connected ──(on_disconnected)──> Reconnecting | Failed
Reconnecting ──(on_retry_tick)──> Connecting
Reconnecting ──(max_retries)──> Failed
```

Retry policy: exponential backoff (1s, 2s, 4s, 8s, 16s) with 25% jitter, capped at 30s, max 5 retries.

The state machine is **synchronous and runtime-agnostic** - it emits `ConnectionAction` values that the caller executes, making it testable and portable across async runtimes.

## Document Registry

The `DocumentRegistry` manages multiple concurrent documents:

```rust
DocumentRegistry
└── documents: HashMap<DocumentId, DocumentEntry>
    └── DocumentEntry { CollabDocument, DocumentMetadata }
        └── DocumentMetadata { created_at, last_modified, custom: HashMap }
```

The registry manages `CollabDocument` instances with metadata tracking (creation time, last-modified time, custom key-value pairs). It supports create, get, close, and open (restore from serialized state) operations. Encrypted document support via `EncryptedDocument` integration is planned.
