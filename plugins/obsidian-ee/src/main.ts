import { Plugin, Notice, MarkdownView, PluginSettingTab, App, Setting } from 'obsidian';
import init, { CollabCore } from './wasm/collab_wasm';
import { CollabClient, CollabClientConfig } from './collab-client';
import { EditorSync } from './editor-sync';

interface CollabPluginSettings {
    relayUrl: string;
}

// SECURITY: Default uses ws:// for local development only.
// Production deployments MUST use wss:// (TLS-encrypted WebSocket).
const DEFAULT_SETTINGS: CollabPluginSettings = {
    relayUrl: 'ws://localhost:8080',
};

export default class CollabPlugin extends Plugin {
    settings: CollabPluginSettings = DEFAULT_SETTINGS;
    private collabCore: CollabCore | null = null;
    private collabClient: CollabClient | null = null;
    private editorSync: EditorSync | null = null;
    private wasmInitialized = false;
    private editorChangeHandler: ReturnType<typeof this.app.workspace.on> | null = null;

    async onload() {
        console.log('Loading Obsidian E2E Collaboration plugin');

        await this.loadSettings();

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

            // Add settings tab
            this.addSettingTab(new CollabSettingTab(this.app, this));
        } catch (error) {
            console.error('Failed to initialize WASM:', error);
            new Notice('Failed to load collaboration plugin');
        }
    }

    async loadSettings(): Promise<void> {
        try {
            const loadedData = await this.loadData();
            this.settings = Object.assign({}, DEFAULT_SETTINGS, loadedData);
        } catch (error) {
            console.error('[CollabPlugin] Failed to load settings, using defaults:', error);
            this.settings = { ...DEFAULT_SETTINGS };
            new Notice('Collaboration settings could not be loaded, using defaults');
        }
    }

    async saveSettings(): Promise<void> {
        try {
            await this.saveData(this.settings);
        } catch (error) {
            console.error('[CollabPlugin] Failed to save settings:', error);
            new Notice('Failed to save collaboration settings');
        }
    }

    async initWasm(): Promise<void> {
        if (this.wasmInitialized) {
            // WASM module is already loaded; recreate the core cheaply if a prior
            // stopSession() freed it (F16 lazy re-init). No need to reload the module.
            if (!this.collabCore) {
                this.collabCore = new CollabCore();
            }
            return;
        }

        // Load WASM from plugin directory (import.meta.url doesn't work in Obsidian)
        const pluginDir = this.manifest.dir;
        if (!pluginDir) {
            throw new Error('Plugin directory not found');
        }

        const wasmPath = `${pluginDir}/collab_wasm_bg.wasm`;
        const wasmBuffer = await this.app.vault.adapter.readBinary(wasmPath);

        // Compile the WASM module first - init() expects a compiled module, not raw bytes
        let wasmModule: WebAssembly.Module;
        try {
            wasmModule = await WebAssembly.compile(wasmBuffer);
        } catch (error) {
            if (error instanceof WebAssembly.CompileError) {
                throw new Error(`WASM compilation failed: ${error.message}`);
            }
            throw new Error(
                `Failed to load WASM module: ${error instanceof Error ? error.message : String(error)}`
            );
        }

        try {
            await init(wasmModule);
        } catch (error) {
            throw new Error(
                `WASM initialization failed: ${error instanceof Error ? error.message : String(error)}`
            );
        }

        this.collabCore = new CollabCore();
        this.wasmInitialized = true;
        console.log('WASM initialized successfully');
    }

    async startSession(): Promise<void> {
        // F15: Guard against double-start. Starting a second session without stopping
        // the first would orphan the first CollabClient (its WebSocket stays open) and
        // EditorSync, and overwrite editorChangeHandler so stopSession() could no longer
        // unregister the first handler.
        if (this.collabClient || this.editorSync) {
            new Notice('Collaboration session already active');
            return;
        }

        // F16: stopSession() frees and nulls collabCore; re-initialize it lazily here
        // so we don't hold a live WASM core between sessions.
        if (!this.collabCore) {
            try {
                await this.initWasm();
            } catch (error) {
                console.error('[CollabPlugin] Failed to initialize WASM:', error);
                new Notice('Failed to initialize collaboration plugin');
                return;
            }
        }

        if (!this.collabCore) {
            new Notice('Plugin not initialized');
            return;
        }

        const activeView = this.app.workspace.getActiveViewOfType(MarkdownView);
        if (!activeView) {
            new Notice('Please open a markdown file first');
            return;
        }

        const config: CollabClientConfig = {
            relayUrl: this.settings.relayUrl,
            userId: `user-${Date.now()}`,
            docId: activeView.file?.path || 'unknown',
            // SECURITY: This is a PLACEHOLDER key - all zeros, completely insecure!
            // Production MUST use:
            // 1. Cryptographically-secure random key generation (crypto.getRandomValues)
            // 2. Secure key exchange mechanism (e.g., via MLS handshake)
            // 3. Key stored securely, never hardcoded
            encryptionKey: new Uint8Array(32),
        };

        // SECURITY WARNING: Warn about insecure placeholder key
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

        // F16: Free CollabCore to release WASM memory and null the reference.
        // startSession() re-creates it lazily via initWasm(), so we don't hold a
        // live core between sessions (and onunload() won't allocate one just to free it).
        if (this.collabCore) {
            try {
                this.collabCore.free();
            } catch (error) {
                console.error('[CollabPlugin] Error freeing WASM resources:', error);
            }
            this.collabCore = null;
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

class CollabSettingTab extends PluginSettingTab {
    plugin: CollabPlugin;

    constructor(app: App, plugin: CollabPlugin) {
        super(app, plugin);
        this.plugin = plugin;
    }

    display(): void {
        const { containerEl, plugin } = this;
        containerEl.empty();

        containerEl.createEl('h2', { text: 'E2E Collaboration Settings' });

        new Setting(containerEl)
            .setName('Relay Server URL')
            .setDesc('WebSocket URL of the relay server. Use wss:// for production.')
            .addText((text) =>
                text
                    .setPlaceholder('ws://localhost:8080')
                    .setValue(plugin.settings.relayUrl)
                    .onChange(async (value) => {
                        plugin.settings.relayUrl = value;
                        // saveSettings already handles errors internally
                        await plugin.saveSettings();
                    })
            );
    }
}
