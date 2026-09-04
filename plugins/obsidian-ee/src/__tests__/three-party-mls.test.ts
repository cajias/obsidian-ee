/**
 * Three-party MLS choreography over the in-process fan-out relay, with the REAL
 * compiled WASM (#72 follow-up).
 *
 * `EncryptedDocument::create_invite` returns BOTH a `welcome` (for the new
 * member) and a `commit` (for the EXISTING members). Forwarding only the Welcome
 * works for exactly two parties and breaks at the third: the existing member
 * never runs `process_commit`, so it stays at the old epoch while the owner and
 * the new member move on. Its MLS state diverges (it can no longer decrypt), and
 * the capability it re-presents is minted at a stale epoch, so under subscribe
 * authorization (#72) the relay withholds content from it entirely.
 *
 * The reference choreography is the Rust half: `three_real_members` in
 * tests/e2e-tests/tests/subscribe_authz.rs, where Bob — an EXISTING member —
 * reaches the new epoch by processing the add-commit, and only Alice (the owner)
 * registers the anchor rotation.
 */
import { describe, it, expect, beforeAll, afterAll, afterEach } from '@jest/globals';
import { CollabClient, type CollabClientConfig, type CollabError } from '../collab-client';
import { loadRealWasm } from './helpers/load-real-wasm';
// Importing installs the NodeWebSocket shim on the global (see the helper).
import {
    OriginalWebSocket,
    RecordingMockRelay,
    type RecordedFrame,
} from './helpers/recording-mock-relay';

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** The MLS epoch a client's file group currently sits at. */
function epochOf(client: CollabClient): bigint {
    // `doc` is private and there is no accessor: the epoch is an internal detail
    // everywhere except here, where divergence IS the thing under test.
    const doc = (client as unknown as { doc: { epoch: bigint } | null }).doc;
    if (!doc) {
        throw new Error('client has no MLS group');
    }
    return doc.epoch;
}

/** Every `subscribe` frame this user sent for `docId`, in send order. */
function subscribeFrames(frames: RecordedFrame[], docId: string): RecordedFrame[] {
    return frames.filter((f) => f.msg.type === 'subscribe' && f.msg.doc_id === docId);
}

describe('three-party MLS: the add-commit reaches existing members', () => {
    let relay: RecordingMockRelay;
    const RELAY_PORT = 8092;
    const RELAY_URL = `ws://localhost:${RELAY_PORT}`;
    const FILE_DOC = 'three-party-doc.md';

    interface TestClient {
        userId: string;
        client: CollabClient;
        errors: CollabError[];
        updates: string[];
    }

    function makeClient(userId: string, role: 'owner' | 'joiner'): TestClient {
        const config: CollabClientConfig = { relayUrl: RELAY_URL, userId, docId: FILE_DOC, role };
        const client = new CollabClient(config);
        const errors: CollabError[] = [];
        const updates: string[] = [];
        client.onError((e) => errors.push(e));
        client.onUpdate((text) => updates.push(text));
        return { userId, client, errors, updates };
    }

    /**
     * Alice (owner), then Bob, then Carol — each connect settling before the
     * next, so Carol's add is a THIRD-party join into a group Bob already
     * belongs to. That ordering is the whole point: Bob is the existing member
     * whose state the add-commit has to move.
     */
    async function connectedTrio(
        tag: string
    ): Promise<{ alice: TestClient; bob: TestClient; carol: TestClient }> {
        const alice = makeClient(`alice-${tag}`, 'owner');
        const bob = makeClient(`bob-${tag}`, 'joiner');
        const carol = makeClient(`carol-${tag}`, 'joiner');
        await alice.client.connect();
        await wait(50);
        await bob.client.connect();
        await wait(300); // Bob's handshake settles: the group reaches epoch 1
        await carol.client.connect();
        await wait(400); // Carol's add settles: the group reaches epoch 2
        return { alice, bob, carol };
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

    let trio: { alice: TestClient; bob: TestClient; carol: TestClient } | undefined;

    afterEach(async () => {
        if (!trio) {
            return;
        }
        [trio.alice, trio.bob, trio.carol].forEach((t) => t.client.disconnect());
        trio = undefined;
        await wait(50);
    });

    it('moves the existing member to the new epoch when a third member joins', async () => {
        const { alice, bob, carol } = (trio = await connectedTrio('epoch'));

        expect(epochOf(alice.client)).toBe(2n);
        expect(epochOf(carol.client)).toBe(2n);
        // The bug: Bob stays at epoch 1, having never seen the add-commit.
        expect(epochOf(bob.client)).toBe(epochOf(alice.client));
    });

    it('keeps content flowing to the existing member after the third join', async () => {
        const { alice, bob, carol } = (trio = await connectedTrio('content'));
        const secret = 'readable by every member, including the one who was already here';

        expect(alice.client.sendUpdate(secret)).toBe(true);
        await wait(300);

        expect(carol.updates).toContain(secret);
        // Bob's group is a stale epoch behind, so MLS refuses the ciphertext.
        expect(bob.updates).toContain(secret);
        expect(bob.errors).toEqual([]);
    });

    it('re-presents the existing member capability at the new epoch', async () => {
        const { bob } = (trio = await connectedTrio('cap'));

        const subscribes = subscribeFrames(relay.framesFrom(bob.userId), FILE_DOC);
        const last = subscribes.at(-1);
        expect(last?.msg.capability).toBeDefined();
        // A capability minted at epoch 1 no longer verifies against an epoch-2
        // anchor: the relay gates content fan-out on strict epoch equality.
        expect(last?.msg.capability.epoch).toBe(2);
    });

    it('CHARACTERIZATION: registers the anchor rotation exactly once, from the owner', async () => {
        // Green before the commit-forwarding fix as well as after: no client
        // registered anything but the owner's rotation when the commit never
        // moved. It is kept as the guard on WHO registers — the wrong fix, in
        // which an existing member registers the rotation process_commit hands
        // it, turns this red.
        const { alice, bob, carol } = (trio = await connectedTrio('anchor'));

        const registrations = (t: TestClient) =>
            relay.framesFrom(t.userId).filter((f) => f.msg.type === 'register_doc_key');

        // Only ONE registration can win: the relay verifies the continuity proof
        // under the CURRENT anchor key and then demands a strictly higher epoch
        // (crates/collab-relay/src/relay.rs, `handle_register_doc_key`). A second
        // registration of the same rotation is rejected `Unauthorized` twice
        // over, so an existing member must process the commit and drop the
        // rotation it returns.
        expect(registrations(bob)).toEqual([]);
        expect(registrations(carol)).toEqual([]);
        expect(registrations(alice).map((f) => f.msg.epoch)).toEqual([0, 1, 2]);
    });

    it('sends the commit before the welcome, so the new member no-ops it', async () => {
        const { alice, carol } = (trio = await connectedTrio('order'));

        const frames = relay.framesFrom(alice.userId);
        const rotationAt = frames.findIndex(
            (f) => f.msg.type === 'register_doc_key' && f.msg.epoch === 2
        );
        expect(rotationAt).toBeGreaterThanOrEqual(0);

        // The owner's reaction to Carol's key package, in order: move the relay's
        // anchor, re-present its own now-stale capability, hand the existing
        // members the commit, and only then admit the new member. The commit
        // MUST precede the welcome — the relay fans every handshake frame out to
        // all other subscribers, so Carol sees it too, and she only no-ops it
        // while she still has no group. After her welcome she would instead try
        // to process the add-commit that created her own epoch, and throw.
        const after = frames.slice(rotationAt + 1);
        expect(after[0].msg.type).toBe('subscribe');
        expect(after[0].msg.capability.epoch).toBe(2);
        expect(after[1].msg.type).toBe('mls_handshake');
        expect(after[1].msg.message_type).toBe('commit');
        expect(after[2].msg.type).toBe('mls_handshake');
        expect(after[2].msg.message_type).toBe('welcome');

        expect(carol.errors).toEqual([]);
    });
});
