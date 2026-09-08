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

/**
 * The un-answered key package a joiner is still holding, or null when it holds
 * none. Same reasoning as `groupOf`: `pending` is an internal detail everywhere
 * except here, where "did the refused joiner let go of its key package?" IS the
 * thing under test.
 */
function pendingOf(client: CollabClient): object | null {
    return (client as unknown as { pending: object | null }).pending;
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

    function makeClient(
        userId: string,
        role: 'owner' | 'joiner',
        allowedJoiners?: string[],
        overrides: Partial<CollabClientConfig> = {}
    ) {
        const config: CollabClientConfig = {
            relayUrl: RELAY_URL,
            userId,
            docId: FILE_DOC,
            role,
            allowedJoiners,
            ...overrides,
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
        // The relay outlives every test in this file; a withhold left armed
        // would silently break the next one.
        relay.withhold = null;
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

    it('an owner that adds an id mid-session admits the joiner it just refused', async () => {
        // The first-run flow, end to end, against the REAL wasm: a joiner must
        // start a session to learn its id, that attempt is refused, the owner
        // pastes the id in, and the joiner retries. Before this the owner's list
        // was a snapshot captured at construction and the refused joiner never
        // let go of its key package, so BOTH sides had to restart.
        const JOIN_TIMEOUT_MS = 150;
        const alice = makeClient('alice-live', 'owner', []);
        const bob = makeClient('bob-live', 'joiner', undefined, {
            joinTimeoutMs: JOIN_TIMEOUT_MS,
        });

        await alice.client.connect();
        await wait(50);
        await bob.client.connect();
        await wait(400);

        // Refused, as the gate requires — and the request really arrived.
        expect(handshakesFrom('bob-live', 'key_package')).toHaveLength(1);
        expect(handshakesFrom('alice-live', 'welcome')).toHaveLength(0);
        // ...and the refusal is now VISIBLE and recoverable: the timeout freed
        // the un-answered key package and told the user.
        expect(pendingOf(bob.client)).toBeNull();
        expect(bob.errors.map((e) => e.type)).toEqual(['sync']);
        expect(bob.errors[0].message).toContain(FILE_DOC);

        // The owner pastes bob's id in. No restart, no new client.
        alice.client.setAllowedJoiners(['bob-live']);

        // Bob retries; the emptied slot is what lets establishGroup re-bootstrap.
        bob.client.disconnect();
        await bob.client.connect();
        await wait(500);

        expect(handshakesFrom('bob-live', 'key_package')).toHaveLength(2);
        expect(handshakesFrom('alice-live', 'welcome')).toHaveLength(1);
        expect(groupOf(bob.client)).not.toBeNull();

        // Membership, not just a Welcome frame: bob decrypts alice's content.
        const secret = 'readable once the owner said yes, without restarting';
        expect(alice.client.sendUpdate(secret)).toBe(true);
        await wait(300);
        expect(bob.updates).toContain(secret);
        expect(groupOf(alice.client)?.epoch).toBe(1n);
    });

    it('admits an identity ONCE, even when its Welcome is lost and it retries', async () => {
        // The join deadline lets a joiner whose key package went unanswered free
        // it and re-send on its next connect. That retry makes DOUBLE admission
        // reachable: if the Welcome is lost in transit the owner has ALREADY
        // added a leaf for that identity, and answering the retry adds a second
        // one. MLS puts no uniqueness constraint on credential identities, so
        // both leaves carry "bob" — and `remove_member` resolves exactly ONE per
        // identity (crates/collab-core/src/mls.rs, `find_member_leaf`), so a
        // later revocation (#31) would leave the other one in the group, still
        // decrypting. The owner therefore admits an identity exactly once.
        const JOIN_TIMEOUT_MS = 150;
        const alice = makeClient('alice-dup', 'owner', ['bob-dup', 'carol-dup']);
        const bob = makeClient('bob-dup', 'joiner', undefined, {
            joinTimeoutMs: JOIN_TIMEOUT_MS,
        });
        // The relay swallows Welcomes; every other frame still routes, so the
        // owner's side of the handshake completes exactly as it would in the
        // relay-blip case this reproduces.
        relay.withhold = (msg) => msg.message_type === 'welcome';

        await alice.client.connect();
        await wait(50);
        await bob.client.connect();
        await wait(400);

        // Admitted once: the owner minted a Welcome that never arrived, and the
        // add-commit already advanced the group — the leaf exists.
        expect(handshakesFrom('alice-dup', 'welcome')).toHaveLength(1);
        expect(groupOf(bob.client)).toBeNull();
        expect(groupOf(alice.client)?.epoch).toBe(1n);

        // The deadline freed the un-answered key package, so the next connect
        // sends a fresh one — the retry that opens the hole.
        expect(pendingOf(bob.client)).toBeNull();
        bob.client.disconnect();
        await bob.client.connect();
        await wait(500);

        // The retry really reached the owner and the owner really had a group to
        // extend, so the refusal below is a decision, not a missing handshake.
        expect(handshakesFrom('bob-dup', 'key_package')).toHaveLength(2);
        expect(groupOf(alice.client)).not.toBeNull();
        // ...and it was refused: no second Welcome, and no second commit — the
        // group is still at the epoch bob's FIRST admission put it at, so there
        // is exactly one leaf for "bob".
        expect(handshakesFrom('alice-dup', 'welcome')).toHaveLength(1);
        expect(handshakesFrom('alice-dup', 'commit')).toHaveLength(1);
        expect(groupOf(alice.client)?.epoch).toBe(1n);

        // Park bob before the discriminator below. `disconnect()` clears the
        // retry budget, and it has to be explicit: the close of the socket bob
        // disconnected above is delivered AFTER the connect() that replaced it,
        // and that stale onclose schedules a reconnect against a client that is
        // already connected — one that fires 1s later, opens a third connection
        // and sends a third key package. That is a connection-lifecycle bug in
        // its own right (reported separately); this test must measure the
        // admission gate, not race it.
        bob.client.disconnect();

        // The discriminator, and the positive half of bob's silence: the owner
        // did not simply stop answering key packages once it had a member. A
        // DIFFERENT allowlisted id still joins and still decrypts, so the
        // refusal is per-identity rather than a gate that fell shut.
        relay.withhold = null;
        const carol = makeClient('carol-dup', 'joiner');
        await carol.client.connect();
        await wait(400);

        expect(handshakesFrom('alice-dup', 'welcome')).toHaveLength(2);
        expect(groupOf(carol.client)).not.toBeNull();
        expect(groupOf(alice.client)?.epoch).toBe(2n);

        const secret = 'readable by a member the owner admitted exactly once';
        expect(alice.client.sendUpdate(secret)).toBe(true);
        await wait(300);
        expect(carol.updates).toContain(secret);
    }, 15000);

    it('setAllowedJoiners keeps a defensive copy and still refuses an unlisted id', async () => {
        // Fail-closed regression: the live setter must not widen the gate, and
        // must not retain the caller's array — a later push() by the settings
        // tab would otherwise admit whoever it appended.
        const ids = ['bob-copy'];
        const alice = makeClient('alice-copy', 'owner', []);
        alice.client.setAllowedJoiners(ids);
        ids.push('mallory-copy');

        const mallory = makeClient('mallory-copy', 'joiner');
        await alice.client.connect();
        await wait(50);
        await mallory.client.connect();
        await wait(400);

        expect(handshakesFrom('mallory-copy', 'key_package')).toHaveLength(1);
        expect(handshakesFrom('alice-copy', 'welcome')).toHaveLength(0);
        expect(groupOf(mallory.client)).toBeNull();
        expect(groupOf(alice.client)?.epoch).toBe(0n);
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
