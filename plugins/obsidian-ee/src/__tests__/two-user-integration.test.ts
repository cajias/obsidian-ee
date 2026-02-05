/**
 * Integration test simulating two users collaborating
 *
 * This tests the full flow without needing actual Obsidian:
 * - Real WebSocket connections to the mock relay server
 * - Encrypted message exchange between users
 * - CRDT-style text synchronization
 */

import { WebSocket, WebSocketServer } from 'ws';

// Store original WebSocket if it exists
const OriginalWebSocket = (global as any).WebSocket;

/**
 * Real WebSocket wrapper that connects to actual mock relay server
 * This allows us to test with real network communication
 */
class NodeWebSocket {
    private ws: WebSocket;
    onopen: (() => void) | null = null;
    onmessage: ((event: { data: string }) => void) | null = null;
    onclose: (() => void) | null = null;
    onerror: ((error: any) => void) | null = null;
    readyState = 0; // CONNECTING

    constructor(url: string) {
        this.ws = new WebSocket(url);

        this.ws.on('open', () => {
            this.readyState = 1; // OPEN
            this.onopen?.();
        });

        this.ws.on('message', (data: Buffer) => {
            this.onmessage?.({ data: data.toString() });
        });

        this.ws.on('close', () => {
            this.readyState = 3; // CLOSED
            this.onclose?.();
        });

        this.ws.on('error', (err: Error) => {
            this.onerror?.(err);
        });
    }

    send(data: string): void {
        if (this.ws.readyState === WebSocket.OPEN) {
            this.ws.send(data);
        }
    }

    close(): void {
        this.ws.close();
    }

    static get CONNECTING() {
        return 0;
    }
    static get OPEN() {
        return 1;
    }
    static get CLOSING() {
        return 2;
    }
    static get CLOSED() {
        return 3;
    }
}

// Override global WebSocket with Node.js implementation
(global as any).WebSocket = NodeWebSocket;

/**
 * Mock CollabCore that simulates Yrs CRDT behavior with encryption
 * Each instance maintains its own document state
 */
const createMockCollabCore = () => {
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
            // Capture key after null check for TypeScript
            const key = encryptionKey;
            // Simulate encryption: marker bytes + XOR with key + text
            const encodedText = new TextEncoder().encode(text);
            const result = new Uint8Array(4 + encodedText.length);
            // Use first 4 bytes of key as "marker" to verify decryption
            result.set(key.slice(0, 4), 0);
            // XOR text with key for simple "encryption"
            encodedText.forEach((byte, idx) => {
                const keyByte = key.at(idx % key.length) ?? 0;
                result.set([byte ^ keyByte], 4 + idx);
            });
            return result;
        }),
        apply_update_encrypted: jest.fn((encrypted: Uint8Array) => {
            if (!encryptionKey || encryptionKey.length === 0) {
                throw new Error('No encryption key set');
            }
            // Capture key after null check for TypeScript
            const key = encryptionKey;
            // Verify marker bytes match
            const marker = encrypted.slice(0, 4);
            const expectedMarker = key.slice(0, 4);
            const markerMatches = marker.every(
                (byte, idx) => byte === (expectedMarker.at(idx) ?? -1)
            );
            if (!markerMatches) {
                throw new Error('Decryption failed: key mismatch');
            }
            // XOR to decrypt
            const encryptedText = encrypted.slice(4);
            const decrypted = new Uint8Array(encryptedText.length);
            encryptedText.forEach((byte, idx) => {
                const keyByte = key.at(idx % key.length) ?? 0;
                decrypted.set([byte ^ keyByte], idx);
            });
            text = new TextDecoder().decode(decrypted);
        }),
        free: jest.fn(),
    };
};

// Mock the WASM module
jest.mock('../wasm/collab_wasm', () => ({
    __esModule: true,
    CollabCore: jest.fn().mockImplementation(createMockCollabCore),
}));

import { CollabClient, CollabClientConfig } from '../collab-client';
import { CollabCore } from '../wasm/collab_wasm';

/**
 * Simple mock relay server for integration testing
 * Broadcasts messages to all connected clients except sender
 */
class IntegrationMockRelay {
    private wss: WebSocketServer | null = null;
    private clients: Map<string, WebSocket> = new Map();

    async start(port: number): Promise<void> {
        return new Promise((resolve, reject) => {
            try {
                this.wss = new WebSocketServer({ port });

                this.wss.on('connection', (ws) => {
                    let clientId: string | null = null;

                    ws.on('message', (data) => {
                        try {
                            const msg = JSON.parse(data.toString());

                            if (msg.type === 'identify') {
                                clientId = msg.user_id as string;
                                this.clients.set(clientId!, ws);
                            } else if (msg.type === 'subscribe') {
                                // Acknowledge subscription
                                ws.send(
                                    JSON.stringify({
                                        type: 'subscribed',
                                        doc_id: msg.doc_id,
                                    })
                                );
                            } else if (msg.type === 'yrs_update') {
                                // Broadcast to other clients
                                this.clients.forEach((client, id) => {
                                    if (id !== clientId && client.readyState === WebSocket.OPEN) {
                                        client.send(
                                            JSON.stringify({
                                                ...msg,
                                                from: clientId,
                                            })
                                        );
                                    }
                                });
                            }
                        } catch (error) {
                            console.error('Failed to parse message:', error);
                        }
                    });

                    ws.on('close', () => {
                        if (clientId) {
                            this.clients.delete(clientId);
                        }
                    });
                });

                this.wss.on('listening', () => resolve());
                this.wss.on('error', (err) => reject(err));
            } catch (error) {
                reject(error);
            }
        });
    }

    getClientCount(): number {
        return this.clients.size;
    }

    async stop(): Promise<void> {
        return new Promise((resolve) => {
            if (this.wss) {
                // Close all client connections first
                this.clients.forEach((client) => {
                    client.close();
                });
                this.clients.clear();

                this.wss.close(() => {
                    this.wss = null;
                    resolve();
                });
            } else {
                resolve();
            }
        });
    }
}

describe('Two User Collaboration Integration', () => {
    let relay: IntegrationMockRelay;
    const RELAY_PORT = 8082; // Use different port to avoid conflicts
    const RELAY_URL = `ws://localhost:${RELAY_PORT}`;
    const sharedEncryptionKey = new Uint8Array(32).fill(42);

    beforeAll(async () => {
        relay = new IntegrationMockRelay();
        await relay.start(RELAY_PORT);
    });

    afterAll(async () => {
        await relay.stop();
    });

    beforeEach(() => {
        jest.clearAllMocks();
    });

    it('should start relay server successfully', () => {
        expect(relay).toBeDefined();
    });

    it('two users can establish connections to relay', async () => {
        const core1 = new CollabCore();
        const core2 = new CollabCore();

        const config1: CollabClientConfig = {
            relayUrl: RELAY_URL,
            userId: 'user1',
            docId: 'test-doc-1',
            encryptionKey: sharedEncryptionKey,
        };

        const config2: CollabClientConfig = {
            relayUrl: RELAY_URL,
            userId: 'user2',
            docId: 'test-doc-1',
            encryptionKey: sharedEncryptionKey,
        };

        const client1 = new CollabClient(core1, config1);
        const client2 = new CollabClient(core2, config2);

        // Connect both clients
        await Promise.all([client1.connect(), client2.connect()]);

        // Wait for connections to stabilize
        await new Promise((r) => setTimeout(r, 100));

        // Both clients should be connected
        expect(relay.getClientCount()).toBe(2);

        // Cleanup
        client1.disconnect();
        client2.disconnect();

        // Wait for disconnections
        await new Promise((r) => setTimeout(r, 100));
    });

    it('user1 sends update and user2 receives it', async () => {
        const core1 = new CollabCore();
        const core2 = new CollabCore();

        const config1: CollabClientConfig = {
            relayUrl: RELAY_URL,
            userId: 'alice',
            docId: 'shared-doc',
            encryptionKey: sharedEncryptionKey,
        };

        const config2: CollabClientConfig = {
            relayUrl: RELAY_URL,
            userId: 'bob',
            docId: 'shared-doc',
            encryptionKey: sharedEncryptionKey,
        };

        const client1 = new CollabClient(core1, config1);
        const client2 = new CollabClient(core2, config2);

        // Track what user2 receives
        let user2ReceivedText = '';
        client2.onUpdate((text) => {
            user2ReceivedText = text;
        });

        // Connect both clients
        await Promise.all([client1.connect(), client2.connect()]);
        await new Promise((r) => setTimeout(r, 100));

        // User1 types "Hello"
        client1.sendUpdate('Hello');

        // Wait for message to propagate through relay
        await new Promise((r) => setTimeout(r, 200));

        // User2 should have received the update
        expect(user2ReceivedText).toBe('Hello');
        expect(core2.apply_update_encrypted).toHaveBeenCalled();

        // Cleanup
        client1.disconnect();
        client2.disconnect();
        await new Promise((r) => setTimeout(r, 100));
    });

    it('bidirectional sync - both users can send and receive', async () => {
        const core1 = new CollabCore();
        const core2 = new CollabCore();

        const config1: CollabClientConfig = {
            relayUrl: RELAY_URL,
            userId: 'writer1',
            docId: 'bidirectional-doc',
            encryptionKey: sharedEncryptionKey,
        };

        const config2: CollabClientConfig = {
            relayUrl: RELAY_URL,
            userId: 'writer2',
            docId: 'bidirectional-doc',
            encryptionKey: sharedEncryptionKey,
        };

        const client1 = new CollabClient(core1, config1);
        const client2 = new CollabClient(core2, config2);

        // Track received updates
        let user1ReceivedText = '';
        let user2ReceivedText = '';

        client1.onUpdate((text) => {
            user1ReceivedText = text;
        });
        client2.onUpdate((text) => {
            user2ReceivedText = text;
        });

        // Connect both clients
        await Promise.all([client1.connect(), client2.connect()]);
        await new Promise((r) => setTimeout(r, 100));

        // User1 types first
        client1.sendUpdate('Hello');
        await new Promise((r) => setTimeout(r, 200));
        expect(user2ReceivedText).toBe('Hello');

        // User2 responds
        client2.sendUpdate('Hello World');
        await new Promise((r) => setTimeout(r, 200));
        expect(user1ReceivedText).toBe('Hello World');

        // User1 adds more
        client1.sendUpdate('Hello World!');
        await new Promise((r) => setTimeout(r, 200));
        expect(user2ReceivedText).toBe('Hello World!');

        // Cleanup
        client1.disconnect();
        client2.disconnect();
        await new Promise((r) => setTimeout(r, 100));
    });

    it('encryption keys must match for successful sync', async () => {
        const core1 = new CollabCore();
        const core2 = new CollabCore();

        const key1 = new Uint8Array(32).fill(1);
        const key2 = new Uint8Array(32).fill(2); // Different key!

        const config1: CollabClientConfig = {
            relayUrl: RELAY_URL,
            userId: 'secure1',
            docId: 'encrypted-doc',
            encryptionKey: key1,
        };

        const config2: CollabClientConfig = {
            relayUrl: RELAY_URL,
            userId: 'secure2',
            docId: 'encrypted-doc',
            encryptionKey: key2,
        };

        const client1 = new CollabClient(core1, config1);
        const client2 = new CollabClient(core2, config2);

        // Track errors
        const consoleErrorSpy = jest.spyOn(console, 'error').mockImplementation();

        // Connect both clients
        await Promise.all([client1.connect(), client2.connect()]);
        await new Promise((r) => setTimeout(r, 100));

        // User1 sends encrypted update
        client1.sendUpdate('Secret message');
        await new Promise((r) => setTimeout(r, 200));

        // User2 should fail to decrypt (key mismatch)
        expect(consoleErrorSpy).toHaveBeenCalledWith('Failed to apply update:', expect.any(Error));

        consoleErrorSpy.mockRestore();

        // Cleanup
        client1.disconnect();
        client2.disconnect();
        await new Promise((r) => setTimeout(r, 100));
    });

    it('multiple rapid updates sync correctly', async () => {
        const core1 = new CollabCore();
        const core2 = new CollabCore();

        const config1: CollabClientConfig = {
            relayUrl: RELAY_URL,
            userId: 'fast-typer',
            docId: 'rapid-doc',
            encryptionKey: sharedEncryptionKey,
        };

        const config2: CollabClientConfig = {
            relayUrl: RELAY_URL,
            userId: 'observer',
            docId: 'rapid-doc',
            encryptionKey: sharedEncryptionKey,
        };

        const client1 = new CollabClient(core1, config1);
        const client2 = new CollabClient(core2, config2);

        const receivedUpdates: string[] = [];
        client2.onUpdate((text) => {
            receivedUpdates.push(text);
        });

        await Promise.all([client1.connect(), client2.connect()]);
        await new Promise((r) => setTimeout(r, 100));

        // Send rapid sequence of updates
        client1.sendUpdate('H');
        client1.sendUpdate('He');
        client1.sendUpdate('Hel');
        client1.sendUpdate('Hell');
        client1.sendUpdate('Hello');

        // Wait for all messages
        await new Promise((r) => setTimeout(r, 500));

        // Should have received all updates (order may vary due to async)
        expect(receivedUpdates.length).toBe(5);
        expect(receivedUpdates).toContain('Hello');

        // Cleanup
        client1.disconnect();
        client2.disconnect();
        await new Promise((r) => setTimeout(r, 100));
    });

    it('late joiner can receive updates from existing user', async () => {
        const core1 = new CollabCore();
        const core2 = new CollabCore();

        const config1: CollabClientConfig = {
            relayUrl: RELAY_URL,
            userId: 'early-bird',
            docId: 'late-join-doc',
            encryptionKey: sharedEncryptionKey,
        };

        const config2: CollabClientConfig = {
            relayUrl: RELAY_URL,
            userId: 'late-joiner',
            docId: 'late-join-doc',
            encryptionKey: sharedEncryptionKey,
        };

        const client1 = new CollabClient(core1, config1);
        const client2 = new CollabClient(core2, config2);

        // Only user1 connects first
        await client1.connect();
        await new Promise((r) => setTimeout(r, 100));

        // User2 joins later
        let user2ReceivedText = '';
        client2.onUpdate((text) => {
            user2ReceivedText = text;
        });

        await client2.connect();
        await new Promise((r) => setTimeout(r, 100));

        // Now user1 sends a message
        client1.sendUpdate('Welcome, late joiner!');
        await new Promise((r) => setTimeout(r, 200));

        // User2 should receive it
        expect(user2ReceivedText).toBe('Welcome, late joiner!');

        // Cleanup
        client1.disconnect();
        client2.disconnect();
        await new Promise((r) => setTimeout(r, 100));
    });

    it('disconnect and reconnect maintains sync capability', async () => {
        const core1 = new CollabCore();
        const core2 = new CollabCore();

        const config1: CollabClientConfig = {
            relayUrl: RELAY_URL,
            userId: 'persistent1',
            docId: 'reconnect-doc',
            encryptionKey: sharedEncryptionKey,
        };

        const config2: CollabClientConfig = {
            relayUrl: RELAY_URL,
            userId: 'persistent2',
            docId: 'reconnect-doc',
            encryptionKey: sharedEncryptionKey,
        };

        const client1 = new CollabClient(core1, config1);
        let client2 = new CollabClient(core2, config2);

        let user2ReceivedText = '';

        // Connect both
        await Promise.all([client1.connect(), client2.connect()]);
        await new Promise((r) => setTimeout(r, 100));

        // Initial sync works
        client2.onUpdate((text) => {
            user2ReceivedText = text;
        });
        client1.sendUpdate('Before disconnect');
        await new Promise((r) => setTimeout(r, 200));
        expect(user2ReceivedText).toBe('Before disconnect');

        // User2 disconnects
        client2.disconnect();
        await new Promise((r) => setTimeout(r, 100));

        // User2 reconnects with new client instance
        const core2New = new CollabCore();
        client2 = new CollabClient(core2New, config2);
        client2.onUpdate((text) => {
            user2ReceivedText = text;
        });
        await client2.connect();
        await new Promise((r) => setTimeout(r, 100));

        // Sync should still work after reconnection
        client1.sendUpdate('After reconnect');
        await new Promise((r) => setTimeout(r, 200));
        expect(user2ReceivedText).toBe('After reconnect');

        // Cleanup
        client1.disconnect();
        client2.disconnect();
        await new Promise((r) => setTimeout(r, 100));
    });
});

// Cleanup - restore original WebSocket if it existed
afterAll(() => {
    if (OriginalWebSocket) {
        (global as any).WebSocket = OriginalWebSocket;
    }
});
