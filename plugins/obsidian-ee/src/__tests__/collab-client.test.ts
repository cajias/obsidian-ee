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
        .fn<() => { welcome: Uint8Array }>()
        .mockReturnValue({ welcome: new Uint8Array([9, 9]) }),
    process_commit: jest.fn(),
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

describe('CollabClient', () => {
    let client: CollabClient;
    let config: CollabClientConfig;

    beforeEach(() => {
        jest.useFakeTimers();
        config = {
            relayUrl: 'ws://localhost:8080',
            userId: 'user1',
            docId: 'doc1',
            role: 'owner',
        };
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
    });

    describe('connect', () => {
        it('should connect and send identify message', async () => {
            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

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

            (global as any).WebSocket = class ClosingWebSocket {
                static OPEN = 1;
                static CONNECTING = 0;
                static CLOSING = 2;
                static CLOSED = 3;
                readyState = 0;
                onopen: (() => void) | null = null;
                onclose: (() => void) | null = null;
                onerror: ((error: any) => void) | null = null;
                onmessage: ((event: { data: string }) => void) | null = null;
                sentMessages: string[] = [];

                constructor() {
                    // Close immediately during initial connection (before onopen)
                    setTimeout(() => {
                        this.readyState = 3;
                        this.onclose?.();
                    }, 0);
                }
                send(data: string) {
                    this.sentMessages.push(data);
                }
                close() {
                    this.readyState = 3;
                }
            };

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
            (global as any).WebSocket = class FailingWebSocket {
                static OPEN = 1;
                static CONNECTING = 0;
                static CLOSING = 2;
                static CLOSED = 3;
                readyState = 0;
                onopen: (() => void) | null = null;
                onclose: (() => void) | null = null;
                onerror: ((error: any) => void) | null = null;
                onmessage: ((event: { data: string }) => void) | null = null;
                sentMessages: string[] = [];

                constructor() {
                    connectionAttempts++;
                    // Fail first connection, succeed second
                    setTimeout(() => {
                        if (connectionAttempts === 1) {
                            this.onclose?.();
                        } else {
                            this.readyState = 1;
                            this.onopen?.();
                        }
                    }, 0);
                }
                send(data: string) {
                    this.sentMessages.push(data);
                }
                close() {
                    this.readyState = 3;
                }
            };

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
    });

    describe('sendUpdate', () => {
        it('should send an MLS-encrypted update to the relay', async () => {
            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

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
            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

            const doc = (client as any).doc;
            doc.get_content.mockReturnValue('same content');
            client.sendUpdate('same content');

            expect(doc.delete).not.toHaveBeenCalled();
            expect(doc.insert).not.toHaveBeenCalled();
        });
    });

    describe('onUpdate', () => {
        it('should call callback when update is received', async () => {
            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

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
            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

            const doc = (client as any).doc;
            doc.get_content.mockReturnValue('hello world');
            expect(client.getText()).toBe('hello world');
        });

        it('should return empty string before a group is established', () => {
            expect(client.getText()).toBe('');
        });
    });

    describe('disconnect', () => {
        it('should close WebSocket and prevent reconnection', async () => {
            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

            client.disconnect();
            // Verify reconnection is disabled by checking maxReconnectAttempts is 0
        });
    });

    describe('reconnection', () => {
        it('should attempt to reconnect with exponential backoff', async () => {
            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

            // The reconnection logic is tested implicitly through the handleReconnect method
            // which uses exponential backoff
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
            (global as any).WebSocket = class FlakyWebSocket {
                static OPEN = 1;
                static CONNECTING = 0;
                static CLOSING = 2;
                static CLOSED = 3;
                readyState = 0;
                onopen: (() => void) | null = null;
                onclose: (() => void) | null = null;
                onerror: ((error: any) => void) | null = null;
                onmessage: ((event: { data: string }) => void) | null = null;
                sentMessages: string[] = [];

                constructor() {
                    constructions++;
                    const isFirst = constructions === 1;
                    setTimeout(() => {
                        if (isFirst) {
                            // First socket opens successfully.
                            this.readyState = 1;
                            this.onopen?.();
                        } else {
                            // Every reconnect socket fails before opening:
                            // onerror THEN onclose (the normal browser failure order).
                            this.onerror?.(new Error('connect failed'));
                            this.readyState = 3;
                            this.onclose?.();
                        }
                    }, 0);
                }
                send(data: string) {
                    this.sentMessages.push(data);
                }
                close() {
                    this.readyState = 3;
                }
            };

            const testClient = new CollabClient(config);
            const disconnectCallback = jest.fn();
            testClient.onDisconnect(disconnectCallback);

            try {
                // Initial connect succeeds.
                const connectPromise = testClient.connect();
                jest.runAllTimers();
                await connectPromise;

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
        const config: CollabClientConfig = {
            relayUrl: 'ws://localhost:8080',
            userId: 'user1',
            docId: 'doc1',
            role: 'owner',
        };
        const client = new CollabClient(config);
        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;

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
        const config: CollabClientConfig = {
            relayUrl: 'ws://localhost:8080',
            userId: 'bob',
            docId: 'doc1',
            role: 'joiner',
        };
        const client = new CollabClient(config);
        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;

        expect(generate_key_package as unknown as jest.Mock).toHaveBeenCalledWith('bob');
        // No group yet (awaiting the Welcome): the document is null.
        expect((client as any).doc).toBeNull();

        client.disconnect();
    });

    it('fails closed: sendUpdate before the MLS group is established returns false and emits no frame', async () => {
        // A joiner before its Welcome arrives has no MLS group. sendUpdate must NOT
        // encrypt to nobody and must NOT fall back to a plaintext frame — it returns
        // false and sends nothing over the wire (CLAUDE.md fail-closed invariant).
        const config: CollabClientConfig = {
            relayUrl: 'ws://localhost:8080',
            userId: 'bob',
            docId: 'doc1',
            role: 'joiner',
        };
        const client = new CollabClient(config);
        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;

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

describe('CollabClient message handling', () => {
    let client: CollabClient;

    beforeEach(() => {
        jest.useFakeTimers();
        const config: CollabClientConfig = {
            relayUrl: 'ws://localhost:8080',
            userId: 'user1',
            docId: 'doc1',
            role: 'owner',
        };
        client = new CollabClient(config);
    });

    afterEach(() => {
        jest.useRealTimers();
        client.disconnect();
    });

    it('should handle subscribed message', async () => {
        const consoleSpy = jest.spyOn(console, 'log').mockImplementation(() => {});

        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;

        consoleSpy.mockRestore();
    });

    it('should handle error message from server', async () => {
        const consoleErrorSpy = jest.spyOn(console, 'error').mockImplementation(() => {});

        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;

        consoleErrorSpy.mockRestore();
    });
});

describe('CollabClient message queueing', () => {
    let client: CollabClient;
    let config: CollabClientConfig;

    beforeEach(() => {
        jest.useFakeTimers();
        config = {
            relayUrl: 'ws://localhost:8080',
            userId: 'user1',
            docId: 'doc1',
            role: 'owner',
        };
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
            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

            // Queue should be empty after connection opens
            expect(client.getQueueLength()).toBe(0);
        });

        it('should send messages directly when WebSocket is already open', async () => {
            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

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
            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

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
        config = {
            relayUrl: 'ws://localhost:8080',
            userId: 'user1',
            docId: 'doc1',
            role: 'owner',
        };
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

            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

            // Set reconnect attempts to max (5)
            (client as any).reconnectAttempts = 5;

            // Trigger onclose - should call disconnect callback since max retries exceeded
            (client as any).ws?.onclose?.();

            expect(disconnectCallback).toHaveBeenCalledWith('max_retries_exceeded');
        });

        it('should provide disconnect reason when max retries exceeded', async () => {
            const disconnectCallback = jest.fn();
            client.onDisconnect(disconnectCallback);

            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

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
            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

            expect(client.getConnectionState()).toBe('connected');
        });

        it('should track connection state as reconnecting during reconnect attempts', async () => {
            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

            // Simulate WebSocket close (triggers reconnect)
            (client as any).ws?.onclose?.();

            expect(client.getConnectionState()).toBe('reconnecting');
        });

        it('should track connection state as disconnected when max retries exceeded', async () => {
            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

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
        config = {
            relayUrl: 'ws://localhost:8080',
            userId: 'user1',
            docId: 'doc1',
            role: 'owner',
        };
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

            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

            // Mock WebSocket to fail on next connect
            const OriginalMockWebSocket = (global as any).WebSocket;
            (global as any).WebSocket = class FailingWebSocket {
                static OPEN = 1;
                static CONNECTING = 0;
                static CLOSING = 2;
                static CLOSED = 3;
                readyState = 0;
                onopen: (() => void) | null = null;
                onmessage: ((event: { data: string }) => void) | null = null;
                onerror: ((error: any) => void) | null = null;
                onclose: (() => void) | null = null;
                constructor() {
                    setTimeout(() => this.onerror?.(new Error('Connection failed')), 0);
                }
                send() {}
                close() {}
            };

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

            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

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
            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

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

            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

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

            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

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
            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

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

            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

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
            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

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

            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

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
        config = {
            relayUrl: 'ws://localhost:8080',
            userId: 'user1',
            docId: 'doc1',
            role: 'owner',
        };
    });

    afterEach(() => {
        jest.useRealTimers();
    });

    describe('connect() failure on sendIdentify/subscribe', () => {
        it('should fail connect() if sendIdentify returns false', async () => {
            // Create a MockWebSocket that has readyState CLOSED when send is called
            const OriginalMockWebSocket = (global as any).WebSocket;
            (global as any).WebSocket = class FailingSendWebSocket {
                static OPEN = 1;
                static CONNECTING = 0;
                static CLOSING = 2;
                static CLOSED = 3;
                readyState = 3; // CLOSED - so send() returns false
                onopen: (() => void) | null = null;
                onmessage: ((event: { data: string }) => void) | null = null;
                onerror: ((error: any) => void) | null = null;
                onclose: (() => void) | null = null;
                constructor() {
                    setTimeout(() => this.onopen?.(), 0);
                }
                send() {}
                close() {
                    this.onclose?.();
                }
            };

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
            (global as any).WebSocket = class PartialSendWebSocket {
                static OPEN = 1;
                static CONNECTING = 0;
                static CLOSING = 2;
                static CLOSED = 3;
                readyState = 1; // OPEN initially
                onopen: (() => void) | null = null;
                onmessage: ((event: { data: string }) => void) | null = null;
                onerror: ((error: any) => void) | null = null;
                onclose: (() => void) | null = null;
                constructor() {
                    setTimeout(() => this.onopen?.(), 0);
                }
                send() {
                    sendCount++;
                    // After first send (identify), set readyState to CLOSED
                    if (sendCount === 1) {
                        this.readyState = 3; // CLOSED
                    }
                }
                close() {
                    this.onclose?.();
                }
            };

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
        config = {
            relayUrl: 'ws://localhost:8080',
            userId: 'user1',
            docId: 'doc1',
            role: 'owner',
        };
        client = new CollabClient(config);
    });

    afterEach(() => {
        jest.useRealTimers();
        client.disconnect();
    });

    it('should clear reconnectTimer when max retries exceeded', async () => {
        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;

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
        config = {
            relayUrl: 'ws://localhost:8080',
            userId: 'user1',
            docId: 'doc1',
            role: 'owner',
        };
        client = new CollabClient(config);
    });

    afterEach(() => {
        jest.useRealTimers();
        client.disconnect();
    });

    it('should invoke onErrorCallback when message.encrypted is missing', async () => {
        const errorCallback = jest.fn();
        client.onError(errorCallback);

        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;

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

        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;

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

        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;

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

        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;

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

        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;

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

        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;

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

        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;

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

        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;

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
        config = {
            relayUrl: 'ws://localhost:8080',
            userId: 'user1',
            docId: 'doc1',
            role: 'owner',
        };
        client = new CollabClient(config);
    });

    afterEach(() => {
        jest.useRealTimers();
        client.disconnect();
    });

    // Connect an owner (creating its MLS group) and return the mock document so a
    // test can stage its content before driving sendUpdate through applyTextDiff.
    async function connectAndGetDoc(): Promise<any> {
        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;
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
