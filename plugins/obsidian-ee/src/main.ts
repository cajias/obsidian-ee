import { Plugin, Notice, MarkdownView } from 'obsidian';
import init, { CollabCore } from './wasm/collab_wasm';
import { CollabClient, CollabClientConfig } from './collab-client';
import { EditorSync } from './editor-sync';

export default class CollabPlugin extends Plugin {
    private collabCore: CollabCore | null = null;
    private collabClient: CollabClient | null = null;
    private editorSync: EditorSync | null = null;
    private wasmInitialized = false;

    async onload() {
        console.log('Loading Obsidian E2E Collaboration plugin');

        try {
            await this.initWasm();
            this.addCommand({
                id: 'start-collab',
                name: 'Start Collaboration Session',
                callback: () => this.startSession(),
            });

            this.addCommand({
                id: 'stop-collab',
                name: 'Stop Collaboration Session',
                callback: () => this.stopSession(),
            });
        } catch (error) {
            console.error('Failed to initialize WASM:', error);
            new Notice('Failed to load collaboration plugin');
        }
    }

    async initWasm(): Promise<void> {
        if (this.wasmInitialized) return;

        await init();
        this.collabCore = new CollabCore();
        this.wasmInitialized = true;
        console.log('WASM initialized successfully');
    }

    async startSession(): Promise<void> {
        if (!this.collabCore) {
            new Notice('Plugin not initialized');
            return;
        }

        const activeView = this.app.workspace.getActiveViewOfType(MarkdownView);
        if (!activeView) {
            new Notice('Please open a markdown file first');
            return;
        }

        // TODO: Get these from settings or modal
        const config: CollabClientConfig = {
            relayUrl: 'ws://localhost:8080',
            userId: `user-${Date.now()}`,
            docId: activeView.file?.path || 'unknown',
            encryptionKey: new Uint8Array(32), // TODO: Generate/share proper key
        };

        try {
            // Create client and editor sync
            this.collabClient = new CollabClient(this.collabCore, config);
            this.editorSync = new EditorSync(this.collabClient);

            // Connect to relay server
            await this.collabClient.connect();

            // Bind to current editor
            this.editorSync.bindToEditor(activeView);

            // Register editor change handler
            this.registerEvent(
                this.app.workspace.on('editor-change', () => {
                    this.editorSync?.onLocalChange();
                })
            );

            new Notice('Collaboration session started');
        } catch (error) {
            console.error('Failed to start collaboration:', error);
            new Notice('Failed to connect to collaboration server');
            this.stopSession();
        }
    }

    stopSession(): void {
        if (this.editorSync) {
            this.editorSync.unbind();
            this.editorSync = null;
        }

        if (this.collabClient) {
            this.collabClient.disconnect();
            this.collabClient = null;
        }

        new Notice('Collaboration session stopped');
    }

    onunload() {
        console.log('Unloading Obsidian E2E Collaboration plugin');

        this.stopSession();

        if (this.collabCore) {
            this.collabCore.free();
            this.collabCore = null;
        }
    }
}
