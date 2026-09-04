import { jest, describe, it, expect, beforeEach, afterEach } from '@jest/globals';
import type { CollabClientConfig } from '../collab-client';

// Subscribe authorization (#72): every subscribe for a document whose MLS group
// exists must carry a freshly minted capability, because a bare Subscribe
// DOWNGRADES an existing authorization back to handshake-only at the relay.
// These tests observe the wire frames the client emits, which is the only thing
// the relay judges.

const sockets: MockWebSocket[] = [];

// Sockets that open on the next timer tick and stay open until a test closes
// them explicitly. Every construction is recorded so a reconnect test can read
// the frames of the SECOND socket.
class MockWebSocket {
    static OPEN = 1;
    static CONNECTING = 0;
    static CLOSING = 2;
    static CLOSED = 3;

    readyState = MockWebSocket.CONNECTING;
    onopen: (() => void) | null = null;
    onmessage: ((event: { data: string }) => void) | null = null;
    onerror: ((error: unknown) => void) | null = null;
    onclose: (() => void) | null = null;
    sentMessages: string[] = [];

    constructor() {
        sockets.push(this);
        setTimeout(() => {
            this.readyState = MockWebSocket.OPEN;
            this.onopen?.();
        }, 0);
    }

    send(data: string): void {
        this.sentMessages.push(data);
    }

    close(): void {
        this.readyState = MockWebSocket.CLOSED;
    }

    /** Drop the connection the way a relay restart would. */
    drop(): void {
        this.readyState = MockWebSocket.CLOSED;
        this.onclose?.();
    }

    simulateMessage(data: object): void {
        this.onmessage?.({ data: JSON.stringify(data) });
    }
}

// @ts-expect-error - Override global WebSocket
global.WebSocket = MockWebSocket;

// The capability shape the relay deserializes into
// `Option<SubscribeCapability>` (crates/collab-proto/src/capability.rs). The
// mock mints a JSON string exactly like the wasm binding does, echoing the
// doc_id it was minted for so a test can tell WHICH group minted it.
const MOCK_EPOCH = 3n;
const makeMockDoc = (docId: string) => ({
    docId,
    get_content: jest.fn<() => string>().mockReturnValue(''),
    insert: jest.fn(),
    delete: jest.fn(),
    get_encrypted_update: jest
        .fn<() => { ciphertext: Uint8Array; epoch: bigint }>()
        .mockReturnValue({ ciphertext: new Uint8Array([1, 2, 3]), epoch: MOCK_EPOCH }),
    apply_encrypted_update: jest.fn(),
    encrypt_bytes: jest
        .fn<() => { ciphertext: Uint8Array; epoch: bigint }>()
        .mockReturnValue({ ciphertext: new Uint8Array([4, 5]), epoch: MOCK_EPOCH }),
    decrypt_bytes: jest.fn<() => Uint8Array>().mockReturnValue(new Uint8Array([6])),
    create_invite: jest.fn(() => ({
        welcome: new Uint8Array([9, 9]),
        // The commit EXISTING members must process to follow the new epoch.
        commit: new Uint8Array([10, 10]),
        // A real create_invite advances the epoch and emits the anchor rotation
        // for the epoch it just created.
        rotation: {
            epoch: 4n,
            public_key: new Uint8Array([11, 11]),
            proof: new Uint8Array([12]),
            rotation_proof: new Uint8Array([13]),
        },
    })),
    process_commit: jest.fn(),
    epoch: MOCK_EPOCH,
    mint_subscribe_capability: jest.fn(
        (user_id: string, capDocId: string, _now: bigint, _ttl: bigint) =>
            JSON.stringify({
                user_id,
                doc_id: capDocId,
                epoch: Number(MOCK_EPOCH),
                expiry_unix: 1_000_000,
                signature: [1, 2, 3],
                // Not a wire field: proves WHICH group's doc minted this.
                minted_by: docId,
            })
    ),
    sign_doc_key_proof: jest.fn<() => Uint8Array>().mockReturnValue(new Uint8Array([7, 7])),
    subscribe_verifying_key: jest.fn<() => Uint8Array>().mockReturnValue(new Uint8Array([8, 8])),
    free: jest.fn(),
});

type MockDoc = ReturnType<typeof makeMockDoc>;
const createdDocs: MockDoc[] = [];
const joinedDocs: MockDoc[] = [];

jest.unstable_mockModule('../wasm/collab_wasm', () => ({
    __esModule: true,
    WasmEncryptedDocument: {
        create: jest.fn((docId: string) => {
            const doc = makeMockDoc(docId);
            createdDocs.push(doc);
            return doc;
        }),
        join: jest.fn((invite: { doc_id?: string }) => {
            const doc = makeMockDoc(invite.doc_id ?? 'joined');
            joinedDocs.push(doc);
            return doc;
        }),
    },
    WasmInvite: {
        from_welcome: jest.fn((docId: string) => ({ doc_id: docId, welcome: new Uint8Array() })),
    },
    generate_key_package: jest.fn(() => ({
        key_package: new Uint8Array([7, 7, 7]),
        free: jest.fn(),
    })),
}));

const { WasmEncryptedDocument } = await import('../wasm/collab_wasm');
const { CollabClient } = await import('../collab-client');
type CollabClient = InstanceType<typeof CollabClient>;

interface SubscribeFrame {
    type: string;
    doc_id: string;
    capability?: { user_id: string; doc_id: string; epoch: number; minted_by?: string };
}

function frames(socket: MockWebSocket): { type: string; [k: string]: unknown }[] {
    return socket.sentMessages.map((m) => JSON.parse(m));
}

function subscribes(socket: MockWebSocket, docId?: string): SubscribeFrame[] {
    return (frames(socket) as unknown as SubscribeFrame[]).filter(
        (f) => f.type === 'subscribe' && (docId === undefined || f.doc_id === docId)
    );
}

function makeConfig(overrides: Partial<CollabClientConfig> = {}): CollabClientConfig {
    return {
        relayUrl: 'ws://localhost:8080',
        userId: 'user1',
        docId: 'doc1',
        role: 'owner',
        ...overrides,
    };
}

async function connectClient(client: CollabClient): Promise<void> {
    const promise = client.connect();
    jest.runAllTimers();
    await promise;
}

describe('subscribe capability (#72)', () => {
    let client: CollabClient | null = null;

    beforeEach(() => {
        jest.useFakeTimers();
        sockets.length = 0;
        createdDocs.length = 0;
        joinedDocs.length = 0;
    });

    afterEach(() => {
        client?.disconnect();
        client = null;
        jest.useRealTimers();
    });

    it('presents a capability once the owner group exists', async () => {
        client = new CollabClient(makeConfig());
        await connectClient(client);

        const presented = subscribes(sockets[0], 'doc1').filter((f) => f.capability);
        expect(presented).toHaveLength(1);
        expect(presented[0].capability).toMatchObject({
            user_id: 'user1',
            doc_id: 'doc1',
            minted_by: 'doc1',
        });
        // Bound to the LOCALLY-trusted doc id, at the group's current epoch.
        expect(createdDocs[0].mint_subscribe_capability).toHaveBeenCalledWith(
            'user1',
            'doc1',
            expect.any(BigInt),
            expect.any(BigInt)
        );
    });

    it('registers the doc anchor as owner so a capability can verify', async () => {
        client = new CollabClient(makeConfig());
        await connectClient(client);

        const registrations = frames(sockets[0]).filter((f) => f.type === 'register_doc_key');
        expect(registrations).toHaveLength(1);
        expect(registrations[0]).toMatchObject({
            doc_id: 'doc1',
            epoch: Number(MOCK_EPOCH),
            public_key: [8, 8],
            proof: [7, 7],
            // First registration is TOFU: no continuity proof exists yet.
            rotation_proof: [],
        });
    });

    it('re-bootstraps a usable group after the first anchor registration throws', async () => {
        // registerAnchor() runs AFTER slot.setDoc(), and sign_doc_key_proof is a
        // wasm-bindgen Result<T, JsError> call away from throwing. A partial
        // bootstrap must leave NOTHING behind: a doc that survives with
        // groupEstablished still set makes every later connect skip bootstrap, so
        // register_doc_key is NEVER sent while the client keeps presenting
        // capabilities for it. The relay answers "no subscribe anchor registered"
        // and the subscription dies, but the client resolves, arms its stability
        // timer and refills its retry budget — silently deaf forever.
        (WasmEncryptedDocument.create as unknown as jest.Mock).mockImplementationOnce(
            (...args: unknown[]) => {
                const doc = makeMockDoc(String(args[0]));
                doc.sign_doc_key_proof.mockImplementationOnce(() => {
                    throw new Error('anchor proof failed');
                });
                createdDocs.push(doc);
                return doc;
            }
        );

        client = new CollabClient(makeConfig());
        const failed = client.connect();
        jest.runAllTimers();
        await expect(failed).rejects.toThrow('anchor proof failed');

        await connectClient(client);

        // The retry must produce a WORKING group, not merely a resolved promise:
        // a fresh doc whose anchor actually reached the relay, with the presented
        // capability minted by that same fresh doc.
        expect(createdDocs).toHaveLength(2);
        expect(createdDocs[0].free).toHaveBeenCalled();
        expect(createdDocs[0].mint_subscribe_capability).not.toHaveBeenCalled();

        const retryFrames = frames(sockets[1]);
        expect(retryFrames.filter((f) => f.type === 'register_doc_key')).toHaveLength(1);
        const presented = subscribes(sockets[1], 'doc1').filter((f) => f.capability);
        expect(presented).toHaveLength(1);
        expect(createdDocs[1].mint_subscribe_capability).toHaveBeenCalled();
        // The anchor must be registered BEFORE the capability that verifies
        // against it is presented.
        const anchorIdx = retryFrames.findIndex((f) => f.type === 'register_doc_key');
        const capabilityIdx = retryFrames.findIndex(
            (f) => f.type === 'subscribe' && f.capability !== undefined
        );
        expect(capabilityIdx).toBeGreaterThan(anchorIdx);
    });

    it('keeps the anchored group when minting throws after register_doc_key went out', async () => {
        // The mirror image of the test above. Once register_doc_key is on the
        // wire the relay holds an anchor for this document, and it rejects a
        // second TOFU registration ("anchor rotation continuity proof
        // verification failed", relay.rs). So a throw from the LAST bootstrap
        // step — minting — must fail the connect WITHOUT discarding the group: a
        // retry with a fresh doc could never re-register, and every capability it
        // minted would verify against the wrong key.
        (WasmEncryptedDocument.create as unknown as jest.Mock).mockImplementationOnce(
            (...args: unknown[]) => {
                const doc = makeMockDoc(String(args[0]));
                doc.mint_subscribe_capability.mockImplementationOnce(() => {
                    throw new Error('mint exploded');
                });
                createdDocs.push(doc);
                return doc;
            }
        );

        client = new CollabClient(makeConfig());
        const failed = client.connect();
        jest.runAllTimers();
        await expect(failed).rejects.toThrow('mint exploded');

        // The anchor DID reach the relay, so the group must survive intact.
        expect(frames(sockets[0]).filter((f) => f.type === 'register_doc_key')).toHaveLength(1);
        expect(createdDocs[0].free).not.toHaveBeenCalled();

        await connectClient(client);

        // The retry reuses that same group: no second doc, no second TOFU
        // registration, and the capability comes from the anchored doc.
        expect(createdDocs).toHaveLength(1);
        expect(frames(sockets[1]).filter((f) => f.type === 'register_doc_key')).toHaveLength(0);
        const presented = subscribes(sockets[1], 'doc1').filter((f) => f.capability);
        expect(presented).toHaveLength(1);
    });

    it('subscribes capability-less first, then re-presents after joining', async () => {
        client = new CollabClient(makeConfig({ role: 'joiner' }));
        await connectClient(client);

        // A joiner MUST be subscribed to receive the Welcome, and cannot mint
        // before it is a member — so the first subscribe carries nothing.
        const beforeJoin = subscribes(sockets[0], 'doc1');
        expect(beforeJoin).toHaveLength(1);
        expect(beforeJoin[0].capability).toBeUndefined();

        sockets[0].onmessage?.({
            data: JSON.stringify({
                type: 'mls_handshake',
                doc_id: 'doc1',
                payload: [1, 2, 3],
                message_type: 'welcome',
            }),
        });

        const afterJoin = subscribes(sockets[0], 'doc1');
        expect(afterJoin).toHaveLength(2);
        expect(afterJoin[1].capability).toMatchObject({ user_id: 'user1', doc_id: 'doc1' });
        expect(joinedDocs).toHaveLength(1);
    });

    it('re-presents a capability on reconnect instead of a bare subscribe', async () => {
        client = new CollabClient(makeConfig({ maxReconnectAttempts: 3 }));
        await connectClient(client);
        expect(sockets).toHaveLength(1);

        sockets[0].drop();
        jest.runAllTimers();
        await Promise.resolve();
        jest.runAllTimers();

        expect(sockets.length).toBeGreaterThan(1);
        const reconnected = subscribes(sockets[1], 'doc1');
        expect(reconnected).toHaveLength(1);
        // A bare Subscribe here would DOWNGRADE the relay-side authorization to
        // handshake-only and silently stop all content.
        expect(reconnected[0].capability).toMatchObject({ user_id: 'user1', doc_id: 'doc1' });
    });

    it('resumes the owner group across an explicit disconnect() -> connect()', async () => {
        // MLS group state is long-lived and outlives the socket. An explicit
        // disconnect() used to free it, so the next connect() built a FRESH
        // epoch-0 group: divergent from every other member, and announced with a
        // TOFU register_doc_key for a document the relay already anchors — which
        // it rejects at the rotation-continuity check, followed by an epoch-0
        // capability it rejects as Unauthorized. Same-process resume, not
        // re-creation.
        client = new CollabClient(makeConfig());
        await connectClient(client);
        expect(createdDocs).toHaveLength(1);

        client.disconnect();
        await connectClient(client);

        expect(sockets).toHaveLength(2);
        expect(createdDocs).toHaveLength(1);
    });

    it('sends no second TOFU registration after an explicit disconnect() -> connect()', async () => {
        // The relay's anchor for this document already exists, and a second
        // registration with an EMPTY rotation_proof fails the continuity check
        // (crates/collab-relay/src/relay.rs, `handle_register_doc_key`). Exactly
        // one registration is correct for the lifetime of the group.
        client = new CollabClient(makeConfig());
        await connectClient(client);

        client.disconnect();
        await connectClient(client);

        const registrations = sockets.flatMap((s) =>
            frames(s).filter((f) => f.type === 'register_doc_key')
        );
        expect(registrations).toHaveLength(1);
    });

    it('re-presents the surviving group capability after an explicit reconnect', async () => {
        // Content has to flow again after the restart, which under subscribe
        // authorization means the second socket's subscribe carries a capability
        // minted by the SAME group — the one the relay's anchor still names.
        client = new CollabClient(makeConfig());
        await connectClient(client);

        client.disconnect();
        await connectClient(client);

        const restarted = subscribes(sockets[1], 'doc1');
        expect(restarted).toHaveLength(1);
        expect(restarted[0].capability).toMatchObject({ user_id: 'user1', doc_id: 'doc1' });
        expect(createdDocs[0].mint_subscribe_capability).toHaveBeenCalledTimes(2);
    });

    it('settles the connect attempt when minting throws on reconnect', async () => {
        const errors: { type: string; message: string }[] = [];
        client = new CollabClient(makeConfig({ maxReconnectAttempts: 3 }));
        client.onError((e) => errors.push(e));
        await connectClient(client);

        // Minting is a wasm-bindgen call that can throw. A throw escaping onopen
        // would leave connectPromise unsettled forever and deadlock the reconnect
        // loop (CLAUDE.md: every connect attempt settles exactly once).
        createdDocs[0].mint_subscribe_capability.mockImplementation(() => {
            throw new Error('mint exploded');
        });

        sockets[0].drop();
        jest.runAllTimers();
        await Promise.resolve();
        jest.runAllTimers();
        // Drain the microtask chain the rejection travels: reject -> .finally
        // (clears connectPromise) -> the reconnect loop's .catch -> onError.
        for (let i = 0; i < 10; i++) {
            await Promise.resolve();
        }

        expect(sockets).toHaveLength(2);
        // The attempt failed BEFORE any subscribe went out, and it settled: the
        // rejection surfaced as a connection error instead of hanging.
        expect(subscribes(sockets[1])).toHaveLength(0);
        expect(sockets[1].readyState).toBe(3);
        expect(errors).toContainEqual(
            expect.objectContaining({ type: 'connection', message: 'mint exploded' })
        );
    });

    it('mints the manifest capability from the manifest group, not the file group', async () => {
        client = new CollabClient(
            makeConfig({
                vaultSync: { apply_remote_manifest: jest.fn(() => []) },
                manifestDocId: 'manifest1',
            })
        );
        await connectClient(client);

        const presented = subscribes(sockets[0], 'manifest1').filter((f) => f.capability);
        expect(presented).toHaveLength(1);
        // The manifest rides its OWN MLS group: its capability must be minted by
        // that group's document, never by the file group's.
        expect(presented[0].capability).toMatchObject({
            doc_id: 'manifest1',
            minted_by: 'manifest1',
        });

        const [fileDoc, manifestDoc] = createdDocs;
        expect(manifestDoc.mint_subscribe_capability).toHaveBeenCalledWith(
            'user1',
            'manifest1',
            expect.any(BigInt),
            expect.any(BigInt)
        );
        expect(fileDoc.mint_subscribe_capability).not.toHaveBeenCalledWith(
            'user1',
            'manifest1',
            expect.any(BigInt),
            expect.any(BigInt)
        );
    });

    it('rotates the anchor and re-presents when inviting a member', async () => {
        client = new CollabClient(makeConfig());
        await connectClient(client);
        const before = frames(sockets[0]).length;

        sockets[0].simulateMessage({
            type: 'mls_handshake',
            doc_id: 'doc1',
            payload: [1, 2, 3],
            message_type: 'key_package',
        });

        const emitted = frames(sockets[0]).slice(before);
        // create_invite advanced the epoch, so the relay's anchor must move with
        // it BEFORE the joiner presents an epoch-4 capability — and this client
        // must re-present its OWN capability at the new epoch before the Welcome
        // lets the joiner start sending, or it is unauthorized in the window
        // between the rotation and its own re-subscribe.
        // The commit precedes the Welcome: the relay fans handshake frames out
        // to every other subscriber in order, and the new member only no-ops the
        // commit while it still has no group of its own.
        expect(emitted.map((f) => f.type)).toEqual([
            'register_doc_key',
            'subscribe',
            'mls_handshake',
            'mls_handshake',
        ]);
        expect(emitted[2]).toMatchObject({ message_type: 'commit', payload: [10, 10] });
        expect(emitted[3]).toMatchObject({ message_type: 'welcome', payload: [9, 9] });
        expect(emitted[0]).toMatchObject({
            doc_id: 'doc1',
            epoch: 4,
            public_key: [11, 11],
            proof: [12],
            rotation_proof: [13],
        });
        expect((emitted[1] as unknown as SubscribeFrame).capability).toBeDefined();
    });

    it('CHARACTERIZATION: manifest-discovered paths stay capability-less (no group yet)', async () => {
        const vaultSync = { apply_remote_manifest: jest.fn(() => ['notes/new.md']) };
        client = new CollabClient(makeConfig({ vaultSync, manifestDocId: 'manifest1' }));
        await connectClient(client);

        sockets[0].simulateMessage({
            type: 'yrs_update',
            doc_id: 'manifest1',
            encrypted: [1, 2],
            epoch: 3,
        });

        const discovered = subscribes(sockets[0], 'notes/new.md');
        expect(discovered).toHaveLength(1);
        // Honest limitation: no MLS group exists for a newly-announced path, so
        // there is nothing to mint from. Handshake-only until one is established.
        expect(discovered[0].capability).toBeUndefined();
    });
});
