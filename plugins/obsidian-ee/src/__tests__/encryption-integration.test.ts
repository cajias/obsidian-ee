/**
 * Integration tests for encrypted collaboration flow
 *
 * These tests verify that the CollabClient properly integrates with
 * the REAL compiled WASM CollabCore for end-to-end encrypted sync.
 *
 * The core is the REAL AES-256-GCM implementation (loaded from src/wasm),
 * so incoming-update tests must feed REAL ciphertext produced by a second
 * real core sharing the key — hand-crafted bytes are rejected by AEAD.
 * A MockWebSocket remains as the controllable transport.
 */

import { jest, describe, it, expect, beforeEach, beforeAll } from '@jest/globals';
import type { CollabClientConfig } from '../collab-client';
import { loadRealWasm } from './helpers/load-real-wasm';

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

// Valid 32-byte AES-256 key for testing
const mockEncryptionKey = new Uint8Array(32).fill(1);

beforeAll(async () => {
    await loadRealWasm();
});

const { CollabClient } = await import('../collab-client');
const { CollabCore } = await import('../wasm/collab_wasm');

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

            // Behavioral: constructor forwards the key to the real core.
            expect(core.has_encryption_key()).toBe(true);
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
            expect(core.has_encryption_key()).toBe(true);
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

            // Real core starts empty, so 'Hello' is a genuine insert.
            client.sendUpdate('Hello');

            const ws = MockWebSocket.instances[0];
            const updateMsg = ws.sentMessages.find((m) => m.type === 'yrs_update');

            expect(updateMsg).toBeDefined();
            expect(updateMsg.encrypted).toBeDefined();
            expect(Array.isArray(updateMsg.encrypted)).toBe(true);
            expect(updateMsg.encrypted.length).toBeGreaterThan(0);

            // Bulletproof proof: a peer core with the SAME key round-trip
            // decrypts the shipped payload back to the original text. A
            // plaintext passthrough (encode_state) cannot survive
            // apply_update_encrypted, so this pins real AES-256-GCM.
            const peer = new CollabCore();
            peer.set_encryption_key(mockEncryptionKey);
            peer.apply_update_encrypted(new Uint8Array(updateMsg.encrypted));
            expect(peer.get_text()).toBe('Hello');
            peer.free();

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

            client.sendUpdate('Test message');

            const ws = MockWebSocket.instances[0];
            const updateMsg = ws.sentMessages.find((m) => m.type === 'yrs_update');

            expect(updateMsg).toMatchObject({
                type: 'yrs_update',
                doc_id: 'doc1',
                epoch: 0,
            });
            expect(updateMsg.encrypted).toBeDefined();
            expect(updateMsg.encrypted.length).toBeGreaterThan(0);

            // Bulletproof proof: a peer core with the SAME key round-trip
            // decrypts the shipped payload back to the original text. A
            // plaintext passthrough (encode_state) cannot survive
            // apply_update_encrypted, so this pins real AES-256-GCM.
            const peer = new CollabCore();
            peer.set_encryption_key(mockEncryptionKey);
            peer.apply_update_encrypted(new Uint8Array(updateMsg.encrypted));
            expect(peer.get_text()).toBe('Test message');
            peer.free();

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

            // Produce REAL ciphertext from a second core sharing the key.
            const sender = new CollabCore();
            sender.set_encryption_key(mockEncryptionKey);
            sender.insert(0, 'Hi');
            const realCipher = [...sender.encode_state_encrypted()];

            const ws = MockWebSocket.instances[0];
            ws.simulateMessage({
                type: 'yrs_update',
                doc_id: 'doc1',
                from: 'user2',
                encrypted: realCipher,
                epoch: 0,
            });

            // Behavioral: decryption + apply succeeded, real text landed.
            expect(core.get_text()).toBe('Hi');

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

            // Real ciphertext for 'Hi' from a peer core with the shared key.
            const sender = new CollabCore();
            sender.set_encryption_key(mockEncryptionKey);
            sender.insert(0, 'Hi');
            const realCipher = [...sender.encode_state_encrypted()];

            const ws = MockWebSocket.instances[0];
            ws.simulateMessage({
                type: 'yrs_update',
                doc_id: 'doc1',
                from: 'user2',
                encrypted: realCipher,
                epoch: 0,
            });

            // Real core returns the actual decrypted text.
            expect(updateCallback).toHaveBeenCalledWith('Hi');

            client.disconnect();
            jest.useRealTimers();
        });
    });

    describe('encryption error handling', () => {
        it('should handle decryption errors gracefully', async () => {
            jest.useFakeTimers();
            const consoleErrorSpy = jest.spyOn(console, 'error').mockImplementation(() => {});

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
            // Real short/foreign ciphertext: AEAD rejects it, the client catches.
            expect(() => {
                ws.simulateMessage({
                    type: 'yrs_update',
                    doc_id: 'doc1',
                    from: 'user2',
                    encrypted: [1, 2, 3, 4],
                    epoch: 0,
                });
            }).not.toThrow();

            // Real errors are PLAIN objects ({type,message}), not Error instances.
            // [1,2,3,4] deterministically yields a 'Ciphertext too short'
            // decryption error, so assert the exact wrapper shape.
            expect(consoleErrorSpy).toHaveBeenCalledWith(
                'Failed to apply update:',
                expect.objectContaining({ type: 'decryption', message: 'Ciphertext too short' })
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

            // Should throw ConfigValidationError for invalid key length
            expect(() => new CollabClient(core, config)).toThrow(
                'encryptionKey must be exactly 32 bytes for AES-256, got 0 bytes'
            );
        });
    });

    describe('end-to-end encrypted sync flow', () => {
        it('should complete full encrypted sync cycle', async () => {
            jest.useFakeTimers();

            // Simulate two clients backed by real cores sharing the key.
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

            // Both clients set the encryption key on their real cores.
            expect(core1.has_encryption_key()).toBe(true);
            expect(core2.has_encryption_key()).toBe(true);

            // Connect both clients
            const connect1 = client1.connect();
            const connect2 = client2.connect();
            jest.runAllTimers();
            await Promise.all([connect1, connect2]);

            // Client 1 sends an update (real core starts empty).
            client1.sendUpdate('Hello from user1');

            // Get the real encrypted message sent by client1
            const ws1 = MockWebSocket.instances[0];
            const sentUpdate = ws1.sentMessages.find((m) => m.type === 'yrs_update');
            expect(sentUpdate).toBeDefined();

            // Client2 receives and decrypts client1's real ciphertext.
            const ws2 = MockWebSocket.instances[1];
            ws2.simulateMessage({
                type: 'yrs_update',
                doc_id: 'shared-doc',
                from: 'user1',
                encrypted: sentUpdate.encrypted,
                epoch: 0,
            });

            // Behavioral proof the full E2E crypto cycle worked.
            expect(core2.get_text()).toBe('Hello from user1');

            client1.disconnect();
            client2.disconnect();
            jest.useRealTimers();
        });
    });
});
