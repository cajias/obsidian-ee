import { WebSocketServer, WebSocket } from 'ws';

export class MockRelay {
    private wss: WebSocketServer | null = null;
    private clients: Map<string, WebSocket> = new Map();

    async start(port: number = 8080): Promise<void> {
        return new Promise((resolve) => {
            this.wss = new WebSocketServer({ port });
            this.wss.on('connection', (ws) => {
                ws.on('message', (data) => {
                    const msg = JSON.parse(data.toString());
                    if (msg.type === 'identify') {
                        this.clients.set(msg.user_id, ws);
                    } else if (msg.type === 'subscribe') {
                        // Send subscription acknowledgment like the real server
                        ws.send(JSON.stringify({ type: 'subscribed', doc_id: msg.doc_id }));
                    } else {
                        // Broadcast to other clients
                        this.broadcast(ws, data.toString());
                    }
                });
            });
            this.wss.on('listening', () => resolve());
        });
    }

    private broadcast(sender: WebSocket, message: string): void {
        this.clients.forEach((client) => {
            if (client !== sender && client.readyState === WebSocket.OPEN) {
                client.send(message);
            }
        });
    }

    async stop(): Promise<void> {
        if (this.wss) {
            this.wss.close();
            this.wss = null;
        }
    }
}
