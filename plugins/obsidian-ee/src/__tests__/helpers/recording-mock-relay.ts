/**
 * Shared in-process relay harness for the multi-client integration specs
 * (vault-sync-client, three-party-mls).
 *
 * Two pieces, both extracted verbatim from vault-sync-client.test.ts when a
 * second spec needed them:
 *  - `NodeWebSocket`, a browser-shaped wrapper over `ws` so `CollabClient` (which
 *    talks to the global `WebSocket`) runs unchanged under jest's node env,
 *  - `RecordingMockRelay`, which fans `mls_handshake`/`yrs_update` out to every
 *    OTHER client in arrival order — the same sender-excluded, FIFO fan-out the
 *    real relay does (crates/collab-relay/src/routing.rs `route_message`) — and
 *    records every inbound frame so a test can assert what each client SENT.
 */
import { WebSocket, WebSocketServer } from 'ws';

/** Browser-shaped WebSocket wrapper over `ws`, as in two-user-integration. */
export class NodeWebSocket {
    private ws: WebSocket;
    onopen: (() => void) | null = null;
    onmessage: ((event: { data: string }) => void) | null = null;
    onclose: (() => void) | null = null;
    onerror: ((error: any) => void) | null = null;
    readyState = 0;

    constructor(url: string) {
        this.ws = new WebSocket(url);
        this.ws.on('open', () => {
            this.readyState = 1;
            this.onopen?.();
        });
        this.ws.on('message', (data: Buffer) => {
            this.onmessage?.({ data: data.toString() });
        });
        this.ws.on('close', () => {
            this.readyState = 3;
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

/**
 * The real global `WebSocket`, captured before the shim below replaces it, so a
 * spec's afterAll can put it back.
 */
export const OriginalWebSocket = (global as any).WebSocket;

// Installed on import: CollabClient reads the global at construction time, so
// the shim must be in place before any spec builds a client.
(global as any).WebSocket = NodeWebSocket;

export interface RecordedFrame {
    from: string | null;
    msg: any;
}

/**
 * Mock relay that fans out mls_handshake AND yrs_update to every other client
 * and RECORDS every inbound frame, so tests can assert which frames each client
 * sent and drive a real two-party MLS handshake.
 */
export class RecordingMockRelay {
    private wss: WebSocketServer | null = null;
    private clients: Map<string, WebSocket> = new Map();
    frames: RecordedFrame[] = [];

    async start(port: number): Promise<void> {
        return new Promise((resolve, reject) => {
            this.wss = new WebSocketServer({ port });
            this.wss.on('connection', (ws) => {
                let clientId: string | null = null;
                ws.on('message', (data) => {
                    try {
                        const msg = JSON.parse(data.toString());
                        if (msg.type === 'identify') {
                            clientId = msg.user_id as string;
                            this.clients.set(clientId, ws);
                        }
                        this.frames.push({ from: clientId, msg });
                        if (msg.type === 'subscribe') {
                            ws.send(JSON.stringify({ type: 'subscribed', doc_id: msg.doc_id }));
                        } else if (msg.type === 'yrs_update' || msg.type === 'mls_handshake') {
                            this.clients.forEach((client, id) => {
                                if (id !== clientId && client.readyState === WebSocket.OPEN) {
                                    client.send(JSON.stringify({ ...msg, from: clientId }));
                                }
                            });
                        }
                    } catch (error) {
                        console.error('Relay failed to parse message:', error);
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
        });
    }

    framesFrom(userId: string): RecordedFrame[] {
        return this.frames.filter((f) => f.from === userId);
    }

    async stop(): Promise<void> {
        if (!this.wss) {
            return;
        }
        this.clients.forEach((client) => client.close());
        this.clients.clear();
        return new Promise((resolve) => {
            this.wss!.close(() => {
                this.wss = null;
                resolve();
            });
        });
    }
}
