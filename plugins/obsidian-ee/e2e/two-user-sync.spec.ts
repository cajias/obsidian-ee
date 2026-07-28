import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { webcrypto } from 'node:crypto';
import { WebSocket as WsWebSocket } from 'ws';
import { MockRelay } from './mock-relay';
import {
    CollabClient,
    ConfigValidationError,
    type CollabClientConfig,
    type CollabError,
} from '../src/collab-client';
import type { CollabCore as CollabCoreType } from '../src/wasm/collab_wasm';

// This spec drives the REAL behavior that exists today entirely in the Playwright
// runner's Node context: two independently-constructed CollabClients, each with its
// own real compiled-WASM CollabCore, exchanging AES-PSK-encrypted updates over the
// real ws-based MockRelay. Driving Obsidian/Electron is explicitly out of scope
// (tracked separately) — this proves the collaboration + fail-closed crypto path.
//
// Requires the compiled WASM on disk (src/wasm/collab_wasm_bg.wasm). `npm run e2e`
// does NOT rebuild it; CI's plugin job builds it via scripts/build-wasm.sh before
// running e2e. The loader below reads that committed-on-disk artifact.
//
// NOTE ON THE LOADER: this duplicates the ~6-line load pattern from
// src/__tests__/helpers/load-real-wasm.ts rather than importing it. That helper
// resolves its path via `import.meta.url` (required under jest's ESM runner), but
// Playwright transpiles specs to CommonJS, and a static/dynamic import of a .ts
// file carrying `import.meta` fails Playwright's loader. Dynamic import() of the
// compiled ESM .js artifact is the one path that works from a CJS spec, so the
// load lives here.

// Real compiled WASM CollabCore constructor, captured after init in beforeAll.
let CollabCore!: typeof CollabCoreType;

/** Load + init the REAL committed WASM artifact (mirrors load-real-wasm.ts). */
async function loadWasm(): Promise<typeof CollabCoreType> {
    // getrandom (wasm-pack --target web) calls crypto.getRandomValues, not OS
    // entropy. Guard thin hosts so encrypt() never surfaces a getrandom error.
    if (!(globalThis as { crypto?: Crypto }).crypto) {
        (globalThis as { crypto?: Crypto }).crypto = webcrypto as unknown as Crypto;
    }
    const mod = await import('../src/wasm/collab_wasm.js');
    const wasmPath = join(__dirname, '..', 'src', 'wasm', 'collab_wasm_bg.wasm');
    const bytes = readFileSync(wasmPath);
    const compiled = await WebAssembly.compile(bytes);
    await mod.default({ module_or_path: compiled });
    return mod.CollabCore as typeof CollabCoreType;
}

/**
 * Real WebSocket wrapper that connects to the actual ws MockRelay. Node 20 has no
 * global WebSocket, and CollabClient constructs `new WebSocket(url)` and reads
 * `WebSocket.OPEN` / `event.data`, so we shim globalThis with this ws-backed class.
 * Ports the proven wrapper from src/__tests__/two-user-integration.test.ts.
 */
class NodeWebSocket {
    private ws: WsWebSocket;
    onopen: (() => void) | null = null;
    onmessage: ((event: { data: string }) => void) | null = null;
    onclose: (() => void) | null = null;
    onerror: ((error: unknown) => void) | null = null;
    readyState = 0; // CONNECTING

    constructor(url: string) {
        this.ws = new WsWebSocket(url);

        this.ws.on('open', () => {
            this.readyState = 1; // OPEN
            this.onopen?.();
        });

        this.ws.on('message', (data: Buffer) => {
            this.onmessage?.({ data: data.toString() });
        });

        this.ws.on('close', () => {
            this.readyState = 3; // CLOSED
            this.onclose?.();
        });

        this.ws.on('error', (err: Error) => {
            this.onerror?.(err);
        });
    }

    send(data: string): void {
        if (this.ws.readyState === WsWebSocket.OPEN) {
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

// Override global WebSocket with the Node.js implementation.
(globalThis as { WebSocket?: unknown }).WebSocket = NodeWebSocket;

// Dedicated port to avoid colliding with the jest integration test (8082) and the
// default relay (8080).
const RELAY_PORT = 8083;
const RELAY_URL = `ws://localhost:${RELAY_PORT}`;

// Small helper: build a client on a given user/doc/key.
function makeClient(userId: string, docId: string, key: Uint8Array): CollabClient {
    const config: CollabClientConfig = {
        relayUrl: RELAY_URL,
        userId,
        docId,
        encryptionKey: key,
    };
    return new CollabClient(new CollabCore(), config);
}

const settle = (ms: number): Promise<void> => new Promise((r) => setTimeout(r, ms));

test.describe('Two User Sync Integration', () => {
    let relay: MockRelay;

    test.beforeAll(async () => {
        CollabCore = await loadWasm();
        relay = new MockRelay();
        await relay.start(RELAY_PORT);
    });

    test.afterAll(async () => {
        await relay.stop();
    });

    test('positive sync: matching keys relay AES-PSK ciphertext both users can read', async () => {
        // SAME 32-byte non-zero key (NOT all-zeros — that is rejected at construction).
        const key = new Uint8Array(32).fill(7);
        const clientA = makeClient('alice', 'shared-doc', key);
        const clientB = makeClient('bob', 'shared-doc', key);

        let bReceivedText = '';
        clientB.onUpdate((text) => {
            bReceivedText = text;
        });

        await Promise.all([clientA.connect(), clientB.connect()]);
        await settle(100);

        clientA.sendUpdate('Hello');
        await settle(200);

        // B decrypted and applied A's ciphertext relayed over the real ws MockRelay.
        expect(bReceivedText).toBe('Hello');
        expect(clientB.getText()).toBe('Hello');

        clientA.disconnect();
        clientB.disconnect();
        await settle(100);
    });

    test('fail-closed: a wrong key yields no plaintext and a decryption error', async () => {
        // DIFFERENT non-zero keys on the same doc — B must not be able to decrypt.
        const keyA = new Uint8Array(32).fill(1);
        const keyB = new Uint8Array(32).fill(2);
        const clientA = makeClient('secure-a', 'encrypted-doc', keyA);
        const clientB = makeClient('secure-b', 'encrypted-doc', keyB);

        const received: string[] = [];
        clientB.onUpdate((text) => {
            received.push(text);
        });
        const errors: CollabError[] = [];
        clientB.onError((err) => {
            errors.push(err);
        });

        await Promise.all([clientA.connect(), clientB.connect()]);
        await settle(100);

        clientA.sendUpdate('Secret message');
        await settle(200);

        // Wrong key = AEAD authentication fails: no update surfaces to B, its doc
        // stays empty, and a decryption-type error is reported (CLAUDE.md negative
        // trust-boundary invariant).
        expect(received).toHaveLength(0);
        expect(clientB.getText()).toBe('');
        expect(errors.some((e) => e.type === 'decryption')).toBe(true);

        clientA.disconnect();
        clientB.disconnect();
        await settle(100);
    });

    test('fail-closed: all-zeros placeholder key is rejected at construction', () => {
        // Reinforces #27's guard: validateConfig fails closed on the all-zeros key.
        const config: CollabClientConfig = {
            relayUrl: RELAY_URL,
            userId: 'zero-key',
            docId: 'shared-doc',
            encryptionKey: new Uint8Array(32), // all zeros
        };
        expect(() => new CollabClient(new CollabCore(), config)).toThrow(ConfigValidationError);
    });
});
