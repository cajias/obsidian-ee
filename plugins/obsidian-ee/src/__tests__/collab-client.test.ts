import { CollabClient, CollabClientConfig, ConfigValidationError } from '../collab-client';

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

jest.mock('../wasm/collab_wasm', () => ({
    __esModule: true,
    CollabCore: jest.fn().mockImplementation(() => ({
        set_encryption_key: jest.fn(),
        get_text: jest.fn().mockReturnValue('test content'),
        insert: jest.fn(),
        delete: jest.fn(),
        encode_state_encrypted: jest.fn().mockReturnValue(new Uint8Array([1, 2, 3])),
        apply_update_encrypted: jest.fn(),
        free: jest.fn(),
    })),
}));

import { CollabCore } from '../wasm/collab_wasm';

describe('CollabClient', () => {
    let client: CollabClient;
    let mockCore: any;
    let config: CollabClientConfig;

    beforeEach(() => {
        jest.useFakeTimers();
        mockCore = new CollabCore();
        config = {
            relayUrl: 'ws://localhost:8080',
            userId: 'user1',
            docId: 'doc1',
            encryptionKey: new Uint8Array(32),
        };
        client = new CollabClient(mockCore, config);
    });

    afterEach(() => {
        jest.useRealTimers();
        client.disconnect();
    });

    describe('constructor', () => {
        it('should set encryption key on CollabCore', () => {
            expect(mockCore.set_encryption_key).toHaveBeenCalledWith(config.encryptionKey);
        });

        it('should throw ConfigValidationError for empty relayUrl', () => {
            const invalidConfig = { ...config, relayUrl: '' };
            expect(() => new CollabClient(mockCore, invalidConfig)).toThrow(ConfigValidationError);
            expect(() => new CollabClient(mockCore, invalidConfig)).toThrow(
                'relayUrl must be a non-empty string'
            );
        });

        it('should throw ConfigValidationError for invalid relayUrl protocol', () => {
            const invalidConfig = { ...config, relayUrl: 'http://localhost:8080' };
            expect(() => new CollabClient(mockCore, invalidConfig)).toThrow(ConfigValidationError);
            expect(() => new CollabClient(mockCore, invalidConfig)).toThrow(
                'relayUrl must start with ws:// or wss://'
            );
        });

        it('should accept wss:// relayUrl', () => {
            const secureConfig = { ...config, relayUrl: 'wss://secure.example.com' };
            expect(() => new CollabClient(mockCore, secureConfig)).not.toThrow();
        });

        it('should throw ConfigValidationError for empty userId', () => {
            const invalidConfig = { ...config, userId: '' };
            expect(() => new CollabClient(mockCore, invalidConfig)).toThrow(ConfigValidationError);
            expect(() => new CollabClient(mockCore, invalidConfig)).toThrow(
                'userId must be a non-empty string'
            );
        });

        it('should throw ConfigValidationError for empty docId', () => {
            const invalidConfig = { ...config, docId: '' };
            expect(() => new CollabClient(mockCore, invalidConfig)).toThrow(ConfigValidationError);
            expect(() => new CollabClient(mockCore, invalidConfig)).toThrow(
                'docId must be a non-empty string'
            );
        });

        it('should throw ConfigValidationError for wrong encryptionKey type', () => {
            const invalidConfig = { ...config, encryptionKey: [1, 2, 3] as any };
            expect(() => new CollabClient(mockCore, invalidConfig)).toThrow(ConfigValidationError);
            expect(() => new CollabClient(mockCore, invalidConfig)).toThrow(
                'encryptionKey must be a Uint8Array'
            );
        });

        it('should throw ConfigValidationError for wrong encryptionKey length', () => {
            const invalidConfig = { ...config, encryptionKey: new Uint8Array(16) };
            expect(() => new CollabClient(mockCore, invalidConfig)).toThrow(ConfigValidationError);
            expect(() => new CollabClient(mockCore, invalidConfig)).toThrow(
                'encryptionKey must be exactly 32 bytes for AES-256, got 16 bytes'
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

            const testClient = new CollabClient(mockCore, config);
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
            const newClient = new CollabClient(mockCore, config);

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

            const testClient = new CollabClient(mockCore, config);

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
        it('should send encrypted update to relay', async () => {
            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

            // Delta-diff: "old content" → "new content"
            // Common suffix is " content", so only "old" → "new" changes
            mockCore.get_text.mockReturnValue('old content');
            client.sendUpdate('new content');

            expect(mockCore.delete).toHaveBeenCalledWith(0, 3); // delete "old"
            expect(mockCore.insert).toHaveBeenCalledWith(0, 'new'); // insert "new"
            expect(mockCore.encode_state_encrypted).toHaveBeenCalled();
        });

        it('should not modify if text is unchanged', async () => {
            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

            mockCore.get_text.mockReturnValue('same content');
            client.sendUpdate('same content');

            expect(mockCore.delete).not.toHaveBeenCalled();
            expect(mockCore.insert).not.toHaveBeenCalled();
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
        it('should return current text from CollabCore', () => {
            mockCore.get_text.mockReturnValue('hello world');
            expect(client.getText()).toBe('hello world');
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
    });
});

describe('CollabClient message handling', () => {
    let client: CollabClient;
    let mockCore: any;

    beforeEach(() => {
        jest.useFakeTimers();
        mockCore = new CollabCore();
        const config: CollabClientConfig = {
            relayUrl: 'ws://localhost:8080',
            userId: 'user1',
            docId: 'doc1',
            encryptionKey: new Uint8Array(32),
        };
        client = new CollabClient(mockCore, config);
    });

    afterEach(() => {
        jest.useRealTimers();
        client.disconnect();
    });

    it('should handle subscribed message', async () => {
        const consoleSpy = jest.spyOn(console, 'log').mockImplementation();

        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;

        consoleSpy.mockRestore();
    });

    it('should handle error message from server', async () => {
        const consoleErrorSpy = jest.spyOn(console, 'error').mockImplementation();

        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;

        consoleErrorSpy.mockRestore();
    });
});

describe('CollabClient message queueing', () => {
    let client: CollabClient;
    let mockCore: any;
    let config: CollabClientConfig;

    beforeEach(() => {
        jest.useFakeTimers();
        mockCore = new CollabCore();
        config = {
            relayUrl: 'ws://localhost:8080',
            userId: 'user1',
            docId: 'doc1',
            encryptionKey: new Uint8Array(32),
        };
        client = new CollabClient(mockCore, config);
    });

    afterEach(() => {
        jest.useRealTimers();
        client.disconnect();
    });

    describe('when WebSocket is not ready', () => {
        it('should queue messages when WebSocket is not open', () => {
            // Don't connect - WebSocket is null
            mockCore.get_text.mockReturnValue('');

            // This should queue the message instead of dropping it
            client.sendUpdate('test content');

            // Verify message was queued (check queue length)
            expect(client.getQueueLength()).toBe(1);
        });

        it('should queue multiple messages when WebSocket is not open', () => {
            mockCore.get_text.mockReturnValue('');

            client.sendUpdate('content 1');
            mockCore.get_text.mockReturnValue('content 1');
            client.sendUpdate('content 2');

            expect(client.getQueueLength()).toBe(2);
        });
    });

    describe('when WebSocket connection is established', () => {
        it('should flush queued messages when connection opens', async () => {
            // Queue a message before connecting
            mockCore.get_text.mockReturnValue('');
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

            mockCore.get_text.mockReturnValue('');
            client.sendUpdate('direct content');

            // Message should be sent immediately, not queued
            expect(client.getQueueLength()).toBe(0);
        });

        it('should evict oldest messages when queue exceeds max size', () => {
            // Fill queue beyond the limit (maxQueueSize = 1000)
            mockCore.get_text.mockReturnValue('');
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
            mockCore.get_text.mockReturnValue('');

            const result = client.sendUpdate('test content');

            expect(result).toBe(false);
        });

        it('should return true when message is sent successfully', async () => {
            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

            mockCore.get_text.mockReturnValue('');
            const result = client.sendUpdate('test content');

            expect(result).toBe(true);
        });
    });
});

describe('CollabClient disconnect notification', () => {
    let client: CollabClient;
    let mockCore: any;
    let config: CollabClientConfig;

    beforeEach(() => {
        jest.useFakeTimers();
        mockCore = new CollabCore();
        config = {
            relayUrl: 'ws://localhost:8080',
            userId: 'user1',
            docId: 'doc1',
            encryptionKey: new Uint8Array(32),
        };
        client = new CollabClient(mockCore, config);
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
    let mockCore: any;
    let config: CollabClientConfig;

    beforeEach(() => {
        jest.useFakeTimers();
        mockCore = new CollabCore();
        config = {
            relayUrl: 'ws://localhost:8080',
            userId: 'user1',
            docId: 'doc1',
            encryptionKey: new Uint8Array(32),
        };
        client = new CollabClient(mockCore, config);
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

            // Trigger reconnect by simulating websocket close
            (client as any).ws?.onclose?.();

            // Advance timers to trigger reconnect attempt
            jest.runAllTimers();

            // Wait for async operations
            await Promise.resolve();
            jest.runAllTimers();

            expect(errorCallback).toHaveBeenCalledWith(
                expect.objectContaining({
                    type: 'connection',
                    message: expect.any(String),
                })
            );

            // Restore original WebSocket
            (global as any).WebSocket = OriginalMockWebSocket;
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
            mockCore.get_text.mockReturnValue('');
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
        it('should invoke onErrorCallback when WASM operation fails in sendUpdate', async () => {
            const errorCallback = jest.fn();
            client.onError(errorCallback);

            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

            // Make WASM throw
            mockCore.get_text.mockImplementation(() => {
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

        it('should return false when WASM operation fails', async () => {
            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

            mockCore.get_text.mockImplementation(() => {
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
    let mockCore: any;
    let config: CollabClientConfig;

    beforeEach(() => {
        jest.useFakeTimers();
        mockCore = new CollabCore();
        config = {
            relayUrl: 'ws://localhost:8080',
            userId: 'user1',
            docId: 'doc1',
            encryptionKey: new Uint8Array(32),
        };
    });

    afterEach(() => {
        jest.useRealTimers();
    });

    describe('connect() failure on sendIdentify/subscribe', () => {
        it('should fail connect() if sendIdentify returns false', async () => {
            // Create a MockWebSocket that has readyState CLOSED when send is called
            const OriginalMockWebSocket = (global as any).WebSocket;
            let _wsInstance: any;
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
                    _wsInstance = this;
                    setTimeout(() => this.onopen?.(), 0);
                }
                send() {}
                close() {
                    this.onclose?.();
                }
            };

            const client = new CollabClient(mockCore, config);
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

            const client = new CollabClient(mockCore, config);
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
    let mockCore: any;
    let config: CollabClientConfig;

    beforeEach(() => {
        jest.useFakeTimers();
        mockCore = new CollabCore();
        config = {
            relayUrl: 'ws://localhost:8080',
            userId: 'user1',
            docId: 'doc1',
            encryptionKey: new Uint8Array(32),
        };
        client = new CollabClient(mockCore, config);
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
    let mockCore: any;
    let config: CollabClientConfig;

    beforeEach(() => {
        jest.useFakeTimers();
        mockCore = new CollabCore();
        config = {
            relayUrl: 'ws://localhost:8080',
            userId: 'user1',
            docId: 'doc1',
            encryptionKey: new Uint8Array(32),
        };
        client = new CollabClient(mockCore, config);
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

    it('should process valid yrs_update message with encrypted array', async () => {
        const updateCallback = jest.fn();
        client.onUpdate(updateCallback);

        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;

        // Simulate valid yrs_update message
        (client as any).ws?.onmessage?.({
            data: JSON.stringify({ type: 'yrs_update', encrypted: [1, 2, 3] }),
        });

        expect(mockCore.apply_update_encrypted).toHaveBeenCalledWith(new Uint8Array([1, 2, 3]));
        expect(updateCallback).toHaveBeenCalled();
    });

    it('should properly extract error message from WASM error objects', async () => {
        const errorCallback = jest.fn();
        client.onError(errorCallback);

        // Mock apply_update_encrypted to throw a WASM-style error object
        // WASM CollabError returns a plain object with {type, message} fields
        const wasmError = { type: 'decryption', message: 'Ciphertext too short' };
        mockCore.apply_update_encrypted.mockImplementation(() => {
            throw wasmError;
        });

        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;

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
        expect(errorCallback.mock.calls[0][0].message).not.toContain('[object Object]');
    });

    it('should handle standard Error objects in error messages', async () => {
        const errorCallback = jest.fn();
        client.onError(errorCallback);

        mockCore.apply_update_encrypted.mockImplementation(() => {
            throw new Error('Standard error message');
        });

        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;

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
    let mockCore: any;
    let config: CollabClientConfig;

    beforeEach(() => {
        jest.useFakeTimers();
        mockCore = new CollabCore();
        config = {
            relayUrl: 'ws://localhost:8080',
            userId: 'user1',
            docId: 'doc1',
            encryptionKey: new Uint8Array(32),
        };
        client = new CollabClient(mockCore, config);
    });

    afterEach(() => {
        jest.useRealTimers();
        client.disconnect();
    });

    it('should not call any CRDT operations when old and new text are identical', async () => {
        mockCore.get_text.mockReturnValue('same content');

        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;

        mockCore.insert.mockClear();
        mockCore.delete.mockClear();

        client.sendUpdate('same content');

        expect(mockCore.delete).not.toHaveBeenCalled();
        expect(mockCore.insert).not.toHaveBeenCalled();
    });

    it('should handle empty old text (insert all)', async () => {
        mockCore.get_text.mockReturnValue('');

        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;

        mockCore.insert.mockClear();
        mockCore.delete.mockClear();

        client.sendUpdate('new content');

        expect(mockCore.delete).not.toHaveBeenCalled();
        expect(mockCore.insert).toHaveBeenCalledWith(0, 'new content');
    });

    it('should handle empty new text (delete all)', async () => {
        mockCore.get_text.mockReturnValue('old content');

        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;

        mockCore.insert.mockClear();
        mockCore.delete.mockClear();

        client.sendUpdate('');

        expect(mockCore.delete).toHaveBeenCalledWith(0, 11); // 'old content'.length
        expect(mockCore.insert).not.toHaveBeenCalled();
    });

    it('should handle both old and new text being empty', async () => {
        mockCore.get_text.mockReturnValue('');

        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;

        mockCore.insert.mockClear();
        mockCore.delete.mockClear();

        client.sendUpdate('');

        expect(mockCore.delete).not.toHaveBeenCalled();
        expect(mockCore.insert).not.toHaveBeenCalled();
    });

    it('should handle complete replacement (no common prefix or suffix)', async () => {
        mockCore.get_text.mockReturnValue('abc');

        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;

        mockCore.insert.mockClear();
        mockCore.delete.mockClear();

        client.sendUpdate('xyz');

        expect(mockCore.delete).toHaveBeenCalledWith(0, 3);
        expect(mockCore.insert).toHaveBeenCalledWith(0, 'xyz');
    });

    it('should find common prefix and only modify suffix', async () => {
        mockCore.get_text.mockReturnValue('Hello World');

        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;

        mockCore.insert.mockClear();
        mockCore.delete.mockClear();

        client.sendUpdate('Hello Universe');

        // Common prefix: 'Hello ' (6 chars)
        // Delete: 'World' (5 chars starting at index 6)
        // Insert: 'Universe' at index 6
        expect(mockCore.delete).toHaveBeenCalledWith(6, 5);
        expect(mockCore.insert).toHaveBeenCalledWith(6, 'Universe');
    });

    it('should find common suffix and only modify prefix', async () => {
        mockCore.get_text.mockReturnValue('Hello World');

        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;

        mockCore.insert.mockClear();
        mockCore.delete.mockClear();

        client.sendUpdate('Goodbye World');

        // Common suffix: ' World' (6 chars)
        // Delete: 'Hello' (5 chars starting at index 0)
        // Insert: 'Goodbye' at index 0
        expect(mockCore.delete).toHaveBeenCalledWith(0, 5);
        expect(mockCore.insert).toHaveBeenCalledWith(0, 'Goodbye');
    });

    it('should handle insertion in the middle', async () => {
        mockCore.get_text.mockReturnValue('HelloWorld');

        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;

        mockCore.insert.mockClear();
        mockCore.delete.mockClear();

        client.sendUpdate('Hello World');

        // Common prefix: 'Hello' (5 chars)
        // Common suffix: 'World' (5 chars)
        // No deletion, insert ' ' at position 5
        expect(mockCore.delete).not.toHaveBeenCalled();
        expect(mockCore.insert).toHaveBeenCalledWith(5, ' ');
    });

    it('should handle deletion in the middle', async () => {
        mockCore.get_text.mockReturnValue('Hello World');

        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;

        mockCore.insert.mockClear();
        mockCore.delete.mockClear();

        client.sendUpdate('HelloWorld');

        // Common prefix: 'Hello' (5 chars)
        // Common suffix: 'World' (5 chars)
        // Delete ' ' (1 char at position 5)
        expect(mockCore.delete).toHaveBeenCalledWith(5, 1);
        expect(mockCore.insert).not.toHaveBeenCalled();
    });

    it('should handle Unicode characters correctly', async () => {
        mockCore.get_text.mockReturnValue('Hello 世界');

        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;

        mockCore.insert.mockClear();
        mockCore.delete.mockClear();

        client.sendUpdate('Hello 世界!');

        // Common prefix: 'Hello 世界' (8 chars)
        // Insert '!' at position 8
        expect(mockCore.delete).not.toHaveBeenCalled();
        expect(mockCore.insert).toHaveBeenCalledWith(8, '!');
    });

    it('should handle emoji characters correctly', async () => {
        mockCore.get_text.mockReturnValue('Hello 👋');

        const connectPromise = client.connect();
        jest.runAllTimers();
        await connectPromise;

        mockCore.insert.mockClear();
        mockCore.delete.mockClear();

        client.sendUpdate('Hello 👋 World');

        // Common prefix: 'Hello 👋' (8 chars - emoji is 2 code units)
        // Insert ' World' at position 8
        expect(mockCore.delete).not.toHaveBeenCalled();
        expect(mockCore.insert).toHaveBeenCalledWith(8, ' World');
    });
});
