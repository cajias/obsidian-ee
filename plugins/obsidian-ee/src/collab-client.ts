import { CollabCore } from './wasm/collab_wasm';

export interface CollabClientConfig {
    relayUrl: string;
    userId: string;
    docId: string;
    encryptionKey: Uint8Array;
}

export type UpdateCallback = (text: string) => void;

export class CollabClient {
    private ws: WebSocket | null = null;
    private collabCore: CollabCore;
    private config: CollabClientConfig;
    private onUpdateCallback: UpdateCallback | null = null;
    private reconnectAttempts = 0;
    private maxReconnectAttempts = 5;
    private reconnectDelay = 1000;

    constructor(collabCore: CollabCore, config: CollabClientConfig) {
        this.collabCore = collabCore;
        this.config = config;
        this.collabCore.set_encryption_key(config.encryptionKey);
    }

    connect(): Promise<void> {
        return new Promise((resolve, reject) => {
            try {
                this.ws = new WebSocket(this.config.relayUrl);

                this.ws.onopen = () => {
                    console.log('Connected to relay server');
                    this.sendIdentify();
                    this.subscribe();
                    this.reconnectAttempts = 0;
                    resolve();
                };

                this.ws.onmessage = (event) => {
                    this.handleMessage(event.data);
                };

                this.ws.onerror = (error) => {
                    console.error('WebSocket error:', error);
                    reject(error);
                };

                this.ws.onclose = () => {
                    console.log('WebSocket closed');
                    this.handleReconnect();
                };
            } catch (error) {
                reject(error);
            }
        });
    }

    private sendIdentify(): void {
        this.send({
            type: 'identify',
            user_id: this.config.userId,
        });
    }

    private subscribe(): void {
        this.send({
            type: 'subscribe',
            doc_id: this.config.docId,
        });
    }

    private send(message: object): void {
        if (this.ws?.readyState === WebSocket.OPEN) {
            this.ws.send(JSON.stringify(message));
        }
    }

    private handleMessage(data: string): void {
        try {
            const message = JSON.parse(data);

            switch (message.type) {
                case 'yrs_update':
                    this.handleYrsUpdate(message);
                    break;
                case 'subscribed':
                    console.log('Subscribed to document:', message.doc_id);
                    break;
                case 'error':
                    console.error('Server error:', message.message);
                    break;
            }
        } catch (error) {
            console.error('Failed to parse message:', error);
        }
    }

    private handleYrsUpdate(message: any): void {
        try {
            const encrypted = new Uint8Array(message.encrypted);
            this.collabCore.apply_update_encrypted(encrypted);

            if (this.onUpdateCallback) {
                this.onUpdateCallback(this.collabCore.get_text());
            }
        } catch (error) {
            console.error('Failed to apply update:', error);
        }
    }

    private handleReconnect(): void {
        if (this.reconnectAttempts < this.maxReconnectAttempts) {
            this.reconnectAttempts++;
            const delay = this.reconnectDelay * Math.pow(2, this.reconnectAttempts - 1);
            console.log(`Reconnecting in ${delay}ms (attempt ${this.reconnectAttempts})`);
            setTimeout(() => this.connect(), delay);
        }
    }

    sendUpdate(text: string): void {
        // Insert the text at the end (simple append for MVP)
        const currentText = this.collabCore.get_text();
        if (text !== currentText) {
            // Clear and reinsert (simple approach for MVP)
            this.collabCore.delete(0, currentText.length);
            this.collabCore.insert(0, text);
        }

        const encrypted = this.collabCore.encode_state_encrypted();
        this.send({
            type: 'yrs_update',
            doc_id: this.config.docId,
            encrypted: Array.from(encrypted),
            epoch: 0,
            signature: [],
        });
    }

    onUpdate(callback: UpdateCallback): void {
        this.onUpdateCallback = callback;
    }

    getText(): string {
        return this.collabCore.get_text();
    }

    disconnect(): void {
        this.maxReconnectAttempts = 0; // Prevent reconnection
        this.ws?.close();
        this.ws = null;
    }
}
