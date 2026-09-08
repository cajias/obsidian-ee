import { App, MarkdownView, Notice, Plugin, PluginSettingTab, Setting } from 'obsidian';
import type { EventRef } from 'obsidian';
import init, { WasmVaultSync, manifest_doc_id } from './wasm/collab_wasm';
import type { WasmSyncAction } from './wasm/collab_wasm';
import { CollabClient, CollabClientConfig, CollabRole, extractErrorMessage } from './collab-client';
import { EditorSync } from './editor-sync';

interface CollabPluginSettings {
    relayUrl: string;
    /**
     * User ids this vault admits to the MLS group of a document it hosts (#71).
     *
     * Empty admits nobody, which is the safe default: an owner that has not said
     * who may join has authorized no one.
     */
    allowedJoiners: string[];
}

// SECURITY: Default uses ws:// for local development only.
// Production deployments MUST use wss:// (TLS-encrypted WebSocket).
const DEFAULT_SETTINGS: CollabPluginSettings = {
    relayUrl: 'ws://localhost:8080',
    allowedJoiners: [],
};

/** Split the comma-separated allowlist setting into trimmed, non-empty ids. */
function parseAllowedJoiners(raw: string): string[] {
    return raw
        .split(',')
        .map((id) => id.trim())
        .filter((id) => id.length > 0);
}

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
    /** The at-rest key for MLS snapshots, resolved once per session (#93). */
    private snapshotKey: Uint8Array | null = null;
    /**
     * The user id THIS session runs under.
     *
     * Persisted with the snapshot and reused on restore: a restored group's MLS
     * leaf still carries the id it was created with, so minting under a fresh
     * `user-${Date.now()}` would leave the leaf identity and the wire identity
     * disagreeing. The capability still verifies either way (the relay binds the
     * connection's identified uid), but the divergence is real and free to avoid.
     */
    private sessionUserId = '';
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
                // Anything that is not a list of strings on disk reads as the
                // empty list, so a corrupt data.json fails closed rather than
                // handing `allowedJoiners` something `includes` would mis-answer.
                allowedJoiners: Array.isArray(loadedData?.allowedJoiners)
                    ? loadedData.allowedJoiners.filter((id): id is string => typeof id === 'string')
                    : [...DEFAULT_SETTINGS.allowedJoiners],
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

    /**
     * Persist the allowed-joiners list AND push it to a running session (#71).
     *
     * `CollabClient` copies the list at construction, so an edit that only
     * touched settings left a live owner enforcing the old one. Admission is
     * deny-by-default, so the normal first-run flow is: the joiner starts a
     * session to learn its id and is refused, the owner pastes that id in — and
     * that paste has to reach the client that is already running, or both sides
     * have to stop and start before anyone can join.
     */
    async setAllowedJoiners(ids: string[]): Promise<void> {
        this.settings.allowedJoiners = ids;
        this.collabClient?.setAllowedJoiners(ids);
        await this.saveSettings();
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

        // Resume the MLS groups a previous session saved (#93). Without this a
        // stopSession() -> startSession() cycle builds a fresh epoch-0 group
        // whose TOFU register_doc_key the relay refuses against the document it
        // already anchored, and the session goes content-blind.
        //
        // Every failure here degrades to a fresh bootstrap rather than aborting:
        // an unreadable key or state file costs content resumption, and refusing
        // to start would turn that into no session at all.
        let saved: { snapshots: Record<string, Uint8Array>; userId?: string } = { snapshots: {} };
        try {
            this.snapshotKey = await this.atRestKey();
            saved = await this.loadMlsState();
        } catch (error) {
            console.warn('[CollabPlugin] MLS state unavailable; starting fresh:', error);
            this.snapshotKey = null;
        }
        // Reuse the persisted id so the restored group's MLS leaf and the wire
        // identity agree; a first-ever session mints a new one.
        this.sessionUserId = saved.userId ?? `user-${Date.now()}`;
        // Surfaced because it is the value an OWNER has to paste into its
        // 'Allowed joiners' setting (#71): an allowlist whose entries cannot be
        // discovered is a gate nobody can open.
        console.log(`[CollabPlugin] This session's user id: ${this.sessionUserId}`);

        const config: CollabClientConfig = {
            relayUrl: this.settings.relayUrl,
            userId: this.sessionUserId,
            docId: activeView.file?.path || 'unknown',
            snapshots: saved.snapshots,
            snapshotKey: this.snapshotKey ?? undefined,
            // owner creates the MLS group; joiner joins via a Welcome. No key input:
            // the group's keys are derived by MLS, so a session fails closed until a
            // group is established (CollabClient.sendUpdate returns false, no plaintext).
            role,
            // Who this owner will admit (#71). Ignored by a joiner, which never
            // answers a key package.
            allowedJoiners: this.settings.allowedJoiners,
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

    /** Where the encrypted MLS snapshots live: the plugin's own folder. */
    private mlsStatePath(): string {
        return `${this.manifest.dir}/mls-state.json`;
    }

    /** Where the at-rest key lives — a SEPARATE file from the blobs it protects. */
    private mlsKeyPath(): string {
        return `${this.manifest.dir}/mls-key.bin`;
    }

    /**
     * Load (or on first use, generate) the 32-byte key that encrypts the MLS
     * snapshots.
     *
     * It lives in its own file and NEVER in `data.json`: settings are the thing
     * a user copies between machines or pastes into a bug report, and a key
     * stored beside the blobs it protects protects nothing.
     *
     * ponytail: a plain key file. Obsidian's DataAdapter cannot set file modes,
     * so unlike the CLI's 0600 this inherits the vault's permissions — anything
     * that can read the vault can read the key. The upgrade path is Electron's
     * `safeStorage` (OS keychain), which would bind the key to the user account;
     * swap the body of this method and nothing else changes.
     */
    private async atRestKey(): Promise<Uint8Array> {
        const adapter = this.app.vault.adapter;
        const path = this.mlsKeyPath();
        if (await adapter.exists(path)) {
            const key = new Uint8Array(await adapter.readBinary(path));
            if (key.length === 32) {
                return key;
            }
            // Refuse rather than regenerate. Overwriting would mint a key that
            // opens none of the existing blobs, permanently orphaning every
            // saved group — the CLI refuses for the same reason. startSession
            // catches this and the session runs unpersisted.
            throw new Error(
                `${path} is ${key.length} bytes, expected 32 — refusing to replace a ` +
                    'malformed at-rest key, which would orphan every saved group'
            );
        }
        const key = new Uint8Array(32);
        crypto.getRandomValues(key);
        await adapter.writeBinary(path, key.buffer as ArrayBuffer);
        return key;
    }

    /**
     * Read the MLS groups a previous session saved, keyed by document id.
     *
     * Returns empty on ANY failure — a missing, corrupt, or unreadable state
     * file must degrade to a fresh bootstrap, never block a session.
     */
    private async loadMlsState(): Promise<{
        snapshots: Record<string, Uint8Array>;
        userId?: string;
    }> {
        try {
            const adapter = this.app.vault.adapter;
            if (!(await adapter.exists(this.mlsStatePath()))) {
                return { snapshots: {} };
            }
            const raw = new TextDecoder().decode(await adapter.readBinary(this.mlsStatePath()));
            const parsed = JSON.parse(raw) as {
                snapshots?: Record<string, number[]>;
                userId?: string;
            };
            // `Object.fromEntries`, not a computed assignment in a loop: the keys
            // come from a JSON file, so `"__proto__"` would set the prototype
            // rather than add an entry. `fromEntries` defines an own property
            // for every key, including that one.
            const snapshots = Object.fromEntries(
                Object.entries(parsed.snapshots ?? {}).map(([docId, bytes]) => [
                    docId,
                    new Uint8Array(bytes),
                ])
            ) as Record<string, Uint8Array>;
            return { snapshots, userId: parsed.userId };
        } catch (error) {
            console.warn('[CollabPlugin] Could not read saved MLS state; starting fresh:', error);
            return { snapshots: {} };
        }
    }

    /** Persist the MLS groups this session established, encrypted at rest. */
    private async saveMlsState(blobs: Record<string, Uint8Array>, userId: string): Promise<void> {
        // Plain number arrays rather than base64: a snapshot is a few KB, so
        // the ~3x on disk buys not needing an encoder at all. `JSON.stringify`
        // of a Uint8Array yields an object keyed by index, not an array, so the
        // conversion is explicit in both directions.
        const snapshots = Object.fromEntries(
            Object.entries(blobs).map(([docId, blob]) => [docId, [...blob]])
        ) as Record<string, number[]>;
        const body = JSON.stringify({ snapshots, userId }, null, 2);
        await this.app.vault.adapter.writeBinary(
            this.mlsStatePath(),
            new TextEncoder().encode(body).buffer as ArrayBuffer
        );
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
            // Snapshot BEFORE destroy(), which frees the wasm handles: taken
            // afterwards this returns nothing, and the next startSession would
            // build a fresh epoch-0 group whose TOFU registration the relay
            // refuses against the already-anchored document (#93 path 3).
            //
            // ponytail: fire-and-forget, because stopSession is sync and
            // saveData is not — onunload may exit before the write lands. Await
            // it if surviving a hard quit matters.
            // The key is resolved at session START and cached, so the snapshot
            // itself is synchronous and destroy() is not deferred behind a
            // promise. Only the WRITE is async.
            // The snapshot is best-effort; freeing the client is not. A throw
            // here used to skip destroy() below, leaving the wasm handles alive
            // and `collabClient` non-null, so startSession's "already active"
            // guard then refused every restart — and onDisconnect calls
            // stopSession, so it did not need the user to touch the stop command.
            // Ending a session must be total.
            if (this.snapshotKey) {
                try {
                    const blobs = this.collabClient.snapshot(this.snapshotKey);
                    void this.saveMlsState(blobs, this.sessionUserId).catch((error) =>
                        console.error('[CollabPlugin] Could not save MLS state:', error)
                    );
                } catch (error) {
                    console.error('[CollabPlugin] Could not snapshot MLS state:', error);
                }
            }
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

        new Setting(containerEl)
            .setName('Allowed joiners')
            .setDesc(
                'Comma-separated user ids admitted to documents you host. Leave ' +
                    'empty to admit nobody. A joiner sees its id in the console when ' +
                    'it starts a session; it must be exchanged out of band. This ' +
                    'applies immediately, including to a session already running — ' +
                    'a joiner you refused is told its request timed out and retries ' +
                    'the next time it connects.'
            )
            .addText((text) =>
                text
                    .setPlaceholder('user-1730000000000, user-1730000000001')
                    .setValue(plugin.settings.allowedJoiners.join(', '))
                    .onChange(async (value) => {
                        await plugin.setAllowedJoiners(parseAllowedJoiners(value));
                    })
            );
    }
}
