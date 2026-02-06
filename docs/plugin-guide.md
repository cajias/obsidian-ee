# Obsidian Plugin Guide

The `plugins/obsidian-ee/` directory contains the TypeScript Obsidian plugin that provides collaborative editing inside the Obsidian desktop app.

## Architecture

```
+------------------------------------------+
|            Obsidian Desktop App           |
|                                          |
|  +------------------------------------+  |
|  |         CollabPlugin (main.ts)     |  |
|  |  - WASM initialization             |  |
|  |  - Session lifecycle management     |  |
|  |  - Settings UI                      |  |
|  +--------+---------------------------+  |
|           |                              |
|  +--------v---------------------------+  |
|  |     EditorSync (editor-sync.ts)    |  |
|  |  - Binds to Obsidian MarkdownView  |  |
|  |  - Debounces local changes (100ms) |  |
|  |  - Applies remote updates          |  |
|  |  - Preserves cursor position       |  |
|  +--------+---------------------------+  |
|           |                              |
|  +--------v---------------------------+  |
|  |    CollabClient (collab-client.ts) |  |
|  |  - WebSocket connection            |  |
|  |  - Reconnection with backoff       |  |
|  |  - Message queue (1000 max)        |  |
|  |  - Text diff algorithm             |  |
|  +--------+---------------------------+  |
|           |                              |
|  +--------v---------------------------+  |
|  |     CollabCore (WASM module)       |  |
|  |  - Yrs CRDT operations            |  |
|  |  - AES-256-GCM encryption         |  |
|  +------------------------------------+  |
+------------------------------------------+
          |
          | wss:// WebSocket
          |
    +-----v-----+
    |   Relay    |
    |   Server   |
    +-----------+
```

## Components

### CollabPlugin (`main.ts`)

The plugin entry point. Responsibilities:
- **WASM Initialization**: Loads `collab_wasm_bg.wasm` from the plugin directory, compiles and initializes it
- **Session Management**: Provides Start/Stop Collaboration commands
- **Settings**: Configurable relay server URL via settings tab
- **Resource Cleanup**: Properly frees WASM memory on session stop and plugin unload

Session lifecycle:
1. `onload()` - Load settings, initialize WASM, register commands
2. `startSession()` - Create CollabClient + EditorSync, connect to relay
3. `stopSession()` - Unbind editor, disconnect WebSocket, free WASM resources
4. `onunload()` - Stop session, free remaining resources

### CollabClient (`collab-client.ts`)

WebSocket client with reconnection logic.

**Connection states**: `disconnected` -> `connecting` -> `connected` -> `reconnecting`

**Features:**
- Automatic reconnection with exponential backoff (1s, 2s, 4s, 8s, 16s)
- Maximum 5 reconnect attempts before giving up
- Message queue (max 1000) for messages sent while disconnected
- FIFO eviction when queue is full
- Text diff algorithm for minimal CRDT operations
- Config validation at construction time

**Message handling:**
- Sends `identify` and `subscribe` on connection
- Receives `yrs_update` messages and decrypts via WASM
- Calls update callback with new text content
- Error and disconnect callbacks for the plugin to handle

**Text Diff Algorithm** (`applyTextDiff`):
Instead of clearing and reinserting the entire document on each change, the client computes the minimal diff:
1. Find common prefix length
2. Find common suffix length
3. Delete the changed range
4. Insert the new content

This preserves CRDT identity for unchanged characters, which is critical for correct collaborative behavior.

### EditorSync (`editor-sync.ts`)

Bridges the CollabClient with Obsidian's editor API.

**Features:**
- Binds to a `MarkdownView`'s editor instance
- Debounces local changes (100ms) to avoid flooding the relay
- Applies remote updates while preserving cursor position
- Uses `isApplyingRemote` flag to prevent echo (remote update -> local change event -> send update)
- Flushes pending changes on unbind to prevent data loss

**Cursor preservation:**
When a remote update changes the document, the editor:
1. Saves the current cursor position
2. Applies the new text via `setValue()`
3. Restores the cursor, clamped to valid line/column range

### WASM Error Handling

WASM errors from Rust are plain JavaScript objects (not `Error` instances):

```javascript
{ type: "encryption", message: "No encryption key set" }
```

The `extractErrorMessage()` function in `collab-client.ts` and `wrapError()` in `editor-sync.ts` handle these gracefully, preventing `[object Object]` from appearing in error messages.

## Settings

| Setting | Default | Description |
|---------|---------|-------------|
| Relay Server URL | `ws://localhost:8080` | WebSocket URL. Use `wss://` in production. |

## Security Notes

1. **Placeholder Key**: The current implementation uses an all-zeros encryption key (`new Uint8Array(32)`). This is insecure and only for development.
2. **Transport Security**: Production deployments must use `wss://` (TLS-encrypted WebSocket).
3. **Key Management**: Production must generate keys via `crypto.getRandomValues()` and exchange them securely (planned: MLS key exchange via WASM).

## Testing

```bash
cd plugins/obsidian-ee

# Unit tests
npm test

# E2E tests (requires Playwright)
npx playwright test
```

### Test Files

| Test File | Coverage |
|-----------|----------|
| `main.test.ts` | Plugin lifecycle, WASM initialization |
| `collab-client.test.ts` | WebSocket client, reconnection, message queue |
| `editor-sync.test.ts` | Editor binding, remote updates, cursor preservation |
| `encryption-integration.test.ts` | WASM encryption roundtrip |
| `wasm-integration.test.ts` | WASM CRDT operations |
| `two-user-integration.test.ts` | Multi-user collaboration |

### E2E Tests

| Test File | Description |
|-----------|-------------|
| `two-user-sync.spec.ts` | Full Playwright test with mock relay |
| `mock-relay.ts` | WebSocket mock for testing without real server |

## Build

```bash
# Build the WASM module first
./scripts/build-wasm.sh

# Then build the plugin
cd plugins/obsidian-ee
npm run build
```

The plugin build uses esbuild (`esbuild.config.mjs`) to bundle TypeScript into a single `main.js` file for Obsidian.

## Plugin Manifest

From `manifest.json`:
- **ID**: `obsidian-ee`
- **Name**: Obsidian E2E Collaboration
- **Min Obsidian Version**: 1.5.0
