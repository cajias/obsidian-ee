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
    signature?: number[];
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
    private connectionState: 'connected' | 'connecting' | 'disconnected' | 'reconnecting' =
        'disconnected';
    private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
    private isInitialConnect = true;

    constructor(collabCore: CollabCore, config: CollabClientConfig) {
        this.collabCore = collabCore;
        this.config = config;
        this.collabCore.set_encryption_key(config.encryptionKey);
    }

    connect(): Promise<void> {
        this.connectionState = 'connecting';
        return new Promise((resolve, reject) => {
            try {
                this.ws = new WebSocket(this.config.relayUrl);

                this.ws.onopen = () => {
                    console.log('Connected to relay server');
                    this.connectionState = 'connected';
                    this.isInitialConnect = false;

                    // Critical: verify initialization messages are sent
                    const identified = this.sendIdentify();
                    const subscribed = this.subscribe();

                    if (!identified || !subscribed) {
                        const error = new Error('Failed to send initialization messages');
                        console.error('[CollabClient]', error.message);
                        this.ws?.close();
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
        });
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
        // Queue message instead of silently dropping it
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
            }
        } catch (error) {
            console.error('Failed to parse message:', error);
            if (this.onErrorCallback) {
                const collabError: CollabError = {
                    type: 'sync',
                    message: `Failed to parse message: ${error instanceof Error ? error.message : String(error)}`,
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
                    message: error instanceof Error ? error.message : String(error),
                    docId: this.config.docId,
                    originalError: error instanceof Error ? error : undefined,
                };
                this.onErrorCallback(collabError);
            }
        }
    }

    private handleReconnect(): void {
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

    sendUpdate(text: string): boolean {
        try {
            // Insert the text at the end (simple append for MVP)
            const currentText = this.collabCore.get_text();
            if (text !== currentText) {
                // Clear and reinsert (simple approach for MVP)
                this.collabCore.delete(0, currentText.length);
                this.collabCore.insert(0, text);
            }

            const encrypted = this.collabCore.encode_state_encrypted();
            return this.send({
                type: 'yrs_update',
                doc_id: this.config.docId,
                encrypted: [...encrypted],
                epoch: 0,
                signature: [],
            });
        } catch (error) {
            console.error('Failed to send update:', error);
            if (this.onErrorCallback) {
                const collabError: CollabError = {
                    type: 'sync',
                    message: error instanceof Error ? error.message : String(error),
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

    getConnectionState(): string {
        return this.connectionState;
    }

    getText(): string {
        return this.collabCore.get_text();
    }

    disconnect(): void {
        this.maxReconnectAttempts = 0; // Prevent reconnection
        if (this.reconnectTimer) {
            clearTimeout(this.reconnectTimer);
            this.reconnectTimer = null;
        }
        this.ws?.close();
        this.ws = null;
    }
}
