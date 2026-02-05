/**
 * Integration tests for encrypted collaboration flow
 *
 * These tests verify that the CollabClient properly integrates with
 * the CollabCore encryption methods for end-to-end encrypted sync.
 */

// Mock WebSocket - must be defined before import
class MockWebSocket {
    static OPEN = 1;
    static CONNECTING = 0;
    static CLOSING = 2;
    static CLOSED = 3;
    static instances: MockWebSocket[] = [];

    readyState = MockWebSocket.OPEN;
    onopen: (() => void) | null = null;
    onmessage: ((event: { data: string }) => void) | null = null;
    onerror: ((error: any) => void) | null = null;
    onclose: (() => void) | null = null;
    sentMessages: any[] = [];

    constructor(public url: string) {
        MockWebSocket.instances.push(this);
        setTimeout(() => this.onopen?.(), 0);
    }

    send(data: string): void {
        this.sentMessages.push(JSON.parse(data));
    }

    close(): void {
        this.readyState = MockWebSocket.CLOSED;
        this.onclose?.();
    }

    // Simulate receiving a message
    simulateMessage(data: object): void {
        this.onmessage?.({ data: JSON.stringify(data) });
    }
}

// @ts-ignore - Override global WebSocket
global.WebSocket = MockWebSocket;

// Mock encryption key for testing
const mockEncryptionKey = new Uint8Array(32).fill(1);

// Real WASM mock that tracks encryption state
jest.mock('../wasm/collab_wasm', () => ({
    __esModule: true,
    CollabCore: jest.fn().mockImplementation(() => {
        let text = '';
        let encryptionKey: Uint8Array | null = null;

        return {
            set_encryption_key: jest.fn((key: Uint8Array) => {
                encryptionKey = key;
            }),
            has_encryption_key: jest.fn(() => encryptionKey !== null && encryptionKey.length > 0),
            get_text: jest.fn(() => text),
            insert: jest.fn((idx: number, content: string) => {
                text = text.slice(0, idx) + content + text.slice(idx);
            }),
            delete: jest.fn((idx: number, len: number) => {
                text = text.slice(0, idx) + text.slice(idx + len);
            }),
            encode_state_encrypted: jest.fn(() => {
                if (!encryptionKey || encryptionKey.length === 0) {
                    throw new Error('No encryption key set');
                }
                // Simulate encrypted state: key prefix (4 bytes) + encoded text
                const encodedText = new TextEncoder().encode(text);
                const result = new Uint8Array(4 + encodedText.length);
                result.set(encryptionKey.slice(0, 4), 0);
                result.set(encodedText, 4);
                return result;
            }),
            apply_update_encrypted: jest.fn((encrypted: Uint8Array) => {
                if (!encryptionKey || encryptionKey.length === 0) {
                    throw new Error('No encryption key set');
                }
                // Simulate decryption - extract text after key prefix
                const decoded = new TextDecoder().decode(encrypted.slice(4));
                text = decoded;
            }),
            free: jest.fn(),
        };
    }),
}));

import { CollabClient, CollabClientConfig } from '../collab-client';
import { CollabCore } from '../wasm/collab_wasm';

describe('Encrypted Collaboration Integration', () => {
    beforeEach(() => {
        MockWebSocket.instances = [];
        jest.clearAllMocks();
    });

    describe('encryption key setup', () => {
        it('should set encryption key on CollabCore during construction', () => {
            const core = new CollabCore();
            const config: CollabClientConfig = {
                relayUrl: 'ws://localhost:8080',
                userId: 'user1',
                docId: 'doc1',
                encryptionKey: mockEncryptionKey,
            };

            new CollabClient(core, config);

            expect(core.set_encryption_key).toHaveBeenCalledWith(mockEncryptionKey);
        });

        it('should accept encryption key of correct length (32 bytes)', () => {
            const core = new CollabCore();
            const validKey = new Uint8Array(32).fill(42);
            const config: CollabClientConfig = {
                relayUrl: 'ws://localhost:8080',
                userId: 'user1',
                docId: 'doc1',
                encryptionKey: validKey,
            };

            expect(() => new CollabClient(core, config)).not.toThrow();
            expect(core.set_encryption_key).toHaveBeenCalledWith(validKey);
        });
    });

    describe('outgoing encrypted updates', () => {
        it('should encrypt outgoing updates before sending', async () => {
            jest.useFakeTimers();

            const core = new CollabCore();
            const config: CollabClientConfig = {
                relayUrl: 'ws://localhost:8080',
                userId: 'user1',
                docId: 'doc1',
                encryptionKey: mockEncryptionKey,
            };

            const client = new CollabClient(core, config);
            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

            // Simulate existing text to trigger update
            (core.get_text as jest.Mock).mockReturnValue('');
            client.sendUpdate('Hello');

            expect(core.encode_state_encrypted).toHaveBeenCalled();

            const ws = MockWebSocket.instances[0];
            const updateMsg = ws.sentMessages.find(m => m.type === 'yrs_update');

            expect(updateMsg).toBeDefined();
            expect(updateMsg.encrypted).toBeDefined();
            expect(Array.isArray(updateMsg.encrypted)).toBe(true);

            client.disconnect();
            jest.useRealTimers();
        });

        it('should include encrypted payload in yrs_update message', async () => {
            jest.useFakeTimers();

            const core = new CollabCore();
            const config: CollabClientConfig = {
                relayUrl: 'ws://localhost:8080',
                userId: 'user1',
                docId: 'doc1',
                encryptionKey: mockEncryptionKey,
            };

            const client = new CollabClient(core, config);
            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

            (core.get_text as jest.Mock).mockReturnValue('');
            client.sendUpdate('Test message');

            const ws = MockWebSocket.instances[0];
            const updateMsg = ws.sentMessages.find(m => m.type === 'yrs_update');

            expect(updateMsg).toMatchObject({
                type: 'yrs_update',
                doc_id: 'doc1',
                epoch: 0,
                signature: [],
            });
            expect(updateMsg.encrypted).toBeDefined();
            expect(updateMsg.encrypted.length).toBeGreaterThan(0);

            client.disconnect();
            jest.useRealTimers();
        });
    });

    describe('incoming encrypted updates', () => {
        it('should decrypt incoming updates from other users', async () => {
            jest.useFakeTimers();

            const core = new CollabCore();
            const config: CollabClientConfig = {
                relayUrl: 'ws://localhost:8080',
                userId: 'user1',
                docId: 'doc1',
                encryptionKey: mockEncryptionKey,
            };

            const client = new CollabClient(core, config);
            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

            const ws = MockWebSocket.instances[0];
            // Simulate encrypted message from another user
            // Key prefix (4 bytes) + "Hi" encoded
            const encryptedData = [1, 1, 1, 1, 72, 105]; // [key prefix] + "Hi"
            ws.simulateMessage({
                type: 'yrs_update',
                doc_id: 'doc1',
                from: 'user2',
                encrypted: encryptedData,
                epoch: 0,
                signature: [],
            });

            expect(core.apply_update_encrypted).toHaveBeenCalled();
            const callArg = (core.apply_update_encrypted as jest.Mock).mock.calls[0][0];
            expect(callArg).toBeInstanceOf(Uint8Array);

            client.disconnect();
            jest.useRealTimers();
        });

        it('should trigger onUpdate callback after decrypting', async () => {
            jest.useFakeTimers();

            const core = new CollabCore();
            const config: CollabClientConfig = {
                relayUrl: 'ws://localhost:8080',
                userId: 'user1',
                docId: 'doc1',
                encryptionKey: mockEncryptionKey,
            };

            const client = new CollabClient(core, config);
            const updateCallback = jest.fn();
            client.onUpdate(updateCallback);

            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

            // Mock get_text to return decrypted content
            (core.get_text as jest.Mock).mockReturnValue('Decrypted content');

            const ws = MockWebSocket.instances[0];
            ws.simulateMessage({
                type: 'yrs_update',
                doc_id: 'doc1',
                from: 'user2',
                encrypted: [1, 1, 1, 1, 68, 101, 99], // Mock encrypted data
                epoch: 0,
                signature: [],
            });

            expect(updateCallback).toHaveBeenCalledWith('Decrypted content');

            client.disconnect();
            jest.useRealTimers();
        });
    });

    describe('encryption error handling', () => {
        it('should handle decryption errors gracefully', async () => {
            jest.useFakeTimers();
            const consoleErrorSpy = jest.spyOn(console, 'error').mockImplementation();

            const core = new CollabCore();
            // Make apply_update_encrypted throw an error
            (core.apply_update_encrypted as jest.Mock).mockImplementation(() => {
                throw new Error('Decryption failed');
            });

            const config: CollabClientConfig = {
                relayUrl: 'ws://localhost:8080',
                userId: 'user1',
                docId: 'doc1',
                encryptionKey: mockEncryptionKey,
            };

            const client = new CollabClient(core, config);
            const connectPromise = client.connect();
            jest.runAllTimers();
            await connectPromise;

            const ws = MockWebSocket.instances[0];
            // This should not throw
            expect(() => {
                ws.simulateMessage({
                    type: 'yrs_update',
                    doc_id: 'doc1',
                    from: 'user2',
                    encrypted: [1, 2, 3, 4],
                    epoch: 0,
                    signature: [],
                });
            }).not.toThrow();

            expect(consoleErrorSpy).toHaveBeenCalledWith(
                'Failed to apply update:',
                expect.any(Error)
            );

            consoleErrorSpy.mockRestore();
            client.disconnect();
            jest.useRealTimers();
        });

        it('should not throw during construction with empty encryption key', () => {
            const core = new CollabCore();
            const config: CollabClientConfig = {
                relayUrl: 'ws://localhost:8080',
                userId: 'user1',
                docId: 'doc1',
                encryptionKey: new Uint8Array(0), // Empty key
            };

            // Should not throw during construction
            expect(() => new CollabClient(core, config)).not.toThrow();
        });
    });

    describe('end-to-end encrypted sync flow', () => {
        it('should complete full encrypted sync cycle', async () => {
            jest.useFakeTimers();

            // Simulate two clients
            const core1 = new CollabCore();
            const core2 = new CollabCore();

            const config1: CollabClientConfig = {
                relayUrl: 'ws://localhost:8080',
                userId: 'user1',
                docId: 'shared-doc',
                encryptionKey: mockEncryptionKey,
            };

            const config2: CollabClientConfig = {
                relayUrl: 'ws://localhost:8080',
                userId: 'user2',
                docId: 'shared-doc',
                encryptionKey: mockEncryptionKey, // Same key for shared doc
            };

            const client1 = new CollabClient(core1, config1);
            const client2 = new CollabClient(core2, config2);

            // Both clients set encryption key
            expect(core1.set_encryption_key).toHaveBeenCalledWith(mockEncryptionKey);
            expect(core2.set_encryption_key).toHaveBeenCalledWith(mockEncryptionKey);

            // Connect both clients
            const connect1 = client1.connect();
            const connect2 = client2.connect();
            jest.runAllTimers();
            await Promise.all([connect1, connect2]);

            // Client 1 sends an update
            (core1.get_text as jest.Mock).mockReturnValue('');
            client1.sendUpdate('Hello from user1');

            expect(core1.encode_state_encrypted).toHaveBeenCalled();

            // Get the encrypted message sent by client1
            const ws1 = MockWebSocket.instances[0];
            const sentUpdate = ws1.sentMessages.find(m => m.type === 'yrs_update');
            expect(sentUpdate).toBeDefined();

            // Simulate client2 receiving this encrypted update
            const ws2 = MockWebSocket.instances[1];
            ws2.simulateMessage({
                type: 'yrs_update',
                doc_id: 'shared-doc',
                from: 'user1',
                encrypted: sentUpdate.encrypted,
                epoch: 0,
                signature: [],
            });

            expect(core2.apply_update_encrypted).toHaveBeenCalled();

            client1.disconnect();
            client2.disconnect();
            jest.useRealTimers();
        });
    });
});
