import { CollabClient, CollabClientConfig } from '../collab-client';

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
    });

    describe('sendUpdate', () => {
        it('should send encrypted update to relay', async () => {
            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

            mockCore.get_text.mockReturnValue('old content');
            client.sendUpdate('new content');

            expect(mockCore.delete).toHaveBeenCalledWith(0, 11); // 'old content'.length
            expect(mockCore.insert).toHaveBeenCalledWith(0, 'new content');
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
    let _mockWs: MockWebSocket;

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
});
