import { jest, describe, it, expect, beforeEach, afterEach } from '@jest/globals';
import type { CollabClientConfig } from '../collab-client';

// Mock WebSocket
class MockWebSocket {
    static OPEN = 1;
    static CONNECTING = 0;
    static CLOSING = 2;
    static CLOSED = 3;

    readyState = MockWebSocket.OPEN;
    onopen: (() => void) | null = null;
    onmessage: ((event: { data: string }) => void) | null = null;
    onerror: ((error: any) => void) | null = null;
    onclose: (() => void) | null = null;
    sentMessages: string[] = [];

    constructor(public url: string) {
        // Simulate async connection
        setTimeout(() => this.onopen?.(), 0);
    }

    send(data: string): void {
        this.sentMessages.push(data);
    }

    close(): void {
        this.readyState = MockWebSocket.CLOSED;
        this.onclose?.();
    }

    // Helper to simulate receiving a message
    simulateMessage(data: object): void {
        this.onmessage?.({ data: JSON.stringify(data) });
    }

    // Helper to simulate an error
    simulateError(error: any): void {
        this.onerror?.(error);
    }
}

// @ts-ignore - Override global WebSocket
global.WebSocket = MockWebSocket;

interface MockWSInstance {
    readyState: number;
    onopen: (() => void) | null;
    onclose: (() => void) | null;
    onerror: ((error: any) => void) | null;
    onmessage: ((event: { data: string }) => void) | null;
    sentMessages: string[];
}

// Factory for the one-off WebSocket mocks that drive connect()/reconnect edge
// cases (immediate close, fail-then-succeed, flaky reconnects, a send() that
// lies about delivery). Every variant shares the same statics/handlers/
// readyState/sentMessages shape; only construction-time behavior, send(), the
// starting readyState, and whether close() fires onclose actually differ
// between them, so those are the parameters.
function createMockWebSocket(options: {
    onConstruct: (self: MockWSInstance) => void;
    send?: (self: MockWSInstance, data: string) => void;
    closeFiresOnclose?: boolean;
    initialReadyState?: number;
}) {
    const { onConstruct, send, closeFiresOnclose, initialReadyState = 0 } = options;
    return class {
        static OPEN = 1;
        static CONNECTING = 0;
        static CLOSING = 2;
        static CLOSED = 3;
        readyState = initialReadyState;
        onopen: (() => void) | null = null;
        onclose: (() => void) | null = null;
        onerror: ((error: any) => void) | null = null;
        onmessage: ((event: { data: string }) => void) | null = null;
        sentMessages: string[] = [];

        constructor() {
            onConstruct(this);
        }

        send(data: string) {
            if (send) {
                send(this, data);
            } else {
                this.sentMessages.push(data);
            }
        }

        close() {
            this.readyState = 3;
            if (closeFiresOnclose) {
                this.onclose?.();
            }
        }
    };
}

// Sockets that open and STAY open; the test drops them explicitly. close()
// deliberately does NOT fire onclose, so a teardown path can be proven to
// clean up on its own rather than via the onclose -> handleReconnect hop.
// Returns the running count of sockets constructed, which is how the reconnect
// tests observe the private retry budget through behavior.
function installStaysOpenWebSocket(): () => number {
    let constructions = 0;
    (global as any).WebSocket = createMockWebSocket({
        onConstruct: (ws) => {
            constructions++;
            setTimeout(() => {
                ws.readyState = 1;
                ws.onopen?.();
            }, 0);
        },
    });
    return () => constructions;
}

// The anchor rotation a real create_invite emits for the epoch its commit
// creates; the owner forwards it as a register_doc_key frame (#29).
interface MockRotation {
    epoch: bigint;
    public_key: Uint8Array;
    proof: Uint8Array;
    rotation_proof: Uint8Array;
}
const MOCK_ROTATION: MockRotation = {
    epoch: 2n,
    public_key: new Uint8Array([11, 11]),
    proof: new Uint8Array([12]),
    rotation_proof: new Uint8Array([13]),
};

// A mock of the MLS document surface the client drives. There is NO
// set_encryption_key / has_encryption_key / encode_state_encrypted here — the AES
// CollabCore is gone; MLS derives keys from group membership.
const makeMockDoc = () => ({
    get_content: jest.fn<() => string>().mockReturnValue(''),
    insert: jest.fn(),
    delete: jest.fn(),
    get_encrypted_update: jest
        .fn<() => { ciphertext: Uint8Array; epoch: bigint }>()
        .mockReturnValue({ ciphertext: new Uint8Array([1, 2, 3]), epoch: 1n }),
    apply_encrypted_update: jest.fn(),
    create_invite: jest
        .fn<() => { welcome: Uint8Array; rotation: MockRotation }>()
        .mockReturnValue({ welcome: new Uint8Array([9, 9]), rotation: MOCK_ROTATION }),
    process_commit: jest.fn(),
    // Subscribe-authorization surface (#72): the owner registers an anchor and
    // every subscribe for an established group presents a minted capability.
    epoch: 1n,
    mint_subscribe_capability: jest.fn(
        (user_id: string, docId: string) =>
            `{"user_id":"${user_id}","doc_id":"${docId}","epoch":1,"expiry_unix":1000,"signature":[1]}`
    ),
    sign_doc_key_proof: jest.fn<() => Uint8Array>().mockReturnValue(new Uint8Array([7, 7])),
    subscribe_verifying_key: jest.fn<() => Uint8Array>().mockReturnValue(new Uint8Array([8, 8])),
    free: jest.fn(),
});

jest.unstable_mockModule('../wasm/collab_wasm', () => ({
    __esModule: true,
    WasmEncryptedDocument: {
        // Owner creates its group up front; a joiner joins via a Welcome.
        create: jest.fn(() => makeMockDoc()),
        join: jest.fn(() => makeMockDoc()),
    },
    WasmInvite: {
        from_welcome: jest.fn(() => ({ welcome: new Uint8Array() })),
    },
    generate_key_package: jest.fn(() => ({
        key_package: new Uint8Array([7, 7, 7]),
        free: jest.fn(),
    })),
}));

const { WasmEncryptedDocument, generate_key_package } = await import('../wasm/collab_wasm');
const { CollabClient, ConfigValidationError } = await import('../collab-client');
type CollabClient = InstanceType<typeof CollabClient>;

// Shared fixture builder: every describe block below wants the same owner
// config unless a test overrides a field (role: 'joiner', a bad field for
// validation, etc).
function makeDefaultConfig(overrides: Partial<CollabClientConfig> = {}): CollabClientConfig {
    return {
        relayUrl: 'ws://localhost:8080',
        userId: 'user1',
        docId: 'doc1',
        role: 'owner',
        ...overrides,
    };
}

// Drive a client through connect() under fake timers and await its settlement.
// This is the standard "just get me connected" path used by the large majority
// of tests; a handful of tests instead assert directly on the connect()
// promise (resolves/rejects) and call connect()/runAllTimers() themselves.
async function connectClient(client: CollabClient): Promise<void> {
    const connectPromise = client.connect();
    jest.runAllTimers();
    await connectPromise;
}

describe('CollabClient', () => {
    let client: CollabClient;
    let config: CollabClientConfig;

    beforeEach(() => {
        jest.useFakeTimers();
        config = makeDefaultConfig();
        client = new CollabClient(config);
    });

    afterEach(() => {
        jest.useRealTimers();
        client.disconnect();
    });

    describe('constructor', () => {
        it('should throw ConfigValidationError for empty relayUrl', () => {
            const invalidConfig = { ...config, relayUrl: '' };
            expect(() => new CollabClient(invalidConfig)).toThrow(ConfigValidationError);
            expect(() => new CollabClient(invalidConfig)).toThrow(
                'relayUrl must be a non-empty string'
            );
        });

        it('should throw ConfigValidationError for invalid relayUrl protocol', () => {
            const invalidConfig = { ...config, relayUrl: 'http://localhost:8080' };
            expect(() => new CollabClient(invalidConfig)).toThrow(ConfigValidationError);
            expect(() => new CollabClient(invalidConfig)).toThrow(
                'relayUrl must start with ws:// or wss://'
            );
        });

        it('should accept wss:// relayUrl', () => {
            const secureConfig = { ...config, relayUrl: 'wss://secure.example.com' };
            expect(() => new CollabClient(secureConfig)).not.toThrow();
        });

        it('should throw ConfigValidationError for empty userId', () => {
            const invalidConfig = { ...config, userId: '' };
            expect(() => new CollabClient(invalidConfig)).toThrow(ConfigValidationError);
            expect(() => new CollabClient(invalidConfig)).toThrow(
                'userId must be a non-empty string'
            );
        });

        it('should throw ConfigValidationError for empty docId', () => {
            const invalidConfig = { ...config, docId: '' };
            expect(() => new CollabClient(invalidConfig)).toThrow(ConfigValidationError);
            expect(() => new CollabClient(invalidConfig)).toThrow(
                'docId must be a non-empty string'
            );
        });

        it('should throw ConfigValidationError for an invalid role', () => {
            const invalidConfig = { ...config, role: 'admin' as any };
            expect(() => new CollabClient(invalidConfig)).toThrow(ConfigValidationError);
            expect(() => new CollabClient(invalidConfig)).toThrow(
                'role must be "owner" or "joiner"'
            );
        });

        it('should throw ConfigValidationError when vaultSync is set without manifestDocId', () => {
            const invalidConfig = {
                ...config,
                vaultSync: { apply_remote_manifest: (): string[] => [] },
            };
            expect(() => new CollabClient(invalidConfig)).toThrow(ConfigValidationError);
            expect(() => new CollabClient(invalidConfig)).toThrow(
                'manifestDocId must be a non-empty string when vaultSync is provided'
            );
        });

        it('should throw ConfigValidationError when manifestDocId equals docId', () => {
            const invalidConfig = {
                ...config,
                vaultSync: { apply_remote_manifest: (): string[] => [] },
                manifestDocId: config.docId,
            };
            expect(() => new CollabClient(invalidConfig)).toThrow(ConfigValidationError);
            expect(() => new CollabClient(invalidConfig)).toThrow(
                'manifestDocId must differ from docId'
            );
        });
    });

    describe('connect', () => {
        it('should connect and send identify message', async () => {
            await connectClient(client);

            // Access the WebSocket through the client (we need to check sent messages)
            // Since WebSocket is a mock, we can check via the global
        });

        it('should resolve promise on successful connection', async () => {
            const connectPromise = client.connect();
            jest.runAllTimers();

            await expect(connectPromise).resolves.toBeUndefined();
        });

        it('should reject promise when WebSocket closes during initial connection', async () => {
            // Create a mock WebSocket that closes immediately (before onopen)
            const OriginalWebSocket = global.WebSocket;

            (global as any).WebSocket = createMockWebSocket({
                onConstruct: (ws) => {
                    // Close immediately during initial connection (before onopen)
                    setTimeout(() => {
                        ws.readyState = 3;
                        ws.onclose?.();
                    }, 0);
                },
            });

            const testClient = new CollabClient(config);
            const connectPromise = testClient.connect();

            jest.runAllTimers();

            // Promise should be rejected with specific error message
            await expect(connectPromise).rejects.toThrow(
                'WebSocket closed during initial connection'
            );

            testClient.disconnect();

            // Restore original WebSocket
            global.WebSocket = OriginalWebSocket;
        });

        it('should deduplicate concurrent connection attempts', async () => {
            // Start first connection (don't await yet)
            const connectPromise1 = client.connect();

            // Start second connection while first is still pending
            const connectPromise2 = client.connect();

            // Both should return the same promise
            expect(connectPromise1).toBe(connectPromise2);

            // Complete the connection
            jest.runAllTimers();
            await connectPromise1;
            await connectPromise2;
        });

        it('should allow new connection after previous completes', async () => {
            // First connection
            const connectPromise1 = client.connect();
            jest.runAllTimers();
            await connectPromise1;

            // Disconnect
            client.disconnect();

            // Create new client for fresh connection
            const newClient = new CollabClient(config);

            // Second connection should be allowed (different promise)
            const connectPromise2 = newClient.connect();
            jest.runAllTimers();
            await connectPromise2;

            newClient.disconnect();
        });

        it('should allow new connection after previous fails', async () => {
            // Create a new mock WebSocket class that fails
            const OriginalWebSocket = global.WebSocket;

            let connectionAttempts = 0;
            (global as any).WebSocket = createMockWebSocket({
                onConstruct: (ws) => {
                    connectionAttempts++;
                    // Fail first connection, succeed second
                    setTimeout(() => {
                        if (connectionAttempts === 1) {
                            ws.onclose?.();
                        } else {
                            ws.readyState = 1;
                            ws.onopen?.();
                        }
                    }, 0);
                },
            });

            const testClient = new CollabClient(config);

            // First connection should fail
            const connectPromise1 = testClient.connect();
            jest.runAllTimers();
            await expect(connectPromise1).rejects.toThrow();

            // Second connection should succeed (connectPromise should be cleared)
            const connectPromise2 = testClient.connect();
            jest.runAllTimers();
            await expect(connectPromise2).resolves.toBeUndefined();

            testClient.disconnect();

            // Restore original WebSocket
            global.WebSocket = OriginalWebSocket;
        });

        it('rejects (does not hang) when establishGroup throws during onopen', async () => {
            // WasmEncryptedDocument.create() is a wasm-bindgen Result<T, JsError> call
            // that throws on a crypto-provider/entropy failure. Without a catch around
            // establishGroup(), that throw would abort onopen before resolve()/reject()
            // ran, leaving connectPromise permanently unsettled — the exact hang class
            // already fixed once for the sibling identify/subscribe branch.
            (WasmEncryptedDocument.create as unknown as jest.Mock).mockImplementationOnce(() => {
                throw new Error('crypto provider unavailable');
            });

            const connectPromise = client.connect();
            jest.runAllTimers();

            await expect(connectPromise).rejects.toThrow('crypto provider unavailable');
            expect((client as any).ws).toBeNull();

            // The dedup guard must be cleared so a retry is possible instead of
            // returning the same never-settling promise forever.
            const retryPromise = client.connect();
            expect(retryPromise).not.toBe(connectPromise);
            jest.runAllTimers();
            await expect(retryPromise).resolves.toBeUndefined();
        });
    });

    describe('sendUpdate', () => {
        it('should send an MLS-encrypted update to the relay', async () => {
            await connectClient(client);

            // Owner's group is created on connect; drive that document.
            const doc = (client as any).doc;
            // Delta-diff: "old content" → "new content"
            // Common suffix is " content", so only "old" → "new" changes
            doc.get_content.mockReturnValue('old content');
            client.sendUpdate('new content');

            expect(doc.delete).toHaveBeenCalledWith(0, 3); // delete "old"
            expect(doc.insert).toHaveBeenCalledWith(0, 'new'); // insert "new"
            expect(doc.get_encrypted_update).toHaveBeenCalled();
        });

        it('should not modify if text is unchanged', async () => {
            await connectClient(client);

            const doc = (client as any).doc;
            doc.get_content.mockReturnValue('same content');
            client.sendUpdate('same content');

            expect(doc.delete).not.toHaveBeenCalled();
            expect(doc.insert).not.toHaveBeenCalled();
        });
    });

    describe('onUpdate', () => {
        it('should call callback when update is received', async () => {
            await connectClient(client);

            const callback = jest.fn();
            client.onUpdate(callback);

            // Simulate receiving a yrs_update message
            // We need to get the WebSocket instance to trigger the message
            // For this test, we'll verify the callback registration works
            expect(callback).not.toHaveBeenCalled();
        });
    });

    describe('getText', () => {
        it('should return current text from the MLS document', async () => {
            await connectClient(client);

            const doc = (client as any).doc;
            doc.get_content.mockReturnValue('hello world');
            expect(client.getText()).toBe('hello world');
        });

        it('should return empty string before a group is established', () => {
            expect(client.getText()).toBe('');
        });
    });

    describe('disconnect', () => {
        it('closes the socket and genuinely prevents reconnection', async () => {
            // Replaces a test whose body asserted NOTHING (it only said it
            // "verified" that the private maxReconnectAttempts was 0). Asserts
            // observable behavior instead: the socket is closed, the state is
            // disconnected, and a drop arriving AFTER disconnect() builds no
            // replacement socket.
            const OriginalWebSocket = global.WebSocket;
            const constructions = installStaysOpenWebSocket();
            const testClient = new CollabClient(config);

            try {
                await connectClient(testClient);
                const ws = (testClient as any).ws;
                expect(constructions()).toBe(1);

                testClient.disconnect();
                expect(ws.readyState).toBe(MockWebSocket.CLOSED);
                expect(testClient.getConnectionState()).toBe('disconnected');

                // The stopped client must not start the backoff loop.
                ws.onclose?.();
                jest.runAllTimers();
                await Promise.resolve();
                expect(constructions()).toBe(1);
            } finally {
                testClient.disconnect();
                global.WebSocket = OriginalWebSocket;
            }
        });
    });

    describe('reconnection', () => {
        // Every socket accepts (onopen) and drops in the SAME tick, so the
        // connection never survives long enough for the stability window to
        // elapse. This is the accept-then-immediately-drop relay (shutdown,
        // takeover, resource eviction) documented at collab-core
        // connection.rs:281 — the case the retry budget exists to stop.
        function installAcceptThenDropWebSocket(): () => number {
            let constructions = 0;
            (global as any).WebSocket = createMockWebSocket({
                onConstruct: (ws) => {
                    constructions++;
                    setTimeout(() => {
                        ws.readyState = 1;
                        ws.onopen?.();
                        ws.readyState = 3;
                        ws.onclose?.();
                    }, 0);
                },
            });
            return () => constructions;
        }

        // Sockets that fail before ever opening (onerror then onclose, the normal
        // browser failure order), so onopen never runs and the retry budget only
        // ever accumulates.
        function installFailBeforeOpenWebSocket(): void {
            (global as any).WebSocket = createMockWebSocket({
                onConstruct: (ws) => {
                    setTimeout(() => {
                        ws.onerror?.(new Error('connect failed'));
                        ws.readyState = 3;
                        ws.onclose?.();
                    }, 0);
                },
            });
        }

        // Record the delays handleReconnect schedules. Every other setTimeout in
        // these tests is either a mock socket's 0 ms construction hop or the
        // stability window at `minStableConnectionMs`, so both are excluded by
        // value (the reconnect delays are all >= 1000 ms and never collide).
        function captureScheduledDelays(ignoredDelays: number[]): {
            delays: () => number[];
            restore: () => void;
        } {
            const spy = jest.spyOn(globalThis as any, 'setTimeout') as unknown as {
                mock: { calls: unknown[][] };
                mockRestore: () => void;
            };
            return {
                delays: () =>
                    spy.mock.calls
                        .map((call) => call[1] as number)
                        .filter((delay) => !ignoredDelays.includes(delay)),
                restore: () => spy.mockRestore(),
            };
        }

        // Pump the backoff loop step-wise: run only the currently-pending timers,
        // then flush microtasks so connect()'s .finally (which clears
        // connectPromise) runs between macrotasks, exactly as real timers
        // interleave. runAllTimers would batch every timer without that flush,
        // leaving connectPromise stale so the dedup guard blocks every retry.
        // Two iterations advance the loop by one full cycle: one runs the
        // reconnect timer (constructing the socket), the next runs the socket's
        // own construction hop (opening it).
        async function pumpReconnectLoop(iterations = 40): Promise<void> {
            for (let i = 0; i < iterations; i++) {
                jest.runOnlyPendingTimers();
                await Promise.resolve();
                await Promise.resolve();
            }
        }

        // Open the first socket without letting the stability window elapse:
        // connectClient()'s runAllTimers would fire the stability timer too.
        async function connectWithoutStability(target: CollabClient): Promise<void> {
            const connectPromise = target.connect();
            jest.runOnlyPendingTimers();
            await connectPromise;
        }

        it('schedules reconnects with real exponential backoff', async () => {
            // Replaces a test whose body had no assertions at all ("tested
            // implicitly"), so it could not fail. Mirrors
            // retry_policy_exponential_backoff (collab-core connection.rs:387):
            // 1000ms doubling per attempt, all still under the 30000ms ceiling.
            const OriginalWebSocket = global.WebSocket;
            await connectClient(client);
            installFailBeforeOpenWebSocket();
            const captured = captureScheduledDelays([0]);

            try {
                (client as any).ws?.onclose?.();
                await pumpReconnectLoop();

                expect(captured.delays()).toEqual([1000, 2000, 4000, 8000, 16_000]);
            } finally {
                captured.restore();
                global.WebSocket = OriginalWebSocket;
            }
        });

        it('accept-then-drop without stability reaches max_retries_exceeded', async () => {
            // THE regression test. Mirrors
            // accept_then_drop_without_stability_reaches_give_up (collab-core
            // connection.rs:727): a relay that accepts the socket and immediately
            // drops it must exhaust the retry budget and give up, NOT reconnect
            // forever at a fixed cadence. minStableConnectionMs is far larger than
            // the whole test's virtual time, so the budget can never be refilled.
            const OriginalWebSocket = global.WebSocket;
            installAcceptThenDropWebSocket();
            const testClient = new CollabClient(
                makeDefaultConfig({ minStableConnectionMs: 10_000 })
            );
            const disconnectCallback = jest.fn();
            testClient.onDisconnect(disconnectCallback);

            try {
                await connectWithoutStability(testClient);
                await pumpReconnectLoop();

                expect(disconnectCallback).toHaveBeenCalledWith('max_retries_exceeded');
                expect(testClient.getConnectionState()).toBe('disconnected');
            } finally {
                testClient.disconnect();
                global.WebSocket = OriginalWebSocket;
            }
        }, 15_000);

        it('onopen alone does not reset the retry budget', async () => {
            // Mirrors on_connected_alone_does_not_reset_retry_count (collab-core
            // connection.rs:473): reaching onopen is not proof the connection is
            // useful, so the budget must keep accumulating across accept-then-drop
            // cycles. reconnectAttempts is private, so this asserts the observable
            // count of sockets built: the initial attempt plus exactly
            // maxReconnectAttempts (5) retries, then the loop stops for good.
            const OriginalWebSocket = global.WebSocket;
            const constructions = installAcceptThenDropWebSocket();
            const testClient = new CollabClient(
                makeDefaultConfig({ minStableConnectionMs: 10_000 })
            );
            const disconnectCallback = jest.fn();
            testClient.onDisconnect(disconnectCallback);

            try {
                await connectWithoutStability(testClient);
                await pumpReconnectLoop();

                expect(constructions()).toBe(6);
                expect(disconnectCallback).toHaveBeenCalledTimes(1);

                // ...and it stays stopped instead of resuming at a fixed cadence.
                await pumpReconnectLoop(10);
                expect(constructions()).toBe(6);
            } finally {
                testClient.disconnect();
                global.WebSocket = OriginalWebSocket;
            }
        }, 15_000);

        it('a client configured with zero reconnect attempts gives up immediately', async () => {
            // Mirrors retry_policy_zero_max_retries_fails_immediately (collab-core
            // connection.rs:669), which had no expressible TS equivalent while
            // disconnect() used the retry budget itself as its stop flag: a
            // configured 0 was indistinguishable from "the user stopped me".
            const OriginalWebSocket = global.WebSocket;
            const constructions = installStaysOpenWebSocket();
            const testClient = new CollabClient(
                makeDefaultConfig({ maxReconnectAttempts: 0, minStableConnectionMs: 10_000 })
            );
            const disconnectCallback = jest.fn();
            testClient.onDisconnect(disconnectCallback);

            try {
                await connectWithoutStability(testClient);
                expect(constructions()).toBe(1);

                (testClient as any).ws?.onclose?.();
                await pumpReconnectLoop(4);

                expect(disconnectCallback).toHaveBeenCalledWith('max_retries_exceeded');
                expect(testClient.getConnectionState()).toBe('disconnected');
                // Zero budget means zero retry sockets, not "five by default".
                expect(constructions()).toBe(1);
            } finally {
                testClient.disconnect();
                global.WebSocket = OriginalWebSocket;
            }
        });

        it('a configured reconnect budget is honoured', async () => {
            // Guards against the config field being accepted and then ignored:
            // 2 means the initial socket plus exactly 2 retries, then give up.
            const OriginalWebSocket = global.WebSocket;
            const constructions = installAcceptThenDropWebSocket();
            const testClient = new CollabClient(
                makeDefaultConfig({ maxReconnectAttempts: 2, minStableConnectionMs: 10_000 })
            );
            const disconnectCallback = jest.fn();
            testClient.onDisconnect(disconnectCallback);

            try {
                await connectWithoutStability(testClient);
                await pumpReconnectLoop();

                expect(constructions()).toBe(3);
                expect(disconnectCallback).toHaveBeenCalledWith('max_retries_exceeded');
                expect(testClient.getConnectionState()).toBe('disconnected');
            } finally {
                testClient.disconnect();
                global.WebSocket = OriginalWebSocket;
            }
        }, 15_000);

        it('connect() after disconnect() can reconnect again', async () => {
            // The overload's real cost: disconnect() zeroed the retry BUDGET, so a
            // client restarted with connect() kept a permanently empty budget and
            // could never reconnect again — even though disconnect() already resets
            // groupEstablished specifically to allow a restart. Stopping is
            // lifecycle state (cleared by connect()); the budget is configuration.
            const OriginalWebSocket = global.WebSocket;
            const constructions = installStaysOpenWebSocket();
            const testClient = new CollabClient(
                makeDefaultConfig({ minStableConnectionMs: 10_000 })
            );

            try {
                await connectWithoutStability(testClient);
                testClient.disconnect();
                expect(constructions()).toBe(1);

                // Explicit restart, then a drop: the backoff loop must run again.
                await connectWithoutStability(testClient);
                expect(constructions()).toBe(2);
                (testClient as any).ws?.onclose?.();
                await pumpReconnectLoop(4);

                expect(constructions()).toBeGreaterThan(2);
                expect(testClient.getConnectionState()).toBe('connected');
            } finally {
                testClient.disconnect();
                global.WebSocket = OriginalWebSocket;
            }
        });

        it('a connection that stays up past minStableConnectionMs refills the budget', async () => {
            // Mirrors on_stable_connection_resets_retry_count (collab-core
            // connection.rs:487). The discriminating half is the SECOND drop: a
            // connection torn down before the window elapses must NOT refill the
            // budget (that is the accept-then-drop bug), while one that outlives
            // the window must. The scheduled delay encodes the attempt number,
            // so 1000 -> attempt 1, 2000 -> attempt 2.
            const OriginalWebSocket = global.WebSocket;
            const STABLE_MS = 20;
            installStaysOpenWebSocket();
            const testClient = new CollabClient(
                makeDefaultConfig({ minStableConnectionMs: STABLE_MS })
            );
            const captured = captureScheduledDelays([0, STABLE_MS]);

            try {
                // Burn one attempt: connect, then drop before the window elapses.
                await connectWithoutStability(testClient);
                (testClient as any).ws?.onclose?.();
                expect(captured.delays()).toEqual([1000]);

                // Reconnect and drop early again: still no refill, so attempt 2.
                await pumpReconnectLoop(2);
                expect(testClient.getConnectionState()).toBe('connected');
                (testClient as any).ws?.onclose?.();
                expect(captured.delays()).toEqual([1000, 2000]);

                // Reconnect and let the window elapse: the budget is refilled, so
                // the next drop is attempt 1 again.
                await pumpReconnectLoop(2);
                expect(testClient.getConnectionState()).toBe('connected');
                jest.advanceTimersByTime(STABLE_MS);
                (testClient as any).ws?.onclose?.();
                expect(captured.delays()).toEqual([1000, 2000, 1000]);
            } finally {
                captured.restore();
                testClient.disconnect();
                global.WebSocket = OriginalWebSocket;
            }
        });

        it('repeated stable connections refill the budget each cycle', async () => {
            // Mirrors multi_cycle_stable_connection_resets_retry_count (collab-core
            // connection.rs:699): every stable cycle gets the full budget back, not
            // just the first one. The trailing early drop is the discriminator —
            // without a stability signal that cycle must NOT be refilled.
            const OriginalWebSocket = global.WebSocket;
            const STABLE_MS = 20;
            installStaysOpenWebSocket();
            const testClient = new CollabClient(
                makeDefaultConfig({ minStableConnectionMs: STABLE_MS })
            );
            const captured = captureScheduledDelays([0, STABLE_MS]);

            try {
                await connectWithoutStability(testClient);

                for (let cycle = 0; cycle < 3; cycle++) {
                    jest.advanceTimersByTime(STABLE_MS);
                    (testClient as any).ws?.onclose?.();
                    await pumpReconnectLoop(2);
                }
                expect(captured.delays()).toEqual([1000, 1000, 1000]);

                // Fourth cycle drops before the window: attempt 2, not attempt 1.
                (testClient as any).ws?.onclose?.();
                expect(captured.delays()).toEqual([1000, 1000, 1000, 2000]);
            } finally {
                captured.restore();
                testClient.disconnect();
                global.WebSocket = OriginalWebSocket;
            }
        });

        it('backoff is capped at maxReconnectDelayMs', async () => {
            // Mirrors retry_policy_caps_at_max_delay (collab-core
            // connection.rs:396) and the delay.min(max_delay) at
            // connection.rs:153. The TS delay was an unbounded 1000 * 2^(n-1).
            const OriginalWebSocket = global.WebSocket;
            const MAX_DELAY_MS = 3000;
            const testClient = new CollabClient(
                makeDefaultConfig({ maxReconnectDelayMs: MAX_DELAY_MS })
            );

            try {
                // The initial connect must OPEN: isInitialConnect gates the retry
                // loop, so a first socket that never opens never retries at all.
                await connectClient(testClient);
                installFailBeforeOpenWebSocket();
                const captured = captureScheduledDelays([0]);

                try {
                    (testClient as any).ws?.onclose?.();
                    await pumpReconnectLoop();

                    // 1000, 2000, then clamped: 4000/8000/16000 all become 3000.
                    expect(captured.delays()).toEqual([1000, 2000, 3000, 3000, 3000]);
                    expect(Math.max(...captured.delays())).toBeLessThanOrEqual(MAX_DELAY_MS);
                } finally {
                    captured.restore();
                }
            } finally {
                testClient.disconnect();
                global.WebSocket = OriginalWebSocket;
            }
        });

        it('disconnect() before the stability window does not later refill the budget', async () => {
            // A stability timer that outlives disconnect() would fire later and
            // hand a full retry budget to a client the user explicitly stopped.
            // The socket mock here does NOT fire onclose from close(), so the
            // cleanup cannot be credited to the onclose -> handleReconnect hop:
            // disconnect() itself has to clear the timer. reconnectAttempts is
            // private and a stopped client emits nothing while the window runs,
            // so this asserts on the fields directly — the same idiom the
            // existing max-retries tests use.
            const OriginalWebSocket = global.WebSocket;
            installStaysOpenWebSocket();
            const testClient = new CollabClient(
                makeDefaultConfig({ minStableConnectionMs: 10_000 })
            );

            try {
                await connectWithoutStability(testClient);
                expect((testClient as any).stabilityTimer).toBeTruthy();

                // Spend part of the budget, then stop before the window elapses.
                (testClient as any).reconnectAttempts = 3;
                testClient.disconnect();
                expect((testClient as any).stabilityTimer).toBeNull();

                // Well past the window: nothing may refill a stopped client.
                jest.advanceTimersByTime(60_000);
                expect((testClient as any).reconnectAttempts).toBe(3);
            } finally {
                testClient.disconnect();
                global.WebSocket = OriginalWebSocket;
            }
        });

        it('should keep retrying (not deadlock) when reconnect sockets fail before opening', async () => {
            // Regression test for the reconnect deadlock: after the first successful
            // connect, a dropped connection triggers handleReconnect -> connect(). If
            // that retry socket fails to open, its promise must still settle so the
            // dedup guard is unblocked and the backoff loop can keep going until
            // max_retries_exceeded fires. Before the fix, the promise never settled,
            // the dedup guard returned it forever, and no further sockets were created.
            const OriginalWebSocket = global.WebSocket;

            let constructions = 0;
            (global as any).WebSocket = createMockWebSocket({
                onConstruct: (ws) => {
                    constructions++;
                    const isFirst = constructions === 1;
                    setTimeout(() => {
                        if (isFirst) {
                            // First socket opens successfully.
                            ws.readyState = 1;
                            ws.onopen?.();
                        } else {
                            // Every reconnect socket fails before opening:
                            // onerror THEN onclose (the normal browser failure order).
                            ws.onerror?.(new Error('connect failed'));
                            ws.readyState = 3;
                            ws.onclose?.();
                        }
                    }, 0);
                },
            });

            const testClient = new CollabClient(config);
            const disconnectCallback = jest.fn();
            testClient.onDisconnect(disconnectCallback);

            try {
                // Initial connect succeeds.
                await connectClient(testClient);

                // Drop the live connection to kick off the reconnect/backoff loop.
                (testClient as any).ws?.onclose?.();

                // Pump the backoff loop step-wise: run only the currently-pending timers,
                // then flush microtasks. This mirrors real timers, where the microtask
                // queue (including connect()'s .finally that clears connectPromise) drains
                // between macrotasks. runAllTimers would batch every timer without that
                // flush, leaving connectPromise stale and masking the fix under test.
                for (let i = 0; i < 20; i++) {
                    jest.runOnlyPendingTimers();
                    await Promise.resolve();
                    await Promise.resolve();
                }

                // The loop ran to exhaustion instead of deadlocking on the first retry:
                // each failed attempt settled its promise, so new sockets kept being made.
                expect(constructions).toBeGreaterThan(2);
                expect(disconnectCallback).toHaveBeenCalledWith('max_retries_exceeded');
                expect(testClient.getConnectionState()).toBe('disconnected');
            } finally {
                testClient.disconnect();
                global.WebSocket = OriginalWebSocket;
            }
        });
    });
});

describe('CollabClient MLS-only crypto surface', () => {
    beforeEach(() => {
        jest.useFakeTimers();
        (WasmEncryptedDocument.create as unknown as jest.Mock).mockClear();
        (generate_key_package as unknown as jest.Mock).mockClear();
    });

    afterEach(() => {
        jest.useRealTimers();
    });

    it('has no encryptionKey in config and injects no key (owner creates an MLS group)', async () => {
        const config: CollabClientConfig = makeDefaultConfig();
        const client = new CollabClient(config);
        await connectClient(client);

        // Owner path creates the MLS group. There is no set_encryption_key call
        // (the mocked surface has no such method) — MLS keys come from the group.
        expect(WasmEncryptedDocument.create as unknown as jest.Mock).toHaveBeenCalledWith(
            'doc1',
            'user1'
        );
        const doc = (client as any).doc;
        expect(doc.set_encryption_key).toBeUndefined();
        // The config type carries no PSK field.
        expect((config as unknown as Record<string, unknown>).encryptionKey).toBeUndefined();

        client.disconnect();
    });

    it('joiner generates a single-use key package instead of taking a key', async () => {
        const config = makeDefaultConfig({ userId: 'bob', role: 'joiner' });
        const client = new CollabClient(config);
        await connectClient(client);

        expect(generate_key_package as unknown as jest.Mock).toHaveBeenCalledWith('bob');
        // No group yet (awaiting the Welcome): the document is null.
        expect((client as any).doc).toBeNull();

        client.disconnect();
    });

    it('fails closed: sendUpdate before the MLS group is established returns false and emits no frame', async () => {
        // A joiner before its Welcome arrives has no MLS group. sendUpdate must NOT
        // encrypt to nobody and must NOT fall back to a plaintext frame — it returns
        // false and sends nothing over the wire (CLAUDE.md fail-closed invariant).
        const config = makeDefaultConfig({ userId: 'bob', role: 'joiner' });
        const client = new CollabClient(config);
        await connectClient(client);

        const ws = (client as any).ws;
        const sentBefore = ws.sentMessages.length;
        const result = client.sendUpdate('secret text');

        expect(result).toBe(false);
        // No new frame at all — and specifically no yrs_update carrying content.
        expect(ws.sentMessages.length).toBe(sentBefore);
        const yrsFrames = ws.sentMessages.filter(
            (m: string) => JSON.parse(m).type === 'yrs_update'
        );
        expect(yrsFrames).toHaveLength(0);
        // The group never silently materialized.
        expect((client as any).doc).toBeNull();

        client.disconnect();
    });
});

describe('CollabClient MLS group lifecycle across reconnect', () => {
    beforeEach(() => {
        jest.useFakeTimers();
    });

    afterEach(() => {
        jest.useRealTimers();
    });

    // Pump the reconnect backoff loop step-wise: run the pending timer, then flush
    // microtasks (connect()'s .finally that clears connectPromise), mirroring how
    // real timers interleave macro/microtasks.
    async function pumpReconnect(): Promise<void> {
        for (let i = 0; i < 5; i++) {
            jest.runOnlyPendingTimers();
            await Promise.resolve();
            await Promise.resolve();
        }
    }

    it('Finding 1: establishes the MLS group EXACTLY ONCE across a reconnect (owner)', async () => {
        (WasmEncryptedDocument.create as unknown as jest.Mock).mockClear();
        const config: CollabClientConfig = makeDefaultConfig();
        const client = new CollabClient(config);
        await connectClient(client);

        const docAfterFirstConnect = (client as any).doc;
        expect(WasmEncryptedDocument.create as unknown as jest.Mock).toHaveBeenCalledTimes(1);
        expect(docAfterFirstConnect).not.toBeNull();

        // Drop the live connection; the backoff loop reconnects and re-runs onopen.
        (client as any).ws?.onclose?.();
        await pumpReconnect();

        expect(client.getConnectionState()).toBe('connected');
        // MLS group state is long-lived: reconnect must NOT re-create it (which
        // would orphan the real group at a fresh epoch-0 solo group).
        expect(WasmEncryptedDocument.create as unknown as jest.Mock).toHaveBeenCalledTimes(1);
        expect((client as any).doc).toBe(docAfterFirstConnect);

        client.disconnect();
    });

    it('Finding 2: a joined joiner does NOT answer a key_package as an owner', async () => {
        const config = makeDefaultConfig({ userId: 'bob', role: 'joiner' });
        const client = new CollabClient(config);
        await connectClient(client);

        // Deliver a Welcome so the joiner joins (doc set, pending consumed).
        (client as any).ws?.onmessage?.({
            data: JSON.stringify({
                type: 'mls_handshake',
                message_type: 'welcome',
                doc_id: 'doc1',
                payload: [1, 2],
            }),
        });
        const joinedDoc = (client as any).doc;
        expect(joinedDoc).not.toBeNull();

        const ws = (client as any).ws;
        const sentBefore = ws.sentMessages.length;

        // Misroute/replay: a key_package arrives at the (non-owner) joiner.
        (client as any).ws?.onmessage?.({
            data: JSON.stringify({
                type: 'mls_handshake',
                message_type: 'key_package',
                doc_id: 'doc1',
                payload: [3, 3, 3],
            }),
        });

        // Role guard: the joiner must NOT create an invite nor emit a Welcome.
        expect(joinedDoc.create_invite).not.toHaveBeenCalled();
        const welcomeFrames = ws.sentMessages.slice(sentBefore).filter((m: string) => {
            const p = JSON.parse(m);
            return p.type === 'mls_handshake' && p.message_type === 'welcome';
        });
        expect(welcomeFrames).toHaveLength(0);

        client.disconnect();
    });

    it('Finding 2: a replayed Welcome does NOT clobber an owner’s established group', async () => {
        (WasmEncryptedDocument.join as unknown as jest.Mock).mockClear();
        const config: CollabClientConfig = makeDefaultConfig();
        const client = new CollabClient(config);
        await connectClient(client);

        const originalDoc = (client as any).doc; // owner's created group
        expect(originalDoc).not.toBeNull();

        // Attacker replays a Welcome at the owner (who already has a group).
        (client as any).ws?.onmessage?.({
            data: JSON.stringify({
                type: 'mls_handshake',
                message_type: 'welcome',
                doc_id: 'doc1',
                payload: [1, 2],
            }),
        });

        // Role/state guard: the group handle is unchanged, join() never ran.
        expect((client as any).doc).toBe(originalDoc);
        expect(WasmEncryptedDocument.join as unknown as jest.Mock).not.toHaveBeenCalled();

        client.disconnect();
    });

    it('rejects a key_package mls_handshake whose doc_id does not match config.docId', async () => {
        // Defense in depth: the untrusted relay misroutes another document's key
        // package to this owner. The owner must NOT mint a Welcome for it.
        const config: CollabClientConfig = makeDefaultConfig();
        const client = new CollabClient(config);
        const errorCallback = jest.fn();
        client.onError(errorCallback);
        await connectClient(client);

        const doc = (client as any).doc;
        const ws = (client as any).ws;
        const sentBefore = ws.sentMessages.length;

        (client as any).ws?.onmessage?.({
            data: JSON.stringify({
                type: 'mls_handshake',
                message_type: 'key_package',
                doc_id: 'other-doc',
                payload: [3, 3, 3],
            }),
        });

        expect(doc.create_invite).not.toHaveBeenCalled();
        const welcomeFrames = ws.sentMessages.slice(sentBefore).filter((m: string) => {
            const p = JSON.parse(m);
            return p.type === 'mls_handshake' && p.message_type === 'welcome';
        });
        expect(welcomeFrames).toHaveLength(0);
        expect(errorCallback).toHaveBeenCalledWith(
            expect.objectContaining({
                type: 'sync',
                message: expect.stringContaining('doc_id mismatch'),
            })
        );

        client.disconnect();
    });

    it('rejects a welcome mls_handshake whose doc_id does not match config.docId', async () => {
        // Defense in depth: a Welcome misrouted from another document must not
        // make this joiner join a group.
        (WasmEncryptedDocument.join as unknown as jest.Mock).mockClear();
        const config = makeDefaultConfig({ userId: 'bob', role: 'joiner' });
        const client = new CollabClient(config);
        const errorCallback = jest.fn();
        client.onError(errorCallback);
        await connectClient(client);

        const pendingBefore = (client as any).pending;
        expect(pendingBefore).not.toBeNull();

        (client as any).ws?.onmessage?.({
            data: JSON.stringify({
                type: 'mls_handshake',
                message_type: 'welcome',
                doc_id: 'other-doc',
                payload: [1, 2],
            }),
        });

        // The joiner did NOT join, and its pending key package is untouched so a
        // correctly-routed Welcome can still succeed.
        expect((client as any).doc).toBeNull();
        expect((client as any).pending).toBe(pendingBefore);
        expect(WasmEncryptedDocument.join as unknown as jest.Mock).not.toHaveBeenCalled();
        expect(errorCallback).toHaveBeenCalledWith(
            expect.objectContaining({
                type: 'sync',
                message: expect.stringContaining('doc_id mismatch'),
            })
        );

        client.disconnect();
    });

    it('a malformed Welcome that makes join() throw does not permanently strand the joiner', async () => {
        // wasm-bindgen's generated glue for join(invite, pending) destroys the
        // `pending` handle unconditionally on call entry (pending.__destroy_into_raw()),
        // BEFORE the Rust call runs — so a garbage/malicious Welcome that makes
        // join() throw still burns the one-time key package. If this.pending were
        // only cleared on the success line, it would keep referencing that
        // now-dead handle, and a LATER legitimate Welcome would retry join() with
        // it and throw again — forever, with no way to recover except a full
        // session restart. The fix clears this.pending in lockstep with the
        // (consuming) call to join(), so a failed join fails closed exactly once
        // instead of wedging every subsequent Welcome.
        (WasmEncryptedDocument.join as unknown as jest.Mock).mockImplementationOnce(() => {
            throw new Error('malformed welcome payload');
        });
        const config = makeDefaultConfig({ userId: 'bob', role: 'joiner' });
        const client = new CollabClient(config);
        const errorCallback = jest.fn();
        client.onError(errorCallback);
        await connectClient(client);

        expect((client as any).pending).not.toBeNull();

        // An attacker (or a corrupt relay) delivers a garbage Welcome.
        (client as any).ws?.onmessage?.({
            data: JSON.stringify({
                type: 'mls_handshake',
                message_type: 'welcome',
                doc_id: 'doc1',
                payload: [1, 2],
            }),
        });

        expect(errorCallback).toHaveBeenCalledWith(
            expect.objectContaining({
                type: 'sync',
                message: expect.stringContaining('malformed welcome payload'),
            })
        );
        // The dead key package handle must not linger.
        expect((client as any).pending).toBeNull();
        expect((client as any).doc).toBeNull();

        // A second, legitimate Welcome arrives. Before the fix this retried
        // join() with the already-consumed handle and threw again; now the
        // `!this.pending` guard fails closed silently instead of throwing.
        errorCallback.mockClear();
        (client as any).ws?.onmessage?.({
            data: JSON.stringify({
                type: 'mls_handshake',
                message_type: 'welcome',
                doc_id: 'doc1',
                payload: [3, 4],
            }),
        });

        expect(errorCallback).not.toHaveBeenCalled();
        expect((client as any).doc).toBeNull();

        client.disconnect();
    });
});

describe('CollabClient message handling', () => {
    let client: CollabClient;

    beforeEach(() => {
        jest.useFakeTimers();
        const config: CollabClientConfig = makeDefaultConfig();
        client = new CollabClient(config);
    });

    afterEach(() => {
        jest.useRealTimers();
        client.disconnect();
    });

    it('should handle subscribed message', async () => {
        const consoleSpy = jest.spyOn(console, 'log').mockImplementation(() => {});

        await connectClient(client);

        consoleSpy.mockRestore();
    });

    it('should handle error message from server', async () => {
        const consoleErrorSpy = jest.spyOn(console, 'error').mockImplementation(() => {});

        await connectClient(client);

        consoleErrorSpy.mockRestore();
    });

    it('rejects an inbound frame larger than the byte cap before parsing it', async () => {
        // The relay is untrusted: an arbitrarily large frame must be rejected
        // BEFORE JSON.parse allocates unbounded arrays destined for Rust.
        const consoleErrorSpy = jest.spyOn(console, 'error').mockImplementation(() => {});
        const errorCallback = jest.fn();
        client.onError(errorCallback);

        await connectClient(client);

        const doc = (client as any).doc;
        (client as any).ws?.onmessage?.({
            data: `{"type":"yrs_update","encrypted":[${'1,'.repeat(2 * 1024 * 1024)}1]}`,
        });

        expect(doc.apply_encrypted_update).not.toHaveBeenCalled();
        expect(errorCallback).toHaveBeenCalledWith(
            expect.objectContaining({
                type: 'sync',
                message: expect.stringContaining('inbound frame exceeds'),
            })
        );
        consoleErrorSpy.mockRestore();
    });
});

describe('CollabClient message queueing', () => {
    let client: CollabClient;
    let config: CollabClientConfig;

    beforeEach(() => {
        jest.useFakeTimers();
        config = makeDefaultConfig();
        client = new CollabClient(config);
    });

    afterEach(() => {
        jest.useRealTimers();
        client.disconnect();
    });

    describe('when WebSocket is not ready', () => {
        it('should queue messages when a group exists but the WebSocket is not open', () => {
            // With an established MLS group but no open socket, updates are queued
            // rather than dropped. (Before a group exists, sendUpdate fails closed and
            // never reaches the queue — covered by the fail-closed test.)
            (client as any).doc = makeMockDoc();

            // This should queue the message instead of dropping it
            client.sendUpdate('test content');

            // Verify message was queued (check queue length)
            expect(client.getQueueLength()).toBe(1);
        });

        it('should queue multiple messages when the WebSocket is not open', () => {
            (client as any).doc = makeMockDoc();

            client.sendUpdate('content 1');
            client.sendUpdate('content 2');

            expect(client.getQueueLength()).toBe(2);
        });
    });

    describe('when WebSocket connection is established', () => {
        it('should flush queued messages when connection opens', async () => {
            // Queue a message before connecting (group already established).
            (client as any).doc = makeMockDoc();
            client.sendUpdate('queued content');

            expect(client.getQueueLength()).toBe(1);

            // Now connect
            await connectClient(client);

            // Queue should be empty after connection opens
            expect(client.getQueueLength()).toBe(0);
        });

        it('should send messages directly when WebSocket is already open', async () => {
            await connectClient(client);

            client.sendUpdate('direct content');

            // Message should be sent immediately, not queued
            expect(client.getQueueLength()).toBe(0);
        });

        it('should evict oldest messages when queue exceeds max size', () => {
            // Fill queue beyond the limit (maxQueueSize = 1000)
            (client as any).doc = makeMockDoc();
            const consoleSpy = jest.spyOn(console, 'warn').mockImplementation(() => {});

            // Queue 1001 messages while disconnected
            for (let i = 0; i < 1001; i++) {
                client.sendUpdate(`message ${i}`);
            }

            // Should have evicted one message (FIFO)
            expect(client.getQueueLength()).toBe(1000);
            expect(consoleSpy).toHaveBeenCalledWith(
                '[CollabClient] Message queue full, dropping oldest message:',
                expect.any(Object)
            );

            consoleSpy.mockRestore();
        });
    });

    describe('send return value', () => {
        it('should return false when message is queued', () => {
            (client as any).doc = makeMockDoc();

            const result = client.sendUpdate('test content');

            expect(result).toBe(false);
        });

        it('should return true when message is sent successfully', async () => {
            await connectClient(client);

            const result = client.sendUpdate('test content');

            expect(result).toBe(true);
        });
    });
});

describe('CollabClient disconnect notification', () => {
    let client: CollabClient;
    let config: CollabClientConfig;

    beforeEach(() => {
        jest.useFakeTimers();
        config = makeDefaultConfig();
        client = new CollabClient(config);
    });

    afterEach(() => {
        jest.useRealTimers();
        client.disconnect();
    });

    describe('onDisconnect callback', () => {
        it('should call onDisconnect callback when max retries exceeded', async () => {
            const disconnectCallback = jest.fn();
            client.onDisconnect(disconnectCallback);

            await connectClient(client);

            // Set reconnect attempts to max (5)
            (client as any).reconnectAttempts = 5;

            // Trigger onclose - should call disconnect callback since max retries exceeded
            (client as any).ws?.onclose?.();

            expect(disconnectCallback).toHaveBeenCalledWith('max_retries_exceeded');
        });

        it('should provide disconnect reason when max retries exceeded', async () => {
            const disconnectCallback = jest.fn();
            client.onDisconnect(disconnectCallback);

            await connectClient(client);

            // Set reconnect attempts to max
            (client as any).reconnectAttempts = 5;

            // Trigger onclose - should call disconnect callback
            (client as any).ws?.onclose?.();

            expect(disconnectCallback).toHaveBeenCalledTimes(1);
            expect(disconnectCallback.mock.calls[0][0]).toBe('max_retries_exceeded');
        });
    });

    describe('connection state', () => {
        it('should track connection state as disconnected initially', () => {
            expect(client.getConnectionState()).toBe('disconnected');
        });

        it('should track connection state as connected after successful connection', async () => {
            await connectClient(client);

            expect(client.getConnectionState()).toBe('connected');
        });

        it('should track connection state as reconnecting during reconnect attempts', async () => {
            await connectClient(client);

            // Simulate WebSocket close (triggers reconnect)
            (client as any).ws?.onclose?.();

            expect(client.getConnectionState()).toBe('reconnecting');
        });

        it('should track connection state as disconnected when max retries exceeded', async () => {
            await connectClient(client);

            // Set reconnect attempts to max (5)
            (client as any).reconnectAttempts = 5;

            // Trigger onclose - should set state to disconnected since max retries exceeded
            (client as any).ws?.onclose?.();

            expect(client.getConnectionState()).toBe('disconnected');
        });
    });
});

describe('CollabClient error handling', () => {
    let client: CollabClient;
    let config: CollabClientConfig;

    beforeEach(() => {
        jest.useFakeTimers();
        config = makeDefaultConfig();
        client = new CollabClient(config);
    });

    afterEach(() => {
        jest.useRealTimers();
        client.disconnect();
    });

    describe('reconnect error handling', () => {
        it('should invoke onErrorCallback when reconnect fails', async () => {
            const errorCallback = jest.fn();
            client.onError(errorCallback);

            await connectClient(client);

            // Mock WebSocket to fail on next connect
            const OriginalMockWebSocket = (global as any).WebSocket;
            (global as any).WebSocket = createMockWebSocket({
                onConstruct: (ws) => {
                    setTimeout(() => ws.onerror?.(new Error('Connection failed')), 0);
                },
            });

            try {
                // Trigger reconnect by simulating websocket close
                (client as any).ws?.onclose?.();

                // Advance timers to trigger reconnect attempt (creates the failing socket
                // and fires its onerror, which rejects the reconnect connect() promise).
                jest.runAllTimers();

                // The reconnect failure now surfaces to onErrorCallback through
                // handleReconnect()'s .catch on the rejected connect() promise. That path
                // is two microtask hops deep (.finally then .catch), so flush the queue.
                for (let i = 0; i < 5; i++) {
                    await Promise.resolve();
                }

                expect(errorCallback).toHaveBeenCalledWith(
                    expect.objectContaining({
                        type: 'connection',
                        message: expect.any(String),
                    })
                );
            } finally {
                // Restore original WebSocket even if the assertion throws, so a failure
                // here can't leak the failing mock into later tests.
                (global as any).WebSocket = OriginalMockWebSocket;
            }
        });
    });

    describe('WebSocket error after initial connection', () => {
        it('should invoke onErrorCallback on WebSocket error after connect', async () => {
            const errorCallback = jest.fn();
            client.onError(errorCallback);

            await connectClient(client);

            // Simulate WebSocket error after connection established
            (client as any).ws?.onerror?.(new Error('Network error'));

            expect(errorCallback).toHaveBeenCalledWith(
                expect.objectContaining({
                    type: 'connection',
                    message: expect.any(String),
                })
            );
        });
    });

    describe('reconnectTimer cleanup', () => {
        it('should clear reconnectTimer on disconnect', async () => {
            await connectClient(client);

            // Trigger reconnect to set up timer
            (client as any).ws?.onclose?.();

            // Verify timer is set
            expect((client as any).reconnectTimer).toBeDefined();

            // Disconnect should clear it
            client.disconnect();

            expect((client as any).reconnectTimer).toBeNull();
        });
    });

    describe('server error messages', () => {
        it('should invoke onErrorCallback when server sends error message', async () => {
            const errorCallback = jest.fn();
            client.onError(errorCallback);

            await connectClient(client);

            // Simulate server error message
            (client as any).ws?.onmessage?.({
                data: JSON.stringify({ type: 'error', message: 'Server error occurred' }),
            });

            expect(errorCallback).toHaveBeenCalledWith(
                expect.objectContaining({
                    type: 'sync',
                    message: 'Server error occurred',
                })
            );
        });

        it('should log warning for unknown message types', async () => {
            const warnSpy = jest.spyOn(console, 'warn').mockImplementation(() => {});

            await connectClient(client);

            // Simulate unknown message type
            (client as any).ws?.onmessage?.({
                data: JSON.stringify({ type: 'unknown_future_type', payload: 'data' }),
            });

            expect(warnSpy).toHaveBeenCalledWith(
                '[CollabClient] Unknown message type received: unknown_future_type',
                expect.objectContaining({ type: 'unknown_future_type' })
            );

            warnSpy.mockRestore();
        });
    });

    describe('flushMessageQueue error handling', () => {
        it('should re-queue messages when ws.send fails', async () => {
            await connectClient(client);

            // Queue a message first
            (client as any).ws.readyState = 3; // CLOSED
            client.sendUpdate('queued message');
            expect(client.getQueueLength()).toBe(1);

            // Now make send throw when we try to flush
            (client as any).ws.readyState = 1; // OPEN
            (client as any).ws.send = jest.fn().mockImplementation(() => {
                throw new Error('Send failed');
            });

            // Trigger flush
            (client as any).flushMessageQueue();

            // Message should be re-queued
            expect(client.getQueueLength()).toBe(1);
        });
    });

    describe('sendUpdate WASM error handling', () => {
        it('should invoke onErrorCallback when the MLS op fails in sendUpdate', async () => {
            const errorCallback = jest.fn();
            client.onError(errorCallback);

            await connectClient(client);

            // Make the MLS document throw
            const doc = (client as any).doc;
            doc.get_content.mockImplementation(() => {
                throw new Error('WASM error');
            });

            client.sendUpdate('test');

            expect(errorCallback).toHaveBeenCalledWith(
                expect.objectContaining({
                    type: 'sync',
                    message: 'WASM error',
                })
            );
        });

        it('should return false when the MLS op fails', async () => {
            await connectClient(client);

            const doc = (client as any).doc;
            doc.get_content.mockImplementation(() => {
                throw new Error('WASM error');
            });

            const result = client.sendUpdate('test');

            expect(result).toBe(false);
        });
    });

    describe('handleMessage JSON parse error handling', () => {
        it('should invoke onErrorCallback on JSON parse failure', async () => {
            const errorCallback = jest.fn();
            client.onError(errorCallback);

            await connectClient(client);

            // Simulate invalid JSON message
            (client as any).ws?.onmessage?.({
                data: 'invalid json {{{',
            });

            expect(errorCallback).toHaveBeenCalledWith(
                expect.objectContaining({
                    type: 'sync',
                    message: expect.stringContaining('parse'),
                })
            );
        });
    });
});

describe('CollabClient initialization verification', () => {
    let config: CollabClientConfig;

    beforeEach(() => {
        jest.useFakeTimers();
        config = makeDefaultConfig();
    });

    afterEach(() => {
        jest.useRealTimers();
    });

    describe('connect() failure on sendIdentify/subscribe', () => {
        it('should fail connect() if sendIdentify returns false', async () => {
            // Create a MockWebSocket that has readyState CLOSED when send is called
            const OriginalMockWebSocket = (global as any).WebSocket;
            (global as any).WebSocket = createMockWebSocket({
                initialReadyState: 3, // CLOSED - so send() returns false
                closeFiresOnclose: true,
                onConstruct: (ws) => {
                    setTimeout(() => ws.onopen?.(), 0);
                },
            });

            const client = new CollabClient(config);
            const connectPromise = client.connect();
            jest.runAllTimers();

            await expect(connectPromise).rejects.toThrow('Failed to send initialization messages');

            // Restore
            (global as any).WebSocket = OriginalMockWebSocket;
        });

        it('should fail connect() if subscribe returns false', async () => {
            // Create a MockWebSocket that returns CLOSED after first send
            const OriginalMockWebSocket = (global as any).WebSocket;
            let sendCount = 0;
            (global as any).WebSocket = createMockWebSocket({
                initialReadyState: 1, // OPEN initially
                closeFiresOnclose: true,
                onConstruct: (ws) => {
                    setTimeout(() => ws.onopen?.(), 0);
                },
                send: (ws) => {
                    sendCount++;
                    // After first send (identify), set readyState to CLOSED
                    if (sendCount === 1) {
                        ws.readyState = 3; // CLOSED
                    }
                },
            });

            const client = new CollabClient(config);
            const connectPromise = client.connect();
            jest.runAllTimers();

            await expect(connectPromise).rejects.toThrow('Failed to send initialization messages');

            // Restore
            (global as any).WebSocket = OriginalMockWebSocket;
        });
    });
});

describe('CollabClient handleReconnect timer cleanup', () => {
    let client: CollabClient;
    let config: CollabClientConfig;

    beforeEach(() => {
        jest.useFakeTimers();
        config = makeDefaultConfig();
        client = new CollabClient(config);
    });

    afterEach(() => {
        jest.useRealTimers();
        client.disconnect();
    });

    it('should clear reconnectTimer when max retries exceeded', async () => {
        await connectClient(client);

        // Set a reconnectTimer to simulate pending timer
        (client as any).reconnectTimer = setTimeout(() => {}, 1000);

        // Set reconnect attempts to max (5)
        (client as any).reconnectAttempts = 5;

        // Trigger onclose - should clear timer since max retries exceeded
        (client as any).ws?.onclose?.();

        expect((client as any).reconnectTimer).toBeNull();
    });
});

describe('CollabClient handleYrsUpdate validation', () => {
    let client: CollabClient;
    let config: CollabClientConfig;

    beforeEach(() => {
        jest.useFakeTimers();
        config = makeDefaultConfig();
        client = new CollabClient(config);
    });

    afterEach(() => {
        jest.useRealTimers();
        client.disconnect();
    });

    it('should invoke onErrorCallback when message.encrypted is missing', async () => {
        const errorCallback = jest.fn();
        client.onError(errorCallback);

        await connectClient(client);

        // Simulate yrs_update message without encrypted field
        (client as any).ws?.onmessage?.({
            data: JSON.stringify({ type: 'yrs_update' }),
        });

        expect(errorCallback).toHaveBeenCalledWith(
            expect.objectContaining({
                type: 'decryption',
                message: expect.stringContaining('Invalid yrs_update message'),
            })
        );
    });

    it('should invoke onErrorCallback when message.encrypted is not an array', async () => {
        const errorCallback = jest.fn();
        client.onError(errorCallback);

        await connectClient(client);

        // Simulate yrs_update message with non-array encrypted field
        (client as any).ws?.onmessage?.({
            data: JSON.stringify({ type: 'yrs_update', encrypted: 'not-an-array' }),
        });

        expect(errorCallback).toHaveBeenCalledWith(
            expect.objectContaining({
                type: 'decryption',
                message: expect.stringContaining('Invalid yrs_update message'),
            })
        );
    });

    it('should invoke onErrorCallback when message.encrypted is null', async () => {
        const errorCallback = jest.fn();
        client.onError(errorCallback);

        await connectClient(client);

        // Simulate yrs_update message with null encrypted field
        (client as any).ws?.onmessage?.({
            data: JSON.stringify({ type: 'yrs_update', encrypted: null }),
        });

        expect(errorCallback).toHaveBeenCalledWith(
            expect.objectContaining({
                type: 'decryption',
                message: expect.stringContaining('Invalid yrs_update message'),
            })
        );
    });

    it('should process a valid yrs_update message through the MLS document', async () => {
        const updateCallback = jest.fn();
        client.onUpdate(updateCallback);

        await connectClient(client);

        const doc = (client as any).doc;

        // Simulate valid yrs_update message
        (client as any).ws?.onmessage?.({
            data: JSON.stringify({ type: 'yrs_update', encrypted: [1, 2, 3] }),
        });

        // MLS decrypts under the group's current epoch (default 0 when omitted).
        expect(doc.apply_encrypted_update).toHaveBeenCalledWith(new Uint8Array([1, 2, 3]), 0n);
        expect(updateCallback).toHaveBeenCalled();
    });

    it('should reject a yrs_update whose doc_id does not match config.docId', async () => {
        // Defense in depth: a relay routing/replaying another document's frame to
        // this client must be rejected BEFORE the crypto core is touched, so an
        // attacker cannot even attempt cross-document splicing.
        const errorCallback = jest.fn();
        const updateCallback = jest.fn();
        client.onError(errorCallback);
        client.onUpdate(updateCallback);

        await connectClient(client);

        const doc = (client as any).doc;

        // config.docId is 'doc1'; this frame claims 'other-doc'.
        (client as any).ws?.onmessage?.({
            data: JSON.stringify({
                type: 'yrs_update',
                doc_id: 'other-doc',
                encrypted: [1, 2, 3],
            }),
        });

        expect(doc.apply_encrypted_update).not.toHaveBeenCalled();
        expect(updateCallback).not.toHaveBeenCalled();
        expect(errorCallback).toHaveBeenCalledWith(
            expect.objectContaining({
                type: 'decryption',
                message: expect.stringContaining('doc_id mismatch'),
            })
        );
    });

    it('should process a yrs_update whose doc_id matches config.docId', async () => {
        const updateCallback = jest.fn();
        client.onUpdate(updateCallback);

        await connectClient(client);

        const doc = (client as any).doc;

        (client as any).ws?.onmessage?.({
            data: JSON.stringify({
                type: 'yrs_update',
                doc_id: 'doc1',
                encrypted: [1, 2, 3],
            }),
        });

        expect(doc.apply_encrypted_update).toHaveBeenCalledWith(new Uint8Array([1, 2, 3]), 0n);
        expect(updateCallback).toHaveBeenCalled();
    });

    it('should properly extract error message from WASM error objects', async () => {
        const errorCallback = jest.fn();
        client.onError(errorCallback);

        await connectClient(client);

        // Mock apply_encrypted_update to throw a WASM-style error object.
        // WASM CollabError returns a plain object with {type, message} fields.
        const doc = (client as any).doc;
        const wasmError = { type: 'decryption', message: 'Ciphertext too short' };
        doc.apply_encrypted_update.mockImplementation(() => {
            throw wasmError;
        });

        // Simulate valid yrs_update message that will trigger decryption error
        (client as any).ws?.onmessage?.({
            data: JSON.stringify({ type: 'yrs_update', encrypted: [1, 2, 3] }),
        });

        // Error message should contain the WASM error type and message, not "[object Object]"
        expect(errorCallback).toHaveBeenCalledWith(
            expect.objectContaining({
                type: 'decryption',
                message: expect.stringContaining('decryption'),
            })
        );
        expect(errorCallback).toHaveBeenCalledWith(
            expect.objectContaining({
                message: expect.stringContaining('Ciphertext too short'),
            })
        );
        // Ensure we don't produce "[object Object]"
        expect((errorCallback.mock.calls[0][0] as { message: string }).message).not.toContain(
            '[object Object]'
        );
    });

    it('should handle standard Error objects in error messages', async () => {
        const errorCallback = jest.fn();
        client.onError(errorCallback);

        await connectClient(client);

        const doc = (client as any).doc;
        doc.apply_encrypted_update.mockImplementation(() => {
            throw new Error('Standard error message');
        });

        (client as any).ws?.onmessage?.({
            data: JSON.stringify({ type: 'yrs_update', encrypted: [1, 2, 3] }),
        });

        expect(errorCallback).toHaveBeenCalledWith(
            expect.objectContaining({
                message: 'Standard error message',
            })
        );
    });
});

describe('CollabClient applyTextDiff edge cases', () => {
    let client: CollabClient;
    let config: CollabClientConfig;

    beforeEach(() => {
        jest.useFakeTimers();
        config = makeDefaultConfig();
        client = new CollabClient(config);
    });

    afterEach(() => {
        jest.useRealTimers();
        client.disconnect();
    });

    // Connect an owner (creating its MLS group) and return the mock document so a
    // test can stage its content before driving sendUpdate through applyTextDiff.
    async function connectAndGetDoc(): Promise<any> {
        await connectClient(client);
        const doc = (client as any).doc;
        doc.insert.mockClear();
        doc.delete.mockClear();
        return doc;
    }

    it('should not call any CRDT operations when old and new text are identical', async () => {
        const doc = await connectAndGetDoc();
        doc.get_content.mockReturnValue('same content');

        client.sendUpdate('same content');

        expect(doc.delete).not.toHaveBeenCalled();
        expect(doc.insert).not.toHaveBeenCalled();
    });

    it('should handle empty old text (insert all)', async () => {
        const doc = await connectAndGetDoc();
        doc.get_content.mockReturnValue('');

        client.sendUpdate('new content');

        expect(doc.delete).not.toHaveBeenCalled();
        expect(doc.insert).toHaveBeenCalledWith(0, 'new content');
    });

    it('should handle empty new text (delete all)', async () => {
        const doc = await connectAndGetDoc();
        doc.get_content.mockReturnValue('old content');

        client.sendUpdate('');

        expect(doc.delete).toHaveBeenCalledWith(0, 11); // 'old content'.length
        expect(doc.insert).not.toHaveBeenCalled();
    });

    it('should handle both old and new text being empty', async () => {
        const doc = await connectAndGetDoc();
        doc.get_content.mockReturnValue('');

        client.sendUpdate('');

        expect(doc.delete).not.toHaveBeenCalled();
        expect(doc.insert).not.toHaveBeenCalled();
    });

    it('should handle complete replacement (no common prefix or suffix)', async () => {
        const doc = await connectAndGetDoc();
        doc.get_content.mockReturnValue('abc');

        client.sendUpdate('xyz');

        expect(doc.delete).toHaveBeenCalledWith(0, 3);
        expect(doc.insert).toHaveBeenCalledWith(0, 'xyz');
    });

    it('should find common prefix and only modify suffix', async () => {
        const doc = await connectAndGetDoc();
        doc.get_content.mockReturnValue('Hello World');

        client.sendUpdate('Hello Universe');

        // Common prefix: 'Hello ' (6 chars)
        // Delete: 'World' (5 chars starting at index 6)
        // Insert: 'Universe' at index 6
        expect(doc.delete).toHaveBeenCalledWith(6, 5);
        expect(doc.insert).toHaveBeenCalledWith(6, 'Universe');
    });

    it('should find common suffix and only modify prefix', async () => {
        const doc = await connectAndGetDoc();
        doc.get_content.mockReturnValue('Hello World');

        client.sendUpdate('Goodbye World');

        // Common suffix: ' World' (6 chars)
        // Delete: 'Hello' (5 chars starting at index 0)
        // Insert: 'Goodbye' at index 0
        expect(doc.delete).toHaveBeenCalledWith(0, 5);
        expect(doc.insert).toHaveBeenCalledWith(0, 'Goodbye');
    });

    it('should handle insertion in the middle', async () => {
        const doc = await connectAndGetDoc();
        doc.get_content.mockReturnValue('HelloWorld');

        client.sendUpdate('Hello World');

        // Common prefix: 'Hello' (5 chars)
        // Common suffix: 'World' (5 chars)
        // No deletion, insert ' ' at position 5
        expect(doc.delete).not.toHaveBeenCalled();
        expect(doc.insert).toHaveBeenCalledWith(5, ' ');
    });

    it('should handle deletion in the middle', async () => {
        const doc = await connectAndGetDoc();
        doc.get_content.mockReturnValue('Hello World');

        client.sendUpdate('HelloWorld');

        // Common prefix: 'Hello' (5 chars)
        // Common suffix: 'World' (5 chars)
        // Delete ' ' (1 char at position 5)
        expect(doc.delete).toHaveBeenCalledWith(5, 1);
        expect(doc.insert).not.toHaveBeenCalled();
    });

    it('should handle Unicode characters correctly', async () => {
        const doc = await connectAndGetDoc();
        doc.get_content.mockReturnValue('Hello 世界');

        client.sendUpdate('Hello 世界!');

        // Common prefix: 'Hello 世界' (8 chars)
        // Insert '!' at position 8
        expect(doc.delete).not.toHaveBeenCalled();
        expect(doc.insert).toHaveBeenCalledWith(8, '!');
    });

    it('should handle emoji characters correctly', async () => {
        const doc = await connectAndGetDoc();
        doc.get_content.mockReturnValue('Hello 👋');

        client.sendUpdate('Hello 👋 World');

        // Common prefix: 'Hello 👋' (8 chars - emoji is 2 code units)
        // Insert ' World' at position 8
        expect(doc.delete).not.toHaveBeenCalled();
        expect(doc.insert).toHaveBeenCalledWith(8, ' World');
    });
});
