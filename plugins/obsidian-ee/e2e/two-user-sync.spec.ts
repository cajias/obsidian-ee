import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { webcrypto } from 'node:crypto';
import { createRequire } from 'node:module';
import { WebSocket as WsWebSocket } from 'ws';
import { MockRelay } from './mock-relay';
import type { CollabClient, CollabClientConfig, CollabRole } from '../src/collab-client';

// CollabClient must NOT be statically imported here. Playwright transpiles this
// CJS spec's imports into require() calls, and collab-client.ts statically imports
// the wasm-pack ESM package (src/wasm has "type": "module"), which cannot be
// require()d — the transpiled chain dies with "exports is not defined in ES module
// scope" and Playwright collects 0 tests. Instead, beforeAll dynamic-imports the
// ESM wasm module (the one loader path that works from CJS — see NOTE below),
// seeds it into the CJS require cache, and only then require()s collab-client so
// its inner require('./wasm/collab_wasm') resolves to the pre-loaded ESM namespace.
type CollabClientCtorType = new (config: CollabClientConfig) => CollabClient;
let CollabClientCtor: CollabClientCtorType;

const nodeRequire = createRequire(__filename);

// This spec drives the REAL MLS behavior entirely in the Playwright runner's Node
// context: two independently-constructed CollabClients, each with its own real
// compiled-WASM MLS document, running the owner/joiner handshake (key_package ->
// welcome) and then exchanging MLS-encrypted updates over the real ws-based
// MockRelay. Driving Obsidian/Electron is explicitly out of scope (tracked
// separately) — this proves the collaboration + fail-closed crypto path.
//
// Requires the compiled WASM on disk (src/wasm/collab_wasm_bg.wasm). `npm run e2e`
// does NOT rebuild it; CI's plugin job builds it via scripts/build-wasm.sh before
// running e2e. The loader below inits that committed-on-disk artifact.
//
// NOTE ON THE LOADER: this duplicates the ~6-line load pattern from
// src/__tests__/helpers/load-real-wasm.ts rather than importing it. That helper
// resolves its path via `import.meta.url` (required under jest's ESM runner), but
// Playwright transpiles specs to CommonJS, and a static/dynamic import of a .ts
// file carrying `import.meta` fails Playwright's loader. Dynamic import() of the
// compiled ESM .js artifact is the one path that works from a CJS spec, so the
// load lives here. CollabClient imports the MLS classes itself; the spec only
// needs the module initialized before any client constructs its group.

/** Init the REAL committed WASM artifact (mirrors load-real-wasm.ts). */
async function initWasm(): Promise<void> {
    // getrandom (wasm-pack --target web) calls crypto.getRandomValues, not OS
    // entropy. Guard thin hosts so MLS key generation never surfaces a getrandom error.
    if (!(globalThis as { crypto?: Crypto }).crypto) {
        (globalThis as { crypto?: Crypto }).crypto = webcrypto as unknown as Crypto;
    }
    const mod = await import('../src/wasm/collab_wasm.js');
    const wasmPath = join(__dirname, '..', 'src', 'wasm', 'collab_wasm_bg.wasm');
    const bytes = readFileSync(wasmPath);
    const compiled = await WebAssembly.compile(bytes);
    await mod.default({ module_or_path: compiled });

    // Seed the initialized ESM namespace into the CJS module cache under the path
    // collab-client.ts's transpiled require('./wasm/collab_wasm') resolves to, then
    // require collab-client. ponytail: require-cache seeding is a contained hack;
    // upgrade path is running the whole Playwright project in ESM mode once
    // Playwright treats src/*.ts (CJS package scope) as ESM.
    const wasmModulePath = nodeRequire.resolve('../src/wasm/collab_wasm.js');
    nodeRequire.cache[wasmModulePath] = {
        id: wasmModulePath,
        filename: wasmModulePath,
        loaded: true,
        exports: mod,
    } as unknown as NodeJS.Module;
    ({ CollabClient: CollabClientCtor } = nodeRequire('../src/collab-client') as {
        CollabClient: CollabClientCtorType;
    });
}

/**
 * Real WebSocket wrapper that connects to the actual ws MockRelay. Node 20 has no
 * global WebSocket, and CollabClient constructs `new WebSocket(url)` and reads
 * `WebSocket.OPEN` / `event.data`, so we shim globalThis with this ws-backed class.
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

// Small helper: build a client on a given user/doc/role.
function makeClient(userId: string, docId: string, role: CollabRole): CollabClient {
    const config: CollabClientConfig = {
        relayUrl: RELAY_URL,
        userId,
        docId,
        role,
    };
    return new CollabClientCtor(config);
}

const settle = (ms: number): Promise<void> => new Promise((r) => setTimeout(r, ms));

test.describe('Two User MLS Sync Integration', () => {
    let relay: MockRelay;

    test.beforeAll(async () => {
        await initWasm();
        relay = new MockRelay();
        await relay.start(RELAY_PORT);
    });

    test.afterAll(async () => {
        await relay.stop();
    });

    test('MLS handshake + round-trip: owner and joiner converge over the relay', async () => {
        const owner = makeClient('alice', 'shared-doc', 'owner');
        const joiner = makeClient('bob', 'shared-doc', 'joiner');

        let bReceivedText = '';
        joiner.onUpdate((text) => {
            bReceivedText = text;
        });

        // Owner connects first so it is registered/subscribed before the joiner's
        // key_package broadcast arrives (the relay fans out only to OTHER clients).
        await owner.connect();
        await settle(100);
        await joiner.connect();
        // Let the KeyPackage -> Welcome handshake complete across the wire.
        await settle(300);

        owner.sendUpdate('Hello over MLS');
        await settle(200);

        // Bob is an MLS group member: he decrypts and applies Alice's ciphertext.
        expect(bReceivedText).toBe('Hello over MLS');
        expect(joiner.getText()).toBe('Hello over MLS');

        owner.disconnect();
        joiner.disconnect();
        await settle(100);
    });

    test('fail-closed: a joiner before its Welcome has no group and emits no update', async () => {
        // A separate relay lets the joiner connect WITHOUT an owner online, so no
        // Welcome ever arrives. sendUpdate must fail closed: no group, no frame,
        // no plaintext path (CLAUDE.md invariant).
        const soloRelay = new MockRelay();
        const soloPort = 8084;
        await soloRelay.start(soloPort);

        const joiner = new CollabClientCtor({
            relayUrl: `ws://localhost:${soloPort}`,
            userId: 'lonely-bob',
            docId: 'orphan-doc',
            role: 'joiner',
        });

        await joiner.connect();
        await settle(200); // no owner => no Welcome => group never established

        // Fail-closed: no MLS group means sendUpdate returns false and sends nothing.
        expect(joiner.sendUpdate('secret text')).toBe(false);
        expect(joiner.getText()).toBe('');

        joiner.disconnect();
        await soloRelay.stop();
    });
});
