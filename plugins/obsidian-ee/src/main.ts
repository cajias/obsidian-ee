import { Plugin, Notice, MarkdownView } from 'obsidian';
import init, { CollabCore } from './wasm/collab_wasm';
import { CollabClient, CollabClientConfig } from './collab-client';
import { EditorSync } from './editor-sync';

export default class CollabPlugin extends Plugin {
    private collabCore: CollabCore | null = null;
    private collabClient: CollabClient | null = null;
    private editorSync: EditorSync | null = null;
    private wasmInitialized = false;
    private editorChangeHandler: ReturnType<typeof this.app.workspace.on> | null = null;

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

        // Warn about insecure placeholder key
        console.warn(
            '[CollabPlugin] SECURITY WARNING: Using placeholder encryption key. ' +
                'This is insecure and should only be used for development.'
        );
        new Notice('Warning: Using insecure placeholder encryption key', 5000);

        try {
            // Create client and editor sync
            this.collabClient = new CollabClient(this.collabCore, config);
            this.editorSync = new EditorSync(this.collabClient);

            // Register error and disconnect callbacks
            this.collabClient.onError((error) => {
                console.error('[CollabPlugin] Collaboration error:', error);
                new Notice(`Collaboration error: ${error.message}`);
            });

            this.collabClient.onDisconnect((reason) => {
                console.warn('[CollabPlugin] Disconnected:', reason);
                new Notice(`Collaboration disconnected: ${reason}`);
                this.stopSession();
            });

            this.editorSync.setErrorCallback((error) => {
                console.error('[CollabPlugin] Editor sync error:', error);
                new Notice(`Sync error: ${error.message}`);
            });

            // Connect to relay server
            await this.collabClient.connect();

            // Bind to current editor
            this.editorSync.bindToEditor(activeView);

            // Register editor change handler
            this.editorChangeHandler = this.app.workspace.on('editor-change', () => {
                this.editorSync?.onLocalChange();
            });
            this.registerEvent(this.editorChangeHandler);

            new Notice('Collaboration session started');
        } catch (error) {
            console.error('Failed to start collaboration:', error);
            new Notice('Failed to connect to collaboration server');
            this.stopSession();
        }
    }

    stopSession(): void {
        // Unregister editor change handler
        if (this.editorChangeHandler) {
            this.app.workspace.offref(this.editorChangeHandler);
            this.editorChangeHandler = null;
        }

        if (this.editorSync) {
            this.editorSync.unbind();
            this.editorSync = null;
        }

        if (this.collabClient) {
            this.collabClient.disconnect();
            this.collabClient = null;
        }

        // Free and recreate CollabCore to prevent memory leak
        if (this.collabCore) {
            try {
                this.collabCore.free();
            } catch (error) {
                console.error('[CollabPlugin] Error freeing WASM resources:', error);
            }
            this.collabCore = new CollabCore();
        }

        new Notice('Collaboration session stopped');
    }

    onunload() {
        console.log('Unloading Obsidian E2E Collaboration plugin');

        try {
            this.stopSession();
        } catch (error) {
            console.error('[CollabPlugin] Error stopping session during unload:', error);
        }

        if (this.collabCore) {
            try {
                this.collabCore.free();
            } catch (error) {
                console.error('[CollabPlugin] Error freeing WASM resources:', error);
            }
            this.collabCore = null;
        }
    }
}
