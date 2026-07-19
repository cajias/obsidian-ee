import { CollabCore } from './wasm/collab_wasm';

export interface CollabClientConfig {
    relayUrl: string;
    userId: string;
    docId: string;
    encryptionKey: Uint8Array;
}

export type UpdateCallback = (text: string) => void;
export type DisconnectCallback = (reason: string) => void;
export type ErrorCallback = (error: CollabError) => void;
export type ConnectionState = 'connected' | 'connecting' | 'disconnected' | 'reconnecting';

export interface CollabError {
    type: 'decryption' | 'connection' | 'sync';
    message: string;
    docId?: string;
    originalError?: Error;
}

export interface YrsUpdateMessage {
    type: 'yrs_update';
    encrypted: number[];
    doc_id?: string;
    epoch?: number;
}

/**
 * Interface for WASM CollabError objects returned from Rust.
 * These are plain JS objects with type and message fields.
 */
interface WasmCollabError {
    type: string;
    message: string;
}

/**
 * Type guard to check if an error is a WASM CollabError object.
 */
function isWasmCollabError(error: unknown): error is WasmCollabError {
    return (
        typeof error === 'object' &&
        error !== null &&
        'type' in error &&
        'message' in error &&
        typeof (error as WasmCollabError).type === 'string' &&
        typeof (error as WasmCollabError).message === 'string'
    );
}

/**
 * Extract error message from various error types including WASM errors.
 * WASM errors are plain objects that would produce "[object Object]" with String().
 */
function extractErrorMessage(error: unknown): string {
    if (error instanceof Error) {
        return error.message;
    }
    if (isWasmCollabError(error)) {
        return `[${error.type}] ${error.message}`;
    }
    return String(error);
}

/**
 * Validation error thrown when CollabClientConfig has invalid values.
 */
export class ConfigValidationError extends Error {
    constructor(message: string) {
        super(message);
        this.name = 'ConfigValidationError';
    }
}

/**
 * Validate CollabClientConfig values at runtime.
 * Throws ConfigValidationError if validation fails.
 */
function validateConfig(config: CollabClientConfig): void {
    // Validate relayUrl
    if (!config.relayUrl || typeof config.relayUrl !== 'string') {
        throw new ConfigValidationError('relayUrl must be a non-empty string');
    }
    if (!config.relayUrl.startsWith('ws://') && !config.relayUrl.startsWith('wss://')) {
        throw new ConfigValidationError('relayUrl must start with ws:// or wss://');
    }

    // Validate userId
    if (!config.userId || typeof config.userId !== 'string') {
        throw new ConfigValidationError('userId must be a non-empty string');
    }

    // Validate docId
    if (!config.docId || typeof config.docId !== 'string') {
        throw new ConfigValidationError('docId must be a non-empty string');
    }

    // Validate encryptionKey
    if (!(config.encryptionKey instanceof Uint8Array)) {
        throw new ConfigValidationError('encryptionKey must be a Uint8Array');
    }
    if (config.encryptionKey.length !== 32) {
        throw new ConfigValidationError(
            `encryptionKey must be exactly 32 bytes for AES-256, got ${config.encryptionKey.length} bytes`
        );
    }
}

export class CollabClient {
    private ws: WebSocket | null = null;
    private collabCore: CollabCore;
    private config: CollabClientConfig;
    private onUpdateCallback: UpdateCallback | null = null;
    private onDisconnectCallback: DisconnectCallback | null = null;
    private onErrorCallback: ErrorCallback | null = null;
    private reconnectAttempts = 0;
    private maxReconnectAttempts = 5;
    private reconnectDelay = 1000;
    private messageQueue: object[] = [];
    private readonly maxQueueSize = 1000;
    private connectionState: ConnectionState = 'disconnected';
    private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
    private isInitialConnect = true;
    private connectPromise: Promise<void> | null = null;

    constructor(collabCore: CollabCore, config: CollabClientConfig) {
        validateConfig(config);
        this.collabCore = collabCore;
        this.config = config;
        this.collabCore.set_encryption_key(config.encryptionKey);
    }

    connect(): Promise<void> {
        // Prevent concurrent connection attempts
        if (this.connectPromise) {
            return this.connectPromise;
        }

        this.connectionState = 'connecting';
        this.connectPromise = new Promise<void>((resolve, reject) => {
            try {
                this.ws = new WebSocket(this.config.relayUrl);

                this.ws.onopen = () => {
                    console.log('Connected to relay server');
                    this.connectionState = 'connected';
                    this.isInitialConnect = false;
                    // Note: Don't clear connectPromise here - the finally block handles that
                    // to avoid race conditions with concurrent connection attempts

                    // Critical: verify initialization messages are sent
                    const identified = this.sendIdentify();
                    const subscribed = this.subscribe();

                    if (!identified || !subscribed) {
                        const error = new Error('Failed to send initialization messages');
                        console.error('[CollabClient]', error.message);
                        this.ws?.close();
                        this.ws = null;
                        reject(error);
                        return;
                    }

                    this.flushMessageQueue();
                    this.reconnectAttempts = 0;
                    resolve();
                };

                this.ws.onmessage = (event) => {
                    this.handleMessage(event.data);
                };

                this.ws.onerror = (error) => {
                    console.error('WebSocket error:', error);
                    if (this.isInitialConnect) {
                        reject(error);
                    } else {
                        // After initial connect, invoke error callback
                        if (this.onErrorCallback) {
                            const collabError: CollabError = {
                                type: 'connection',
                                message: error instanceof Error ? error.message : 'WebSocket error',
                                docId: this.config.docId,
                                originalError: error instanceof Error ? error : undefined,
                            };
                            this.onErrorCallback(collabError);
                        }
                    }
                };

                this.ws.onclose = () => {
                    console.log('WebSocket closed');
                    if (this.isInitialConnect) {
                        // Connection closed during initial connect - reject the promise
                        reject(new Error('WebSocket closed during initial connection'));
                    } else {
                        this.handleReconnect();
                    }
                };
            } catch (error) {
                reject(error);
            }
        }).finally(() => {
            // Clear promise tracking on completion (success or failure)
            this.connectPromise = null;
        });

        return this.connectPromise;
    }

    private sendIdentify(): boolean {
        return this.send({
            type: 'identify',
            user_id: this.config.userId,
        });
    }

    private subscribe(): boolean {
        return this.send({
            type: 'subscribe',
            doc_id: this.config.docId,
        });
    }

    private send(message: object): boolean {
        if (this.ws?.readyState === WebSocket.OPEN) {
            this.ws.send(JSON.stringify(message));
            return true;
        }
        // Queue message instead of silently dropping it, with FIFO eviction at max size
        if (this.messageQueue.length >= this.maxQueueSize) {
            const dropped = this.messageQueue.shift();
            console.warn('[CollabClient] Message queue full, dropping oldest message:', dropped);
        }
        this.messageQueue.push(message);
        return false;
    }

    private flushMessageQueue(): void {
        const failedMessages: object[] = [];
        while (this.messageQueue.length > 0) {
            const message = this.messageQueue.shift();
            if (message && this.ws?.readyState === WebSocket.OPEN) {
                try {
                    this.ws.send(JSON.stringify(message));
                } catch (error) {
                    console.error('Failed to send queued message:', error);
                    failedMessages.push(message);
                }
            }
        }
        // Re-queue failed messages
        this.messageQueue.push(...failedMessages);
    }

    getQueueLength(): number {
        return this.messageQueue.length;
    }

    private handleMessage(data: string): void {
        try {
            const message = JSON.parse(data);

            switch (message.type) {
                case 'yrs_update':
                    this.handleYrsUpdate(message as YrsUpdateMessage);
                    break;
                case 'subscribed':
                    console.log('Subscribed to document:', message.doc_id);
                    break;
                case 'error':
                    console.error('Server error:', message.message);
                    if (this.onErrorCallback) {
                        const collabError: CollabError = {
                            type: 'sync',
                            message: message.message || 'Server error',
                            docId: this.config.docId,
                        };
                        this.onErrorCallback(collabError);
                    }
                    break;
                default:
                    console.warn(
                        `[CollabClient] Unknown message type received: ${message.type}`,
                        message
                    );
                    break;
            }
        } catch (error) {
            console.error('Failed to parse message:', error);
            if (this.onErrorCallback) {
                const collabError: CollabError = {
                    type: 'sync',
                    message: `Failed to parse message: ${extractErrorMessage(error)}`,
                    docId: this.config.docId,
                    originalError: error instanceof Error ? error : undefined,
                };
                this.onErrorCallback(collabError);
            }
        }
    }

    private handleYrsUpdate(message: YrsUpdateMessage): void {
        try {
            if (!message.encrypted || !Array.isArray(message.encrypted)) {
                throw new Error('Invalid yrs_update message: missing or invalid encrypted field');
            }
            const encrypted = new Uint8Array(message.encrypted);
            this.collabCore.apply_update_encrypted(encrypted);

            if (this.onUpdateCallback) {
                this.onUpdateCallback(this.collabCore.get_text());
            }
        } catch (error) {
            console.error('Failed to apply update:', error);
            if (this.onErrorCallback) {
                const collabError: CollabError = {
                    type: 'decryption',
                    message: extractErrorMessage(error),
                    docId: this.config.docId,
                    originalError: error instanceof Error ? error : undefined,
                };
                this.onErrorCallback(collabError);
            }
        }
    }

    private handleReconnect(): void {
        // Clear any existing timer to prevent old timers from interfering
        if (this.reconnectTimer) {
            clearTimeout(this.reconnectTimer);
            this.reconnectTimer = null;
        }

        if (this.reconnectAttempts < this.maxReconnectAttempts) {
            this.connectionState = 'reconnecting';
            this.reconnectAttempts++;
            const delay = this.reconnectDelay * Math.pow(2, this.reconnectAttempts - 1);
            console.log(`Reconnecting in ${delay}ms (attempt ${this.reconnectAttempts})`);
            this.reconnectTimer = setTimeout(() => {
                // Check if disconnect() was called while waiting
                if (this.maxReconnectAttempts === 0) {
                    return;
                }
                this.connect().catch((error) => {
                    console.error('Reconnect failed:', error);
                    if (this.onErrorCallback) {
                        const collabError: CollabError = {
                            type: 'connection',
                            message: error instanceof Error ? error.message : 'Reconnection failed',
                            docId: this.config.docId,
                            originalError: error instanceof Error ? error : undefined,
                        };
                        this.onErrorCallback(collabError);
                    }
                });
            }, delay);
        } else {
            this.connectionState = 'disconnected';
            // Clear any pending timer
            if (this.reconnectTimer) {
                clearTimeout(this.reconnectTimer);
                this.reconnectTimer = null;
            }
            if (this.onDisconnectCallback) {
                this.onDisconnectCallback('max_retries_exceeded');
            }
        }
    }

    /**
     * Apply a minimal text diff between the current CRDT text and the new text.
     * This avoids clearing and reinserting the entire document, which is
     * critical for proper CRDT collaborative behavior.
     */
    private applyTextDiff(oldText: string, newText: string): void {
        if (oldText === newText) {
            return;
        }

        const oldLen = oldText.length;
        const newLen = newText.length;

        // Find common prefix length
        let prefixLen = 0;
        const maxPrefix = Math.min(oldLen, newLen);
        while (prefixLen < maxPrefix && oldText.charAt(prefixLen) === newText.charAt(prefixLen)) {
            prefixLen++;
        }

        // Find common suffix length (after the prefix)
        let oldEnd = oldLen;
        let newEnd = newLen;
        while (
            oldEnd > prefixLen &&
            newEnd > prefixLen &&
            oldText.charAt(oldEnd - 1) === newText.charAt(newEnd - 1)
        ) {
            oldEnd--;
            newEnd--;
        }

        // Calculate what to delete and insert
        const deleteLen = oldEnd - prefixLen;
        const insertText = newText.slice(prefixLen, newEnd);

        // Apply minimal operations
        if (deleteLen > 0) {
            this.collabCore.delete(prefixLen, deleteLen);
        }
        if (insertText.length > 0) {
            this.collabCore.insert(prefixLen, insertText);
        }
    }

    sendUpdate(text: string): boolean {
        try {
            const currentText = this.collabCore.get_text();

            if (text !== currentText) {
                // Apply minimal diff instead of clearing and reinserting
                this.applyTextDiff(currentText, text);
            }

            const encrypted = this.collabCore.encode_state_encrypted();
            return this.send({
                type: 'yrs_update',
                doc_id: this.config.docId,
                encrypted: [...encrypted],
                epoch: 0,
            });
        } catch (error) {
            console.error('Failed to send update:', error);
            if (this.onErrorCallback) {
                const collabError: CollabError = {
                    type: 'sync',
                    message: extractErrorMessage(error),
                    docId: this.config.docId,
                    originalError: error instanceof Error ? error : undefined,
                };
                this.onErrorCallback(collabError);
            }
            return false;
        }
    }

    onUpdate(callback: UpdateCallback): void {
        this.onUpdateCallback = callback;
    }

    onDisconnect(callback: DisconnectCallback): void {
        this.onDisconnectCallback = callback;
    }

    onError(callback: ErrorCallback): void {
        this.onErrorCallback = callback;
    }

    getConnectionState(): ConnectionState {
        return this.connectionState;
    }

    getText(): string {
        return this.collabCore.get_text();
    }

    disconnect(): void {
        this.maxReconnectAttempts = 0; // Prevent reconnection
        this.connectPromise = null; // Clear any pending connection promise
        this.connectionState = 'disconnected';
        if (this.reconnectTimer) {
            clearTimeout(this.reconnectTimer);
            this.reconnectTimer = null;
        }
        this.ws?.close();
        this.ws = null;
    }
}
