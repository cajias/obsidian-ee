/**
 * Owner-side join gate (#71), over the in-process fan-out relay with the REAL
 * compiled WASM.
 *
 * `applyGroupHandshake`, case `'key_package'`, used to answer EVERY inbound key
 * package on the document channel with a Welcome. There was no allowlist, invite
 * token, or confirmation step, so anyone who could reach the relay and knew a
 * `doc_id` became a decrypting member: relay-reachability equalled membership.
 *
 * The three cases here are deliberately one gate, not three:
 *  - the NEGATIVE case is the invariant (an unlisted requester gets nothing),
 *  - the POSITIVE case is what makes the negative one evidence rather than a
 *    vacuum — a listed requester still joins, so silence means "refused", not
 *    "the handshake never happened",
 *  - the IMPOSTOR case is the discriminator. The relay stamps a `from` on every
 *    fanned-out frame, and that field is whatever the sender typed at
 *    `identify`. A gate that read it would pass the first two cases and still be
 *    open, so one client claims an allowlisted name while presenting a key
 *    package minted for a different identity. The gate must read the identity
 *    out of the key package's own MLS credential — the identity that would
 *    actually end up in the group — never off the inbound frame.
 */
import { describe, it, expect, beforeAll, afterAll, afterEach } from '@jest/globals';
import { WebSocket } from 'ws';
import { CollabClient, type CollabClientConfig, type CollabError } from '../collab-client';
import { loadRealWasm } from './helpers/load-real-wasm';
// Importing installs the NodeWebSocket shim on the global (see the helper).
import { OriginalWebSocket, RecordingMockRelay } from './helpers/recording-mock-relay';

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

const RELAY_PORT = 8093;
const RELAY_URL = `ws://localhost:${RELAY_PORT}`;
const FILE_DOC = 'join-gate-doc.md';

interface TestClient {
    userId: string;
    client: CollabClient;
    errors: CollabError[];
    updates: string[];
}

/**
 * The MLS group a client currently holds, or null when it holds none.
 *
 * `doc` is private and there is no accessor: whether a group exists, and which
 * epoch it sits at, are internal details everywhere except here — where "was
 * the group extended?" IS the thing under test. Mirrors `epochOf` in
 * three-party-mls.test.ts.
 */
function groupOf(client: CollabClient): { epoch: bigint } | null {
    return (client as unknown as { doc: { epoch: bigint } | null }).doc;
}

describe('owner-side join gate (#71)', () => {
    let relay: RecordingMockRelay;
    const live: TestClient[] = [];

    /** Every handshake frame of `messageType` that `userId` sent for the file doc. */
    function handshakesFrom(userId: string, messageType: string) {
        return relay
            .framesFrom(userId)
            .filter(
                (f) =>
                    f.msg.type === 'mls_handshake' &&
                    f.msg.doc_id === FILE_DOC &&
                    f.msg.message_type === messageType
            );
    }

    function makeClient(userId: string, role: 'owner' | 'joiner', allowedJoiners?: string[]) {
        const config: CollabClientConfig = {
            relayUrl: RELAY_URL,
            userId,
            docId: FILE_DOC,
            role,
            ...(allowedJoiners ? { allowedJoiners } : {}),
        };
        const entry: TestClient = {
            userId,
            client: new CollabClient(config),
            errors: [],
            updates: [],
        };
        entry.client.onError((e) => entry.errors.push(e));
        entry.client.onUpdate((text) => entry.updates.push(text));
        live.push(entry);
        return entry;
    }

    beforeAll(async () => {
        await loadRealWasm();
        relay = new RecordingMockRelay();
        await relay.start(RELAY_PORT);
    });

    afterAll(async () => {
        await relay.stop();
        if (OriginalWebSocket) {
            (global as unknown as { WebSocket: unknown }).WebSocket = OriginalWebSocket;
        }
    });

    afterEach(async () => {
        // destroy(), not disconnect(): these clients hold REAL wasm groups, which
        // disconnect() deliberately keeps alive for a resume.
        live.splice(0).forEach((t) => t.client.destroy());
        await wait(50);
    });

    it('an unauthorized key package receives no Welcome and does not extend the group', async () => {
        const alice = makeClient('alice-unauth', 'owner', ['bob-unauth']);
        const mallory = makeClient('mallory-unauth', 'joiner');

        await alice.client.connect();
        await wait(50);
        await mallory.client.connect();
        await wait(400);

        // The request really reached the owner, and the owner really had a group
        // to extend — so the refusal below is a decision, not a missing handshake.
        expect(handshakesFrom('mallory-unauth', 'key_package')).toHaveLength(1);
        expect(groupOf(alice.client)).not.toBeNull();

        // No Welcome...
        expect(handshakesFrom('alice-unauth', 'welcome')).toHaveLength(0);
        // ...and the group was not extended: an add would have committed it to
        // epoch 1, and the commit that carries existing members would have gone out.
        expect(groupOf(alice.client)?.epoch).toBe(0n);
        expect(handshakesFrom('alice-unauth', 'commit')).toHaveLength(0);
        expect(groupOf(mallory.client)).toBeNull();

        // The property all of that exists for: the outsider decrypts nothing.
        expect(alice.client.sendUpdate('members only')).toBe(true);
        await wait(200);
        expect(mallory.updates).toEqual([]);
    });

    it('an owner that configured no allowlist admits nobody', async () => {
        // The default plugin settings ship an empty list. An unset list must not
        // read as "admit everyone" — that is the open admission #71 closes.
        const alice = makeClient('alice-nolist', 'owner');
        const bob = makeClient('bob-nolist', 'joiner');

        await alice.client.connect();
        await wait(50);
        await bob.client.connect();
        await wait(400);

        expect(handshakesFrom('bob-nolist', 'key_package')).toHaveLength(1);
        expect(handshakesFrom('alice-nolist', 'welcome')).toHaveLength(0);
        expect(groupOf(alice.client)?.epoch).toBe(0n);
        expect(groupOf(bob.client)).toBeNull();
    });

    it('an allowlisted joiner is still welcomed and still receives content', async () => {
        const alice = makeClient('alice-allowed', 'owner', ['bob-allowed']);
        const bob = makeClient('bob-allowed', 'joiner');

        await alice.client.connect();
        await wait(50);
        await bob.client.connect();
        await wait(400);

        expect(handshakesFrom('alice-allowed', 'welcome')).toHaveLength(1);
        expect(groupOf(bob.client)).not.toBeNull();
        expect(groupOf(alice.client)?.epoch).toBe(1n);

        const secret = 'readable by the member the owner actually invited';
        expect(alice.client.sendUpdate(secret)).toBe(true);
        await wait(300);
        expect(bob.updates).toContain(secret);
        expect(bob.errors).toEqual([]);
    });

    it('refuses a key package whose credential is not the allowlisted name it claims', async () => {
        const { generate_key_package } = await loadRealWasm();
        const alice = makeClient('alice-impostor', 'owner', ['bob-impostor']);
        await alice.client.connect();
        await wait(50);

        // Minted for mallory, presented by a client identifying as bob.
        const pending = generate_key_package('mallory-impostor');
        const keyPackage = [...pending.key_package];
        pending.free();

        const socket = new WebSocket(RELAY_URL);
        await new Promise<void>((resolve, reject) => {
            socket.on('open', () => resolve());
            socket.on('error', reject);
        });
        socket.send(JSON.stringify({ type: 'identify', user_id: 'bob-impostor' }));
        socket.send(
            JSON.stringify({
                type: 'mls_handshake',
                doc_id: FILE_DOC,
                payload: keyPackage,
                message_type: 'key_package',
            })
        );
        await wait(400);

        // It arrived under the allowlisted name...
        expect(handshakesFrom('bob-impostor', 'key_package')).toHaveLength(1);
        // ...and was still refused, because the credential inside it says mallory.
        expect(handshakesFrom('alice-impostor', 'welcome')).toHaveLength(0);
        expect(groupOf(alice.client)?.epoch).toBe(0n);

        socket.close();
    });
});
