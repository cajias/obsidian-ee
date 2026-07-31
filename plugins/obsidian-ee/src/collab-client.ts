import {
    WasmEncryptedDocument,
    WasmInvite,
    WasmPendingMember,
    generate_key_package,
} from './wasm/collab_wasm';

export type CollabRole = 'owner' | 'joiner';

export interface CollabClientConfig {
    relayUrl: string;
    userId: string;
    docId: string;
    role: CollabRole; // owner creates the MLS group; joiner joins via a Welcome
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

export interface MlsHandshakeMessage {
    type: 'mls_handshake';
    doc_id?: string;
    payload: number[];
    message_type: 'key_package' | 'welcome' | 'commit';
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

    // Validate role
    if (config.role !== 'owner' && config.role !== 'joiner') {
        throw new ConfigValidationError('role must be "owner" or "joiner"');
    }
}

export class CollabClient {
    private ws: WebSocket | null = null;
    private doc: WasmEncryptedDocument | null = null;
    private pending: WasmPendingMember | null = null;
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

    constructor(config: CollabClientConfig) {
        validateConfig(config);
        this.config = config;
    }

    /**
     * Establish the MLS group for this connection. Called from onopen after
     * identify/subscribe so group state is fresh per connection.
     * - owner: creates the group document immediately.
     * - joiner: generates a single-use key package and ships it as an
     *   mls_handshake frame; `this.doc` stays null until the Welcome arrives.
     */
    private establishGroup(): void {
        if (this.config.role === 'owner') {
            this.doc = WasmEncryptedDocument.create(this.config.docId, this.config.userId);
            return;
        }
        this.pending = generate_key_package(this.config.userId);
        this.send({
            type: 'mls_handshake',
            doc_id: this.config.docId,
            payload: [...this.pending.key_package],
            message_type: 'key_package',
        });
    }

    connect(): Promise<void> {
        // Prevent concurrent connection attempts
        if (this.connectPromise) {
            return this.connectPromise;
        }

        this.connectionState = 'connecting';
        this.connectPromise = new Promise<void>((resolve, reject) => {
            // Per-attempt flag: has THIS socket reached onopen yet? Every attempt
            // (initial or reconnect) must settle its promise exactly once, so that
            // .finally() clears connectPromise and the dedup guard above won't keep
            // returning a stale, never-settling promise during a transient outage.
            let hasOpened = false;

            try {
                this.ws = new WebSocket(this.config.relayUrl);

                this.ws.onopen = () => {
                    console.log('Connected to relay server');
                    hasOpened = true;
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

                    this.establishGroup();

                    this.flushMessageQueue();
                    this.reconnectAttempts = 0;
                    resolve();
                };

                this.ws.onmessage = (event) => {
                    this.handleMessage(event.data);
                };

                this.ws.onerror = (error) => {
                    console.error('WebSocket error:', error);
                    if (!hasOpened) {
                        // Socket failed before opening. Reject this attempt's promise so
                        // .finally() clears connectPromise (rejection is delegated to
                        // onclose, which follows onerror, to drive the backoff loop).
                        reject(error);
                    } else if (this.onErrorCallback) {
                        // Post-open error on a live connection: surface via error callback.
                        const collabError: CollabError = {
                            type: 'connection',
                            message: error instanceof Error ? error.message : 'WebSocket error',
                            docId: this.config.docId,
                            originalError: error instanceof Error ? error : undefined,
                        };
                        this.onErrorCallback(collabError);
                    }
                };

                this.ws.onclose = () => {
                    console.log('WebSocket closed');
                    if (!hasOpened) {
                        // Attempt closed before opening (initial OR reconnect). Settle this
                        // attempt's promise (no-op if onerror already rejected it) so the
                        // dedup guard is unblocked, then delegate to the backoff scheduler.
                        // Settling does NOT abort the retry chain — handleReconnect() runs
                        // on its own timer independent of this promise.
                        reject(
                            new Error(
                                this.isInitialConnect
                                    ? 'WebSocket closed during initial connection'
                                    : 'WebSocket closed during reconnection'
                            )
                        );
                        if (!this.isInitialConnect) {
                            this.handleReconnect();
                        }
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
                case 'mls_handshake':
                    this.handleMlsHandshake(message as MlsHandshakeMessage);
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
            // Defense in depth: reject a frame routed for a different document
            // before touching the crypto core. The relay is untrusted, so a
            // mismatched doc_id means a misroute or a cross-document replay attempt.
            // (The AEAD doc_id binding below is the load-bearing guarantee; this
            // rejects the frame early with a clear error.)
            if (message.doc_id !== undefined && message.doc_id !== this.config.docId) {
                throw new Error(
                    `yrs_update doc_id mismatch: expected ${this.config.docId}, got ${message.doc_id}`
                );
            }
            // Fail closed: an update that arrives before the MLS group is
            // established cannot be decrypted. Surface it as an error rather than
            // silently dropping it.
            if (this.doc === null) {
                throw new Error('no MLS group established');
            }
            const encrypted = new Uint8Array(message.encrypted);
            // MLS authenticates and decrypts under the group's current epoch.
            this.doc.apply_encrypted_update(encrypted, BigInt(message.epoch ?? 0));

            if (this.onUpdateCallback) {
                this.onUpdateCallback(this.doc.get_content());
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

    /**
     * Handle the MLS handshake wire protocol (#51):
     * - owner receives a joiner's key_package → builds and sends a Welcome.
     * - joiner receives a Welcome → joins the group, consuming its key package.
     * - either side receives a commit → applies it to advance the epoch.
     */
    private handleMlsHandshake(message: MlsHandshakeMessage): void {
        try {
            if (!message.payload || !Array.isArray(message.payload)) {
                throw new Error('Invalid mls_handshake message: missing or invalid payload');
            }
            const payload = new Uint8Array(message.payload);

            switch (message.message_type) {
                case 'key_package': {
                    // Owner side: only if the group document exists.
                    if (!this.doc) {
                        return;
                    }
                    const invite = this.doc.create_invite(payload);
                    this.send({
                        type: 'mls_handshake',
                        doc_id: this.config.docId,
                        payload: [...invite.welcome],
                        message_type: 'welcome',
                    });
                    break;
                }
                case 'welcome': {
                    // Joiner side: bind the LOCAL docId, never message.doc_id.
                    const invite = WasmInvite.from_welcome(this.config.docId, payload);
                    this.doc = WasmEncryptedDocument.join(invite, this.pending!);
                    this.pending = null;
                    break;
                }
                case 'commit': {
                    this.doc?.process_commit(payload);
                    break;
                }
                default:
                    console.warn(
                        `[CollabClient] Unknown mls_handshake message_type: ${message.message_type}`,
                        message
                    );
                    break;
            }
        } catch (error) {
            console.error('Failed to process MLS handshake:', error);
            if (this.onErrorCallback) {
                const collabError: CollabError = {
                    type: 'sync',
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
            // reconnectTimer is already null here (cleared at entry, only reassigned
            // in the branch above), so no timer cleanup is needed.
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
            this.doc?.delete(prefixLen, deleteLen);
        }
        if (insertText.length > 0) {
            this.doc?.insert(prefixLen, insertText);
        }
    }

    sendUpdate(text: string): boolean {
        // Fail-closed guard (replaces the old all-zeros-key guard): without an
        // established MLS group there is no key to encrypt under, so send NOTHING
        // rather than falling back to a plaintext path.
        if (this.doc === null) {
            return false;
        }
        try {
            const currentText = this.doc.get_content();

            if (text !== currentText) {
                // Apply minimal diff instead of clearing and reinserting
                this.applyTextDiff(currentText, text);
            }

            // MLS encrypts under the group's current epoch; the op carries both.
            const op = this.doc.get_encrypted_update();
            return this.send({
                type: 'yrs_update',
                doc_id: this.config.docId,
                encrypted: [...op.ciphertext],
                epoch: Number(op.epoch),
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
        return this.doc?.get_content() ?? '';
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
        this.doc?.free();
        this.doc = null;
    }
}
