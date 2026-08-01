# Issue #28 — Plugin MLS rewire: reference implementation

Date: 2026-07-30
Branch: `feat/28-remove-aes-mls-only`
Status: implementation spec (transcribe this; no design freedom needed)

This is the exact target for the plugin rewire. The AES `CollabCore` is already
deleted from the WASM (Phase A); the WASM now exposes ONLY the MLS surface
(`WasmEncryptedDocument`, `WasmInvite`, `WasmPendingMember`, `WasmEncryptedOp`,
`generate_key_package`). The relay wire already routes `mls_handshake` frames
(`KeyPackage`/`Welcome`/`Commit`/`Application`) — proven by
`src/__tests__/two-user-mls-integration.test.ts`. Adopt that exact choreography.

## Exact WASM signatures (from collab_wasm.d.ts)
- `WasmEncryptedDocument.create(doc_id: string, user_id: string): WasmEncryptedDocument`
- `WasmEncryptedDocument.join(invite: WasmInvite, pending: WasmPendingMember): WasmEncryptedDocument` (consumes `pending`)
- `doc.create_invite(key_package: Uint8Array): WasmInvite`
- `doc.get_encrypted_update(): WasmEncryptedOp` → `{ ciphertext: Uint8Array, epoch: bigint }`
- `doc.apply_encrypted_update(ciphertext: Uint8Array, epoch: bigint): void`
- `doc.insert(index, text)`, `doc.delete(index, len)`, `doc.get_content(): string`, `doc.free()`, `readonly epoch: bigint`
- `WasmInvite.from_welcome(doc_id: string, welcome: Uint8Array): WasmInvite`, getter `.welcome: Uint8Array`
- `WasmPendingMember` getter `.key_package: Uint8Array`, `.free()`
- `generate_key_package(user_id: string): WasmPendingMember`

## Target `collab-client.ts` — crypto-relevant parts (preserve reconnect/queue logic VERBATIM)

Keep VERBATIM (hard-won regression fixes — do NOT touch): `connect()` and its
onopen/onerror/onclose settle-exactly-once logic, `handleReconnect()`,
`send()`/`flushMessageQueue()`/queue eviction, `applyTextDiff()`,
`getConnectionState()`, `disconnect()`'s reconnect-disable, the callback setters.

CHANGE these:

### Imports + config
```ts
import {
    WasmEncryptedDocument,
    WasmInvite,
    generate_key_package,
    type WasmPendingMember,
} from './wasm/collab_wasm';

export type CollabRole = 'owner' | 'joiner';

export interface CollabClientConfig {
    relayUrl: string;
    userId: string;
    docId: string;
    role: CollabRole; // owner creates the MLS group; joiner joins via a Welcome
}
```
Remove `encryptionKey` and the `import { CollabCore }`.

### validateConfig — drop key checks, validate role
```ts
function validateConfig(config: CollabClientConfig): void {
    // ... keep relayUrl / userId / docId checks verbatim ...
    if (config.role !== 'owner' && config.role !== 'joiner') {
        throw new ConfigValidationError("role must be 'owner' or 'joiner'");
    }
}
```
Delete the three `encryptionKey` checks (Uint8Array / 32-byte / all-zeros).

### Fields + constructor
```ts
private doc: WasmEncryptedDocument | null = null;
private pending: WasmPendingMember | null = null;
// ...existing ws/reconnect/queue fields unchanged...

constructor(config: CollabClientConfig) {
    validateConfig(config);
    this.config = config;
    // Owner's group exists independently of the network; create it up front.
    // Joiner has no group until a Welcome arrives — it publishes a key package.
    if (config.role === 'owner') {
        this.doc = WasmEncryptedDocument.create(config.docId, config.userId);
    } else {
        this.pending = generate_key_package(config.userId);
    }
}
```
(Drop the old `collabCore` constructor param and `set_encryption_key` call.)

### handleMessage — add mls_handshake; keep subscribed/error/default
In the `subscribed` case, a JOINER publishes its key package (it is now
registered, so the relay will fan the frame out to the owner):
```ts
case 'subscribed':
    if (this.config.role === 'joiner' && this.pending) {
        this.send({
            type: 'mls_handshake',
            doc_id: this.config.docId,
            payload: [...this.pending.key_package],
            message_type: 'key_package',
        });
    }
    break;
case 'mls_handshake':
    this.handleMlsHandshake(message);
    break;
```

### New handleMlsHandshake
```ts
private handleMlsHandshake(message: {
    message_type?: string;
    payload?: number[];
    doc_id?: string;
}): void {
    // Untrusted relay: reject a frame routed for a different document early.
    if (message.doc_id !== undefined && message.doc_id !== this.config.docId) {
        return;
    }
    const payload = new Uint8Array(message.payload ?? []);
    try {
        if (message.message_type === 'key_package' && this.config.role === 'owner' && this.doc) {
            // Owner invites the member and ships back the Welcome.
            const invite = this.doc.create_invite(payload);
            this.send({
                type: 'mls_handshake',
                doc_id: this.config.docId,
                payload: [...invite.welcome],
                message_type: 'welcome',
            });
        } else if (message.message_type === 'welcome' && this.config.role === 'joiner' && this.pending) {
            // Joiner opens the Welcome with its LOCAL docId (never the frame's).
            const invite = WasmInvite.from_welcome(this.config.docId, payload);
            this.doc = WasmEncryptedDocument.join(invite, this.pending);
            this.pending = null; // consumed by join
        }
        // 'commit'/'application' unhandled in the 2-party first cut (documented residual).
    } catch (error) {
        if (this.onErrorCallback) {
            this.onErrorCallback({
                type: 'sync',
                message: extractErrorMessage(error),
                docId: this.config.docId,
                originalError: error instanceof Error ? error : undefined,
            });
        }
    }
}
```

### handleYrsUpdate — route through MLS; fail-closed if no group
```ts
private handleYrsUpdate(message: YrsUpdateMessage): void {
    if (this.doc === null) {
        return; // no group yet: cannot decrypt (fail-closed, no plaintext path)
    }
    try {
        if (!message.encrypted || !Array.isArray(message.encrypted)) {
            throw new Error('Invalid yrs_update message: missing or invalid encrypted field');
        }
        if (message.doc_id !== undefined && message.doc_id !== this.config.docId) {
            throw new Error(
                `yrs_update doc_id mismatch: expected ${this.config.docId}, got ${message.doc_id}`
            );
        }
        const encrypted = new Uint8Array(message.encrypted);
        // MLS binds each message to the group via its internal GroupContext; a
        // ciphertext from a different group fails authentication here (proven by
        // two-user-mls-integration.test.ts cross-group test). No docId-AAD needed.
        this.doc.apply_encrypted_update(encrypted, BigInt(message.epoch ?? 0));
        if (this.onUpdateCallback) {
            this.onUpdateCallback(this.doc.get_content());
        }
    } catch (error) {
        // ... keep existing decryption-error callback ...
    }
}
```
`YrsUpdateMessage` keeps `epoch?: number` (Number on the wire; converted to BigInt here).

### sendUpdate — fail-closed if no group
```ts
sendUpdate(text: string): boolean {
    if (this.doc === null) {
        return false; // fail-closed: never encrypt-to-nobody, never a plaintext frame
    }
    try {
        const currentText = this.doc.get_content();
        if (text !== currentText) {
            this.applyTextDiff(currentText, text);
        }
        const op = this.doc.get_encrypted_update();
        return this.send({
            type: 'yrs_update',
            doc_id: this.config.docId,
            encrypted: [...op.ciphertext],
            epoch: Number(op.epoch),
        });
    } catch (error) {
        // ... keep existing sync-error callback, return false ...
    }
}
```
`applyTextDiff` calls `this.doc.delete(...)` / `this.doc.insert(...)` (guard: only
called from sendUpdate where doc is non-null).

### getText + disconnect dispose
```ts
getText(): string {
    return this.doc?.get_content() ?? '';
}

disconnect(): void {
    // ... keep existing reconnect-disable + ws close verbatim ...
    this.doc?.free();
    this.doc = null;
    this.pending?.free();
    this.pending = null;
}
```

## Target `main.ts` changes
- Drop `import init, { CollabCore }` → `import init from './wasm/collab_wasm';` (the
  MLS classes are used inside CollabClient, not main.ts).
- Remove `encryptionKey` from `CollabPluginSettings` + `DEFAULT_SETTINGS`, and
  `decodeBase64Key`/`encodeBase64Key`.
- Add `role: CollabRole` to settings? NO — simpler: keep it out of persisted
  settings; instead expose TWO commands: `start-collab-owner` ("Start Collaboration
  (create group)") and `start-collab-join` ("Join Collaboration"), each calling
  `startSession(role)`.
- `startSession(role: CollabRole)`: remove the `collabCore` / encryptionKey guard
  block entirely. Build config `{ relayUrl, userId: user-${Date.now()}, docId:
  activeView.file.path, role }`. Construct `new CollabClient(config)` (no core arg).
  Keep the `connect()` → bind editor ordering: do NOT bind the editor until
  `connect()` resolves (fail-closed: no editor binding without a live session).
- Remove the `collabCore` field, `initWasm`'s `new CollabCore()` calls, and the
  `collabCore.free()` in stopSession/onunload — the client owns doc lifetime now
  and frees it in `disconnect()`. `initWasm` still must `await init(wasmModule)`
  once (the MLS classes need the module initialized before `CollabClient`'s
  constructor calls `WasmEncryptedDocument.create`/`generate_key_package`).
  Keep the double-start guard (`if (this.collabClient || this.editorSync)`).
- Settings UI (`CollabSettingTab`): remove the Encryption Key text field +
  "Generate random key" button. Keep the Relay URL field. (Role is chosen via the
  two commands, not settings.)

## `editor-sync.ts`
No API change needed (it uses `client.getText()`/`sendUpdate()`/`onUpdate()` only).
Touch ONLY if the constructor/API you changed forces it.

## `crates/collab-wasm/src/mls.rs:4`
One-line: the doc comment "unlike the AES `CollabCore` path" references a deleted
type. Reword to e.g. "unlike the removed AES path". No behavior change.

## Fail-closed invariant (CLAUDE.md) — where it lives + test
- No key input exists → a placeholder/all-zeros key is impossible by construction.
- `sendUpdate` returns false + emits NO frame when `doc === null`.
- `handleYrsUpdate` returns early when `doc === null` (no decrypt attempt).
- Decryption gated by MLS membership (cross-group test already proves it).
Test: assert `sendUpdate('x')` before a group is established returns false and the
websocket sent nothing.
