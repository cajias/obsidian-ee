/**
 * Two-client MLS handshake + encrypted round-trip over the REAL collab-relay
 * BINARY (issue #51).
 *
 * Unlike two-user-integration.test.ts (JS mock relay, AES-PSK shared key) and
 * mls-wasm.test.ts (in-process MLS, no relay), this spec spawns the actual
 * `cargo run -p collab-relay` process and drives two real-compiled-WASM clients
 * through a full MLS handshake (KeyPackage -> Welcome) across the wire, then
 * round-trips a real MLS-encrypted yrs update A->B. This is the real-relay
 * analog of collab-core's `test_two_users_collaborate`: it proves the relay is a
 * zero-knowledge router and MLS membership -- not relay access -- gates
 * decryption.
 *
 * The relay fans each frame out to all OTHER subscribers (sender excluded), so
 * both clients identify + subscribe (awaiting `subscribed`) before any handshake
 * frame is sent, or the fan-out reaches an empty set.
 */
import { describe, it, expect, beforeAll, afterAll, afterEach } from '@jest/globals';
import { spawn, type ChildProcess } from 'node:child_process';
import { createConnection } from 'node:net';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { WebSocket } from 'ws';
import { loadRealWasm } from './helpers/load-real-wasm';
// Instance type: WasmEncryptedDocument has a private constructor, so
// InstanceType<typeof ...> is not derivable; import the generated class as a type.
import type { WasmEncryptedDocument as WasmDoc } from '../wasm/collab_wasm';

// Dedicated port: 8080 (relay default), 8082 (jest mock), 8083 (playwright) are taken.
const PORT = 8085;
const URL = `ws://127.0.0.1:${PORT}`;
const DOC_ID = 'mls-doc';

// __tests__ -> src -> obsidian-ee -> plugins -> repo root (4 levels up).
const here = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = join(here, '..', '..', '..', '..');

type Wasm = Awaited<ReturnType<typeof loadRealWasm>>;

/** Loosely-typed relay ServerMessage frame (snake_case, serde tag = "type"). */
interface WireMsg {
    type: string;
    user_id?: string;
    doc_id?: string;
    from?: string;
    payload?: number[];
    message_type?: string;
    encrypted?: number[];
    epoch?: number;
    code?: string;
    message?: string;
}

/** A single websocket client with a message buffer + predicate waiter. */
interface WireClient {
    send: (msg: unknown) => void;
    waitFor: (pred: (m: WireMsg) => boolean, timeoutMs?: number) => Promise<WireMsg>;
    close: () => void;
}

let relayProc: ChildProcess | undefined;
let wasm: Wasm;
const openClients: WebSocket[] = [];

/** TCP-connect poll: the relay exposes no HTTP health endpoint (docker uses `nc -z`). */
async function waitForPort(port: number, timeoutMs: number): Promise<void> {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
        const reachable = await new Promise<boolean>((resolve) => {
            const socket = createConnection({ port, host: '127.0.0.1' });
            socket.once('connect', () => {
                socket.destroy();
                resolve(true);
            });
            socket.once('error', () => {
                socket.destroy();
                resolve(false);
            });
        });
        if (reachable) {
            return;
        }
        await new Promise((r) => setTimeout(r, 200));
    }
    throw new Error(`relay port ${port} not ready within ${timeoutMs}ms`);
}

/** Open a raw ws client that buffers frames and resolves waiters as they arrive. */
async function makeClient(): Promise<WireClient> {
    const ws = new WebSocket(URL);
    openClients.push(ws);
    const messages: WireMsg[] = [];
    const waiters: Array<{ pred: (m: WireMsg) => boolean; resolve: (m: WireMsg) => void }> = [];

    ws.on('message', (data: Buffer) => {
        // The relay only emits JSON, but a malformed frame must not throw inside
        // the event callback (an uncaught throw here crashes the jest worker).
        let msg: WireMsg;
        try {
            msg = JSON.parse(data.toString()) as WireMsg;
        } catch {
            return;
        }
        messages.push(msg);
        for (let i = waiters.length - 1; i >= 0; i--) {
            const waiter = waiters[i];
            if (waiter.pred(msg)) {
                waiter.resolve(msg);
                waiters.splice(i, 1);
            }
        }
    });

    await new Promise<void>((resolve, reject) => {
        ws.once('open', () => resolve());
        ws.once('error', reject);
    });

    return {
        send: (msg: unknown) => ws.send(JSON.stringify(msg)),
        waitFor: (pred, timeoutMs = 10_000) =>
            new Promise<WireMsg>((resolve, reject) => {
                const existing = messages.find((m) => pred(m));
                if (existing) {
                    resolve(existing);
                    return;
                }
                const timer = setTimeout(() => reject(new Error('waitFor timed out')), timeoutMs);
                waiters.push({
                    pred,
                    resolve: (m) => {
                        clearTimeout(timer);
                        resolve(m);
                    },
                });
            }),
        close: () => ws.close(),
    };
}

/** Identify as `userId`, subscribe to `DOC_ID`, awaiting each ack. */
async function connectAndJoin(userId: string): Promise<WireClient> {
    const client = await makeClient();
    client.send({ type: 'identify', user_id: userId });
    await client.waitFor((m) => m.type === 'identified' && m.user_id === userId);
    client.send({ type: 'subscribe', doc_id: DOC_ID });
    await client.waitFor((m) => m.type === 'subscribed' && m.doc_id === DOC_ID);
    return client;
}

/**
 * Drive the KeyPackage -> Welcome handshake between an already-subscribed Alice
 * and Bob over the relay. Returns the two real WASM documents.
 *
 * A 2-user join needs only the Welcome for the second member; there are no OTHER
 * existing members for the commit to update (mirrors collab-core's
 * `setup_two_user_group`).
 */
async function handshakeAliceBob(
    aliceClient: WireClient,
    bobClient: WireClient
): Promise<{ alice: WasmDoc; bob: WasmDoc }> {
    const { WasmEncryptedDocument, WasmInvite, generate_key_package } = wasm;

    // Bob publishes his key package.
    const bobPending = generate_key_package('bob');
    bobClient.send({
        type: 'mls_handshake',
        doc_id: DOC_ID,
        payload: [...bobPending.key_package],
        message_type: 'key_package',
    });

    // Alice receives it, opens her group, and ships back the Welcome.
    const kpFrame = await aliceClient.waitFor(
        (m) => m.type === 'mls_handshake' && m.message_type === 'key_package' && m.from === 'bob'
    );
    const alice = WasmEncryptedDocument.create(DOC_ID, 'alice');
    const invite = alice.create_invite(new Uint8Array(kpFrame.payload ?? []));
    aliceClient.send({
        type: 'mls_handshake',
        doc_id: DOC_ID,
        payload: [...invite.welcome],
        message_type: 'welcome',
    });

    // Bob receives the Welcome bytes off the wire and joins. In the MLS path
    // `doc_id` is only the yrs identity label (passed to CollabDocument::new);
    // it has NO cryptographic effect -- mls.encrypt/decrypt take no AAD/doc_id
    // and MLS binds each message to the group via its internal GroupContext.
    // What gates Bob's decryption is MLS GROUP MEMBERSHIP: the `welcome` bytes
    // seal group secrets to his key. Bob uses his local DOC_ID as the yrs label
    // (fine), but that is a naming choice, not a crypto trust boundary. (The
    // docId-as-AAD trust-boundary rule lives in the OTHER AES-PSK CollabCore
    // path, not here.)
    const welFrame = await bobClient.waitFor(
        (m) => m.type === 'mls_handshake' && m.message_type === 'welcome' && m.from === 'alice'
    );
    const inviteForBob = WasmInvite.from_welcome(DOC_ID, new Uint8Array(welFrame.payload ?? []));
    const bob = WasmEncryptedDocument.join(inviteForBob, bobPending);
    return { alice, bob };
}

describe('Two-user MLS over the real relay binary', () => {
    beforeAll(async () => {
        relayProc = spawn('cargo', ['run', '--quiet', '-p', 'collab-relay'], {
            cwd: REPO_ROOT,
            env: { ...process.env, RELAY_ADDR: `127.0.0.1:${PORT}` },
            stdio: 'ignore',
            // `cargo run` launches the relay as a GRANDCHILD; detached makes this
            // child a process-group leader so the relay joins its group and the
            // whole tree can be signalled at once in afterAll (see below).
            detached: true,
        });
        // Cold `cargo run` may compile first; TCP-poll until the listener is up.
        await waitForPort(PORT, 120_000);
        wasm = await loadRealWasm();
    }, 120_000);

    afterEach(() => {
        for (const ws of openClients) {
            ws.close();
        }
        openClients.length = 0;
    });

    afterAll(() => {
        // `cargo run` reparents the relay binary as a grandchild; killing only the
        // direct cargo child would orphan the relay (it reparents to init and keeps
        // holding the port). A later run's waitForPort could then connect to the
        // zombie and FALSE-PASS against it. We spawned detached (process-group
        // leader), so signal the whole GROUP via the negative pid. afterAll runs
        // even when a test throws, so nothing leaks across the suite.
        if (relayProc?.pid) {
            try {
                process.kill(-relayProc.pid, 'SIGKILL'); // negative pid = kill the process group
            } catch {
                /* group already gone */
            }
        }
    });

    it('round-trips a real MLS-encrypted yrs update A->B through the relay', async () => {
        const aliceClient = await connectAndJoin('alice');
        const bobClient = await connectAndJoin('bob');

        const { alice, bob } = await handshakeAliceBob(aliceClient, bobClient);

        // Alice edits and ships the MLS-encrypted op over the wire.
        alice.insert(0, 'Hello over MLS');
        const op = alice.get_encrypted_update();
        aliceClient.send({
            type: 'yrs_update',
            doc_id: DOC_ID,
            encrypted: [...op.ciphertext],
            // BigInt is not JSON-serializable; marshal epoch as a Number on the wire.
            epoch: Number(op.epoch),
        });

        // Bob receives and decrypts. epoch comes back as a Number; the binding
        // wants a BigInt.
        const update = await bobClient.waitFor(
            (m) => m.type === 'yrs_update' && m.from === 'alice'
        );
        bob.apply_encrypted_update(
            new Uint8Array(update.encrypted ?? []),
            BigInt(update.epoch ?? 0)
        );

        expect(bob.get_content()).toBe('Hello over MLS');
    });

    it('rejects a cross-group op: Eve founds a SEPARATE MLS group sharing the same doc_id (shared docId grants nothing)', async () => {
        const aliceClient = await connectAndJoin('alice');
        const bobClient = await connectAndJoin('bob');
        // Eve has full relay access to the doc but is NOT in Alice+Bob's MLS group.
        const eveClient = await connectAndJoin('eve');

        const { alice, bob } = await handshakeAliceBob(aliceClient, bobClient);
        // Eve is the FOUNDER of her OWN independent MLS group that merely reuses
        // the same doc_id STRING. `create` opens a brand-new group with fresh
        // secrets -- it does NOT join Alice's group. This is the strong form of
        // the CLAUDE.md invariant "a ciphertext valid for one document MUST fail
        // authentication when applied to another": the shared doc_id label gives
        // the attacker NOTHING because doc_id has no crypto effect in the MLS
        // path -- decryption is gated by MLS group membership alone.
        const eveDoc = wasm.WasmEncryptedDocument.create(DOC_ID, 'eve');

        alice.insert(0, 'members only');
        const op = alice.get_encrypted_update();
        aliceClient.send({
            type: 'yrs_update',
            doc_id: DOC_ID,
            encrypted: [...op.ciphertext],
            epoch: Number(op.epoch),
        });

        // Both Bob (group member) and Eve (separate group, same doc_id) receive
        // the SAME frame off the relay.
        const bobUpdate = await bobClient.waitFor(
            (m) => m.type === 'yrs_update' && m.from === 'alice'
        );
        const eveUpdate = await eveClient.waitFor(
            (m) => m.type === 'yrs_update' && m.from === 'alice'
        );

        // The frame is a genuinely valid ciphertext: the member decrypts it.
        bob.apply_encrypted_update(
            new Uint8Array(bobUpdate.encrypted ?? []),
            BigInt(bobUpdate.epoch ?? 0)
        );
        expect(bob.get_content()).toBe('members only');

        // Eve's SEPARATE group cannot: the ciphertext authenticates against
        // Alice's GroupContext, not Eve's. MLS group membership -- not relay
        // access, not the shared doc_id -- gates decryption.
        expect(() =>
            eveDoc.apply_encrypted_update(
                new Uint8Array(eveUpdate.encrypted ?? []),
                BigInt(eveUpdate.epoch ?? 0)
            )
        ).toThrow();
        expect(eveDoc.get_content()).toBe('');
    });
});
