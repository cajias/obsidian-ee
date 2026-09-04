import { Plugin, Notice, MarkdownView, PluginSettingTab, App, Setting } from 'obsidian';
import type { EventRef } from 'obsidian';
import init, { WasmVaultSync, manifest_doc_id } from './wasm/collab_wasm';
import type { WasmSyncAction } from './wasm/collab_wasm';
import { CollabClient, CollabClientConfig, CollabRole, extractErrorMessage } from './collab-client';
import { EditorSync } from './editor-sync';

interface CollabPluginSettings {
    relayUrl: string;
}

// SECURITY: Default uses ws:// for local development only.
// Production deployments MUST use wss:// (TLS-encrypted WebSocket).
const DEFAULT_SETTINGS: CollabPluginSettings = {
    relayUrl: 'ws://localhost:8080',
};

/**
 * Trust boundary for remote manifest paths (#32). The manifest is network-fed,
 * so a malicious peer could announce traversal paths; reject anything that is
 * empty, absolute, contains a backslash, or has `..` (or empty) segments BEFORE
 * it reaches any vault API.
 */
function isSafeVaultPath(path: string): boolean {
    if (!path || path.startsWith('/') || path.includes('\\')) {
        return false;
    }
    return path.split('/').every((segment) => segment !== '' && segment !== '..');
}

export default class CollabPlugin extends Plugin {
    settings: CollabPluginSettings = DEFAULT_SETTINGS;
    private collabClient: CollabClient | null = null;
    private editorSync: EditorSync | null = null;
    private vaultSync: WasmVaultSync | null = null;
    private vaultEventRefs: EventRef[] = [];
    // Paths currently being created FROM a remote manifest: the vault 'create'
    // event they fire must not echo back out as a new manifest update.
    private materializing = new Set<string>();
    private wasmInitialized = false;
    private editorChangeHandler: ReturnType<typeof this.app.workspace.on> | null = null;

    async onload() {
        console.log('Loading Obsidian E2E Collaboration plugin');

        await this.loadSettings();

        try {
            await this.initWasm();
            // Two entry points: the owner creates the MLS group; the joiner joins
            // an existing one via a Welcome. Role is chosen by which command runs,
            // not persisted in settings.
            this.addCommand({
                id: 'start-collab-owner',
                name: 'Start Collaboration (create group)',
                callback: () => this.startSession('owner'),
            });

            this.addCommand({
                id: 'start-collab-join',
                name: 'Join Collaboration',
                callback: () => this.startSession('joiner'),
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
            const loadedData = (await this.loadData()) as Record<string, unknown> | null;
            // Copy ONLY known settings fields. A data.json written by an older
            // plugin version can carry legacy fields (including plaintext key
            // material from the removed pre-MLS crypto); picking known fields
            // drops them in memory.
            this.settings = {
                relayUrl:
                    typeof loadedData?.relayUrl === 'string'
                        ? loadedData.relayUrl
                        : DEFAULT_SETTINGS.relayUrl,
            };
            // Purge legacy fields from DISK immediately: without this rewrite the
            // old plaintext pre-MLS key would linger in data.json until the user
            // happened to edit a setting and trigger saveSettings.
            if (loadedData && Object.keys(loadedData).some((key) => !(key in this.settings))) {
                await this.saveSettings();
            }
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
            throw new Error(`Failed to load WASM module: ${extractErrorMessage(error)}`);
        }

        try {
            await init(wasmModule);
        } catch (error) {
            throw new Error(`WASM initialization failed: ${extractErrorMessage(error)}`);
        }

        this.wasmInitialized = true;
        console.log('WASM initialized successfully');
    }

    async startSession(role: CollabRole): Promise<void> {
        // F15: Guard against double-start. Starting a second session without stopping
        // the first would orphan the first CollabClient (its WebSocket stays open) and
        // EditorSync, and overwrite editorChangeHandler so stopSession() could no longer
        // unregister the first handler.
        if (this.collabClient || this.editorSync || this.vaultSync) {
            new Notice('Collaboration session already active');
            return;
        }

        try {
            await this.initWasm();
        } catch (error) {
            console.error('[CollabPlugin] Failed to initialize WASM:', error);
            new Notice('Failed to initialize collaboration plugin');
            return;
        }

        const activeView = this.app.workspace.getActiveViewOfType(MarkdownView);
        if (!activeView) {
            new Notice('Please open a markdown file first');
            return;
        }

        // Vault sync (#32): default scope (whole vault, .md only), deletions and
        // renames propagated. The manifest CRDT rides the same relay connection
        // under its own MLS group, established by the same owner/joiner handshake.
        this.vaultSync = new WasmVaultSync([], [], true, true);

        const config: CollabClientConfig = {
            relayUrl: this.settings.relayUrl,
            userId: `user-${Date.now()}`,
            docId: activeView.file?.path || 'unknown',
            // owner creates the MLS group; joiner joins via a Welcome. No key input:
            // the group's keys are derived by MLS, so a session fails closed until a
            // group is established (CollabClient.sendUpdate returns false, no plaintext).
            role,
            vaultSync: this.vaultSync,
            manifestDocId: manifest_doc_id(),
        };

        try {
            // Create client and editor sync. CollabClient owns the MLS document
            // lifetime and frees it in destroy() (see stopSession).
            this.collabClient = new CollabClient(config);
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

            // Materialize files announced by remote manifests (#32): a note
            // created on one client appears as a file on the others.
            this.collabClient.onManifestPaths((paths) => this.materializeRemotePaths(paths));

            // Connect to relay server
            await this.collabClient.connect();

            // Bind to current editor
            this.editorSync.bindToEditor(activeView);

            // Register editor change handler
            this.editorChangeHandler = this.app.workspace.on('editor-change', () => {
                this.editorSync?.onLocalChange();
            });
            this.registerEvent(this.editorChangeHandler);

            // Route vault file events into manifest updates (#32).
            this.registerVaultHandlers();

            new Notice('Collaboration session started');
        } catch (error) {
            console.error('Failed to start collaboration:', error);
            new Notice('Failed to connect to collaboration server');
            this.stopSession();
        }
    }

    /**
     * Wire vault file events into manifest updates (#32). Refs are tracked for
     * stopSession and also registered with Obsidian for unload cleanup.
     */
    private registerVaultHandlers(): void {
        const refs: EventRef[] = [
            this.app.vault.on('create', (file) => {
                // Echo guard: a create we performed ourselves while materializing
                // a remote manifest path must not loop back into an outbound update.
                if (this.materializing.has(file.path)) {
                    return;
                }
                this.handleVaultAction(() => this.vaultSync!.handle_created(file.path));
            }),
            this.app.vault.on('delete', (file) => {
                this.handleVaultAction(() => this.vaultSync!.handle_deleted(file.path));
            }),
            this.app.vault.on('rename', (file, oldPath) => {
                this.handleVaultAction(() => this.vaultSync!.handle_renamed(oldPath, file.path));
            }),
        ];
        for (const ref of refs) {
            this.vaultEventRefs.push(ref);
            this.registerEvent(ref);
        }
    }

    /** Run a vault-sync action and broadcast its manifest update unless ignored. */
    private handleVaultAction(run: () => WasmSyncAction): void {
        if (!this.vaultSync || !this.collabClient) {
            return;
        }
        let action: WasmSyncAction | undefined;
        try {
            action = run();
            if (action.kind !== 'ignored') {
                this.collabClient.sendManifestUpdate(action.manifest_update);
            }
        } catch (error) {
            console.error('[CollabPlugin] Vault sync error:', error);
            new Notice(`Vault sync error: ${extractErrorMessage(error)}`);
        } finally {
            // Free the WASM-owned action even when run() or the send throws.
            action?.free();
        }
    }

    /**
     * Create vault files for paths announced by a remote manifest (#32).
     * Unsafe paths are rejected at the trust boundary (see isSafeVaultPath) and
     * surfaced as errors; failures never throw out of the manifest callback.
     */
    private async materializeRemotePaths(paths: string[]): Promise<void> {
        for (const path of paths) {
            if (!isSafeVaultPath(path)) {
                console.error('[CollabPlugin] Rejected unsafe remote manifest path:', path);
                new Notice(`Rejected unsafe synced path: ${path}`);
                continue;
            }
            if (this.app.vault.getAbstractFileByPath(path)) {
                continue;
            }
            this.materializing.add(path);
            try {
                const parent = path.split('/').slice(0, -1).join('/');
                if (parent && !this.app.vault.getAbstractFileByPath(parent)) {
                    await this.app.vault.createFolder(parent);
                }
                await this.app.vault.create(path, '');
            } catch (error) {
                console.error('[CollabPlugin] Failed to materialize remote file:', path, error);
                new Notice(`Failed to create synced file ${path}: ${extractErrorMessage(error)}`);
            } finally {
                this.materializing.delete(path);
            }
        }
    }

    stopSession(): void {
        // Unregister editor change handler
        if (this.editorChangeHandler) {
            this.app.workspace.offref(this.editorChangeHandler);
            this.editorChangeHandler = null;
        }

        // Unregister vault event handlers and free the vault-sync manager (#32).
        for (const ref of this.vaultEventRefs) {
            this.app.vault.offref(ref);
        }
        this.vaultEventRefs = [];
        if (this.vaultSync) {
            try {
                this.vaultSync.free();
            } catch (error) {
                console.error('[CollabPlugin] Error freeing vault sync resources:', error);
            }
            this.vaultSync = null;
        }

        if (this.editorSync) {
            this.editorSync.unbind();
            this.editorSync = null;
        }

        // CollabClient owns the MLS document and frees it in destroy(). NOT
        // disconnect(): that one deliberately KEEPS an established group so a
        // reconnecting client resumes it instead of building a fresh epoch-0
        // group. Ending the session is what releases the wasm handles.
        if (this.collabClient) {
            this.collabClient.destroy();
            this.collabClient = null;
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
