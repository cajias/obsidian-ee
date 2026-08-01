/**
 * Two-client vault-sync integration (#32), MLS surface (post-#68).
 *
 * The manifest rides the same relay connection as the file doc but under its
 * OWN MLS group on MANIFEST_DOC_ID, established by the same owner/joiner
 * handshake. There is no shared key: doc-scoping and replay isolation come from
 * per-group separation.
 *
 * Positive paths: create / delete / rename propagate owner -> joiner and the
 * joiner subscribes to newly-announced paths.
 *
 * Negative paths (trust boundaries):
 *  - a yrs_update for a doc_id that is neither the file doc nor the manifest is
 *    still rejected (regression on the misroute guard),
 *  - a ciphertext from a FOREIGN MLS group injected on the manifest channel
 *    fails authentication in the manifest group (cross-group rejection); the
 *    manifest is left untouched and the client survives,
 *  - garbage plaintext that DOES decrypt under the manifest group surfaces an
 *    error without crashing.
 *
 * Uses the REAL compiled WASM (WasmEncryptedDocument + WasmVaultSync) and a real
 * WebSocketServer mock relay that fans out both mls_handshake and yrs_update.
 */

import { jest, describe, it, expect, beforeAll, afterAll, afterEach } from '@jest/globals';
import { WebSocket, WebSocketServer } from 'ws';
import {
    CollabClient,
    type CollabClientConfig,
    type CollabError,
    type CollabRole,
} from '../collab-client';
import type {
    WasmEncryptedDocument as WasmDocType,
    WasmVaultSync as WasmVaultSyncType,
    generate_key_package as GenKeyPackage,
} from '../wasm/collab_wasm';
import { loadRealWasm } from './helpers/load-real-wasm';

// Real compiled WASM constructors, captured after init in beforeAll.
let WasmEncryptedDocument!: typeof WasmDocType;
let WasmVaultSync!: typeof WasmVaultSyncType;
let generate_key_package!: typeof GenKeyPackage;
let MANIFEST_DOC_ID!: string;

const OriginalWebSocket = (global as any).WebSocket;

/** Browser-shaped WebSocket wrapper over `ws`, as in two-user-integration. */
class NodeWebSocket {
    private ws: WebSocket;
    onopen: (() => void) | null = null;
    onmessage: ((event: { data: string }) => void) | null = null;
    onclose: (() => void) | null = null;
    onerror: ((error: any) => void) | null = null;
    readyState = 0;

    constructor(url: string) {
        this.ws = new WebSocket(url);
        this.ws.on('open', () => {
            this.readyState = 1;
            this.onopen?.();
        });
        this.ws.on('message', (data: Buffer) => {
            this.onmessage?.({ data: data.toString() });
        });
        this.ws.on('close', () => {
            this.readyState = 3;
            this.onclose?.();
        });
        this.ws.on('error', (err: Error) => {
            this.onerror?.(err);
        });
    }

    send(data: string): void {
        if (this.ws.readyState === WebSocket.OPEN) {
            this.ws.send(data);
        }
    }

    close(): void {
        this.ws.close();
    }

    static get CONNECTING() {
        return 0;
    }
    static get OPEN() {
        return 1;
    }
    static get CLOSING() {
        return 2;
    }
    static get CLOSED() {
        return 3;
    }
}

(global as any).WebSocket = NodeWebSocket;

interface RecordedFrame {
    from: string | null;
    msg: any;
}

/**
 * Mock relay that fans out mls_handshake AND yrs_update to every other client
 * and RECORDS every inbound frame, so tests can assert which frames each client
 * sent and drive a real two-party MLS handshake.
 */
class RecordingMockRelay {
    private wss: WebSocketServer | null = null;
    private clients: Map<string, WebSocket> = new Map();
    frames: RecordedFrame[] = [];

    async start(port: number): Promise<void> {
        return new Promise((resolve, reject) => {
            this.wss = new WebSocketServer({ port });
            this.wss.on('connection', (ws) => {
                let clientId: string | null = null;
                ws.on('message', (data) => {
                    try {
                        const msg = JSON.parse(data.toString());
                        if (msg.type === 'identify') {
                            clientId = msg.user_id as string;
                            this.clients.set(clientId, ws);
                        }
                        this.frames.push({ from: clientId, msg });
                        if (msg.type === 'subscribe') {
                            ws.send(JSON.stringify({ type: 'subscribed', doc_id: msg.doc_id }));
                        } else if (msg.type === 'yrs_update' || msg.type === 'mls_handshake') {
                            this.clients.forEach((client, id) => {
                                if (id !== clientId && client.readyState === WebSocket.OPEN) {
                                    client.send(JSON.stringify({ ...msg, from: clientId }));
                                }
                            });
                        }
                    } catch (error) {
                        console.error('Relay failed to parse message:', error);
                    }
                });
                ws.on('close', () => {
                    if (clientId) {
                        this.clients.delete(clientId);
                    }
                });
            });
            this.wss.on('listening', () => resolve());
            this.wss.on('error', (err) => reject(err));
        });
    }

    framesFrom(userId: string): RecordedFrame[] {
        return this.frames.filter((f) => f.from === userId);
    }

    async stop(): Promise<void> {
        if (!this.wss) {
            return;
        }
        this.clients.forEach((client) => client.close());
        this.clients.clear();
        return new Promise((resolve) => {
            this.wss!.close(() => {
                this.wss = null;
                resolve();
            });
        });
    }
}

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

describe('Vault sync over its own MLS group (two clients)', () => {
    let relay: RecordingMockRelay;
    const RELAY_PORT = 8091;
    const RELAY_URL = `ws://localhost:${RELAY_PORT}`;
    const FILE_DOC = 'shared-doc.md';

    interface TestClient {
        vaultSync: InstanceType<typeof WasmVaultSyncType>;
        client: CollabClient;
        errors: CollabError[];
        manifestPaths: string[][];
    }

    function makeClient(userId: string, role: CollabRole): TestClient {
        const vaultSync = new WasmVaultSync([], [], true, true);
        const config: CollabClientConfig = {
            relayUrl: RELAY_URL,
            userId,
            docId: FILE_DOC,
            role,
            vaultSync,
            manifestDocId: MANIFEST_DOC_ID,
        };
        const client = new CollabClient(config);
        const errors: CollabError[] = [];
        const manifestPaths: string[][] = [];
        client.onError((e) => errors.push(e));
        client.onManifestPaths((paths) => {
            manifestPaths.push(paths);
        });
        return { vaultSync, client, errors, manifestPaths };
    }

    /**
     * A fully-established owner+joiner pair. The owner connects first (its groups
     * exist immediately); the joiner then connects, ships key packages, and the
     * relay fan-out drives both MLS handshakes (file + manifest) to completion.
     */
    async function connectedPair(tag: string): Promise<{ a: TestClient; b: TestClient }> {
        const a = makeClient(`alice-${tag}`, 'owner');
        const b = makeClient(`bob-${tag}`, 'joiner');
        await a.client.connect();
        await wait(50);
        await b.client.connect();
        await wait(400); // let both file+manifest handshakes settle
        return { a, b };
    }

    /** Raw attacker connection that can inject arbitrary frames. */
    async function connectMallory(): Promise<WebSocket> {
        const ws = new WebSocket(RELAY_URL);
        await new Promise<void>((resolve, reject) => {
            ws.on('open', () => resolve());
            ws.on('error', reject);
        });
        ws.send(JSON.stringify({ type: 'identify', user_id: 'mallory' }));
        await wait(50);
        return ws;
    }

    beforeAll(async () => {
        const wasm = await loadRealWasm();
        WasmEncryptedDocument = wasm.WasmEncryptedDocument;
        WasmVaultSync = wasm.WasmVaultSync;
        generate_key_package = wasm.generate_key_package;
        MANIFEST_DOC_ID = wasm.manifest_doc_id();
        relay = new RecordingMockRelay();
        await relay.start(RELAY_PORT);
    });

    afterAll(async () => {
        await relay.stop();
        if (OriginalWebSocket) {
            (global as any).WebSocket = OriginalWebSocket;
        }
    });

    // Every test that calls connectedPair() stashes its pair here so afterEach
    // can tear it down uniformly, instead of each `it` repeating the same
    // three-line disconnect/wait boilerplate.
    let pair: { a: TestClient; b: TestClient } | undefined;

    afterEach(async () => {
        if (!pair) {
            return;
        }
        pair.a.client.disconnect();
        pair.b.client.disconnect();
        pair = undefined;
        await wait(50);
    });

    it('both clients subscribe to the manifest doc on connect', async () => {
        pair = await connectedPair('sub');
        for (const user of ['alice-sub', 'bob-sub']) {
            expect(
                relay
                    .framesFrom(user)
                    .some((f) => f.msg.type === 'subscribe' && f.msg.doc_id === MANIFEST_DOC_ID)
            ).toBe(true);
        }
    });

    it('propagates a file creation: manifest applies, callback fires, receiver subscribes to the new path', async () => {
        const { a, b } = (pair = await connectedPair('create'));

        const action = a.vaultSync.handle_created('notes/x.md');
        expect(action.kind).toBe('created');
        expect(a.client.sendManifestUpdate(action.manifest_update)).toBe(true);
        await wait(250);

        expect(b.manifestPaths.flat()).toContain('notes/x.md');
        expect(b.vaultSync.list_files()).toContain('notes/x.md');
        expect(
            relay
                .framesFrom('bob-create')
                .some((f) => f.msg.type === 'subscribe' && f.msg.doc_id === 'notes/x.md')
        ).toBe(true);
    });

    it('propagates a deletion as a tombstone', async () => {
        const { a, b } = (pair = await connectedPair('del'));

        const created = a.vaultSync.handle_created('notes/y.md');
        a.client.sendManifestUpdate(created.manifest_update);
        await wait(250);
        expect(b.vaultSync.list_files()).toContain('notes/y.md');

        const deleted = a.vaultSync.handle_deleted('notes/y.md');
        expect(deleted.kind).toBe('deleted');
        a.client.sendManifestUpdate(deleted.manifest_update);
        await wait(250);
        expect(b.vaultSync.list_files()).not.toContain('notes/y.md');
    });

    it('propagates a rename and the receiver subscribes to the new path', async () => {
        const { a, b } = (pair = await connectedPair('ren'));

        const created = a.vaultSync.handle_created('notes/old.md');
        a.client.sendManifestUpdate(created.manifest_update);
        await wait(250);
        expect(b.vaultSync.list_files()).toContain('notes/old.md');

        const renamed = a.vaultSync.handle_renamed('notes/old.md', 'notes/new.md');
        expect(renamed.kind).toBe('renamed');
        expect(renamed.new_path).toBe('notes/new.md');
        a.client.sendManifestUpdate(renamed.manifest_update);
        await wait(250);

        expect(b.vaultSync.list_files()).toContain('notes/new.md');
        expect(b.vaultSync.list_files()).not.toContain('notes/old.md');
        expect(
            relay
                .framesFrom('bob-ren')
                .some((f) => f.msg.type === 'subscribe' && f.msg.doc_id === 'notes/new.md')
        ).toBe(true);
    });

    it('sendManifestUpdate carries the manifest group real epoch, not 0', async () => {
        const { a } = (pair = await connectedPair('epoch'));
        const action = a.vaultSync.handle_created('notes/e.md');
        a.client.sendManifestUpdate(action.manifest_update);
        await wait(250);

        // The owner added the joiner to the manifest group, so its epoch advanced
        // past 0; the frame the relay recorded must reflect that real epoch.
        const update = relay
            .framesFrom('alice-epoch')
            .find((f) => f.msg.type === 'yrs_update' && f.msg.doc_id === MANIFEST_DOC_ID);
        expect(update).toBeDefined();
        expect(update!.msg.epoch).toBeGreaterThan(0);
    });

    it('REGRESSION: still rejects a yrs_update whose doc_id is neither the file doc nor the manifest', async () => {
        const consoleErrorSpy = jest.spyOn(console, 'error').mockImplementation(() => {});
        const { b } = (pair = await connectedPair('guard'));

        const mallory = await connectMallory();
        mallory.send(
            JSON.stringify({
                type: 'yrs_update',
                doc_id: 'other-doc',
                encrypted: [1, 2, 3, 4],
                epoch: 0,
            })
        );
        await wait(250);

        expect(b.errors.some((e) => e.message.includes('doc_id mismatch'))).toBe(true);
        expect(b.vaultSync.list_files()).toHaveLength(0);
        expect(b.manifestPaths).toHaveLength(0);

        mallory.close();
        consoleErrorSpy.mockRestore();
    });

    it('rejects a FOREIGN-group ciphertext on the manifest channel; manifest unchanged, client survives', async () => {
        const consoleErrorSpy = jest.spyOn(console, 'error').mockImplementation(() => {});
        const { a, b } = (pair = await connectedPair('foreign'));

        // A ciphertext from an unrelated MLS group (mallory's own document), NOT
        // the group bob's manifest belongs to. AEAD/MLS authentication must fail.
        const foreign = WasmEncryptedDocument.create('mallory-doc', 'mallory');
        const bobPending = generate_key_package('mallory-peer');
        foreign.create_invite(bobPending.key_package); // advance past epoch 0 like a real group
        const foreignCiphertext = foreign.encrypt_bytes(new Uint8Array([1, 2, 3, 4, 5]));

        const mallory = await connectMallory();
        mallory.send(
            JSON.stringify({
                type: 'yrs_update',
                doc_id: MANIFEST_DOC_ID,
                encrypted: [...foreignCiphertext.ciphertext],
                epoch: Number(foreignCiphertext.epoch),
            })
        );
        await wait(250);

        expect(b.errors.some((e) => e.docId === MANIFEST_DOC_ID)).toBe(true);
        expect(b.vaultSync.list_files()).toHaveLength(0);
        expect(b.manifestPaths).toHaveLength(0);

        // No crash: a correctly-bound manifest update from the real peer still applies.
        const action = a.vaultSync.handle_created('notes/after-foreign.md');
        expect(a.client.sendManifestUpdate(action.manifest_update)).toBe(true);
        await wait(250);
        expect(b.vaultSync.list_files()).toContain('notes/after-foreign.md');

        mallory.close();
        consoleErrorSpy.mockRestore();
    });

    it('surfaces malformed manifest plaintext (decrypts under the group, garbage bytes) via onError without crashing', async () => {
        const consoleErrorSpy = jest.spyOn(console, 'error').mockImplementation(() => {});
        const { a, b } = (pair = await connectedPair('junk'));

        // Real peer, real group: the bytes decrypt, but they are not a valid
        // manifest CRDT update, so apply_remote_manifest must reject them.
        const garbage = new Uint8Array(16).fill(0xff);
        expect(a.client.sendManifestUpdate(garbage)).toBe(true);
        await wait(250);

        expect(b.errors.some((e) => e.docId === MANIFEST_DOC_ID)).toBe(true);
        expect(b.vaultSync.list_files()).toHaveLength(0);
        expect(b.manifestPaths).toHaveLength(0);

        // No crash: a real manifest update still applies afterwards.
        const action = a.vaultSync.handle_created('notes/after-junk.md');
        a.client.sendManifestUpdate(action.manifest_update);
        await wait(250);
        expect(b.vaultSync.list_files()).toContain('notes/after-junk.md');

        consoleErrorSpy.mockRestore();
    });

    it('marks an out-of-scope create as ignored with an empty manifest update', async () => {
        // Scoped sync: only the "notes" folder is in scope.
        const scoped = new WasmVaultSync(['notes'], [], true, true);
        const action = scoped.handle_created('private/secret.md');
        expect(action.kind).toBe('ignored');
        expect(action.manifest_update).toHaveLength(0);
        expect(scoped.list_files()).toHaveLength(0);
    });
});
