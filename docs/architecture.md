# Architecture Overview

Obsidian E2E is an end-to-end encrypted collaborative document editing system. It combines **Yrs CRDT** for conflict-free real-time editing with **MLS (RFC 9420)** for group encryption, routed through a **zero-knowledge WebSocket relay**.

## System Context

```mermaid
graph TD
    A1[Obsidian App<br/>Plugin + WASM<br/>CollabCore / CollabClient] -->|Encrypted WebSocket wss://| R
    A2[Obsidian App<br/>Plugin + WASM<br/>CollabCore / CollabClient] -->|Encrypted WebSocket wss://| R
    R[WebSocket Relay Server<br/>Zero-Knowledge: cannot read content] --> D[(DynamoDB<br/>Offline Queue)]
    R --> Redis[(Redis<br/>Presence)]
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

```mermaid
flowchart TD
    A["Alice types 'Hello'"] --> B["1. CollabDocument (Yrs CRDT)<br/>Text inserted via Yrs transaction<br/>State vector updated<br/>Incremental update encoded (V1)"]
    B --> C["2. EncryptedDocument (MLS Layer)<br/>Yrs update encrypted with MLS group key<br/>Produces EncryptedOp { ciphertext, epoch }"]
    C --> D["3. WebSocket Transport<br/>Serialized as ClientMessage::YrsUpdate (JSON)<br/>Contains: doc_id, encrypted, epoch, signature"]
    D --> E["4. Relay Server<br/>Deserializes header (doc_id, from)<br/>Forwards to subscribers EXCEPT sender<br/>Never inspects encrypted payload"]
    E --> F["5. Bob's Client Receives ServerMessage::YrsUpdate<br/>MLS decryption with group key<br/>Yrs update applied to local document<br/>CRDT conflict resolution (automatic)"]
    F --> G["Both Alice and Bob have identical document state"]
```

### MLS Group Formation

```mermaid
sequenceDiagram
    participant Alice
    participant Bob

    Note over Alice: 1. MlsDocumentGroup::create("alice")<br/>MLS group with single member<br/>Ciphersuite: X25519 + AES-128-GCM + SHA-256 + Ed25519

    Note over Bob: 2. PendingMember::new("bob")<br/>Key package containing Bob's public keys
    Bob->>Alice: key_package_bytes

    Note over Alice: 3. alice_group.add_member(bob_key_package)<br/>→ commit + welcome<br/>→ Epoch incremented
    Alice->>Bob: welcome_bytes
    Alice->>Alice: commit_bytes (for self)

    Note over Bob: 4. bob_pending.join(welcome_bytes)<br/>Bob now has MLS group state

    Note over Alice: 5. alice_group.process_commit(commit_bytes)<br/>Both at same epoch<br/>Bidirectional encryption works
```

## Layer Architecture

```mermaid
block-beta
    columns 1
    A["Obsidian Plugin (TypeScript)\nmain.ts → CollabClient → EditorSync"]
    B["WASM Bridge (collab-wasm)\nCollabCore: Yrs CRDT + AES-256-GCM"]
    C["Protocol (collab-proto)\nClientMessage | ServerMessage | MlsMessageType"]
    D["Core Library (collab-core)\nCollabDocument | MlsDocumentGroup | Registry\nEncryptedDocument | ConnectionStateMachine"]
    E["Relay Server (collab-relay)\nRelayServer | MessageRouter | OfflineQueue"]
    F["Infrastructure\nDynamoDB | Redis | Docker | AWS CDK"]

    A --> B --> C --> D
    D --> E --> F
```

## Module Dependency Graph

```mermaid
graph BT
    proto[collab-proto<br/>shared types]
    core[collab-core] --> proto
    relay[collab-relay] --> proto
    cli[collab-cli] --> core
    relay ~~~ cli

    wasm[collab-wasm<br/>independent, yrs + aes-gcm]
    plugin[plugins/obsidian-ee<br/>TypeScript] --> wasm

    watcher[collab-watcher<br/>independent, file system events]
```

Key design decisions:
- `collab-proto` has zero business logic; it's a pure type definition crate
- `collab-core` and `collab-relay` depend on `collab-proto` but not on each other
- `collab-wasm` uses a simplified encryption model (AES-256-GCM) as an MVP, with MLS planned for future integration
- `collab-watcher` is fully independent and communicates via async channels

## Connection State Machine

The `ConnectionStateMachine` in `collab-core` manages WebSocket lifecycle:

```mermaid
stateDiagram-v2
    [*] --> Disconnected
    Disconnected --> Connecting : connect / auto_connect
    Connecting --> Connected : on_connected
    Connecting --> Reconnecting : on_error
    Connecting --> Failed : on_error (max retries)
    Connected --> Reconnecting : on_disconnected
    Connected --> Failed : on_disconnected (max retries)
    Reconnecting --> Connecting : on_retry_tick
    Reconnecting --> Failed : max_retries exceeded
    Failed --> [*]
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
