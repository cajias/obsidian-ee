/**
 * Plugin wiring for vault sync (#32): startSession constructs a WasmVaultSync,
 * hands it (plus the manifest doc id) to the CollabClient, and routes vault
 * create/delete/rename events into manifest updates. Ignored (out-of-scope)
 * events send NOTHING. The lifecycle is idempotent: double-start registers
 * nothing twice, stopSession unregisters handlers and frees the WasmVaultSync.
 *
 * Mirrors main.test.ts: obsidian, wasm, collab-client, editor-sync all mocked.
 * Post-#68 the plugin is MLS-only — startSession takes a role and there is no
 * encryption key input; the manifest gets its own MLS group inside the client.
 */

import { jest, describe, it, expect, beforeEach, afterEach } from '@jest/globals';

const MANIFEST_ID = '__vault_manifest__';

// Mock WebAssembly.compile for WASM loading
const mockWasmModule = {};
const mockCompile = jest
    .fn<(bytes: BufferSource) => Promise<WebAssembly.Module>>()
    .mockResolvedValue(mockWasmModule as WebAssembly.Module);
(global as unknown as { WebAssembly: typeof WebAssembly }).WebAssembly = {
    ...WebAssembly,
    compile: mockCompile,
};

jest.unstable_mockModule('obsidian', () => ({
    Plugin: class {
        app: any;
        manifest: any;
        constructor(app: any, manifest: any) {
            this.app = app;
            this.manifest = manifest;
        }
        addCommand(_cmd: any): void {}
        addSettingTab(_tab: any): void {}
        registerEvent(_event: any): void {}
        loadData(): Promise<any> {
            return Promise.resolve({ relayUrl: 'ws://localhost:8080' });
        }
        saveData(_data: any): Promise<void> {
            return Promise.resolve();
        }
    },
    PluginSettingTab: class {
        app: any;
        plugin: any;
        containerEl: any;
        constructor(app: any, plugin: any) {
            this.app = app;
            this.plugin = plugin;
            this.containerEl = { empty: jest.fn(), createEl: jest.fn() };
        }
    },
    Setting: jest.fn().mockImplementation(() => ({
        setName: jest.fn().mockReturnThis(),
        setDesc: jest.fn().mockReturnThis(),
        addText: jest.fn().mockReturnThis(),
    })),
    Notice: jest.fn(),
    MarkdownView: class {},
}));

/** A controllable WasmSyncAction-shaped object. */
function makeAction(kind: string, path: string, newPath?: string) {
    return {
        kind,
        path,
        new_path: newPath,
        manifest_update: new Uint8Array([9, 9, 9]),
        free: jest.fn(),
    };
}

const mockVaultSyncInstance = {
    handle_created: jest.fn(),
    handle_deleted: jest.fn(),
    handle_renamed: jest.fn(),
    apply_remote_manifest: jest.fn(),
    list_files: jest.fn(),
    free: jest.fn(),
};
const MockWasmVaultSync = jest.fn().mockImplementation(() => mockVaultSyncInstance);

const mockWasmInit = jest.fn<() => Promise<void>>().mockResolvedValue(undefined);

jest.unstable_mockModule('../wasm/collab_wasm', () => ({
    __esModule: true,
    default: mockWasmInit,
    WasmVaultSync: MockWasmVaultSync,
    manifest_doc_id: jest.fn().mockReturnValue(MANIFEST_ID),
}));

const mockClientInstance = {
    connect: jest.fn<() => Promise<void>>().mockResolvedValue(undefined),
    disconnect: jest.fn(),
    getText: jest.fn().mockReturnValue(''),
    sendUpdate: jest.fn(),
    sendManifestUpdate: jest.fn(),
    onUpdate: jest.fn(),
    onError: jest.fn(),
    onDisconnect: jest.fn(),
    onManifestPaths: jest.fn(),
};
const MockCollabClient = jest.fn().mockImplementation(() => mockClientInstance);

jest.unstable_mockModule('../collab-client', () => ({
    CollabClient: MockCollabClient,
}));

jest.unstable_mockModule('../editor-sync', () => ({
    EditorSync: jest.fn().mockImplementation(() => ({
        bindToEditor: jest.fn(),
        unbind: jest.fn(),
        onLocalChange: jest.fn(),
        getText: jest.fn().mockReturnValue(''),
        setErrorCallback: jest.fn(),
    })),
}));

const { default: CollabPlugin } = await import('../main');
type CollabPlugin = InstanceType<typeof CollabPlugin>;

interface VaultHandlers {
    [event: string]: (...args: any[]) => void;
}

function createMockPlugin(): { plugin: CollabPlugin; vaultHandlers: VaultHandlers; app: any } {
    const vaultHandlers: VaultHandlers = {};
    const mockApp = {
        vault: {
            adapter: {
                readBinary: jest
                    .fn<() => Promise<ArrayBuffer>>()
                    .mockResolvedValue(new ArrayBuffer(8)),
            },
            on: jest.fn((event: string, cb: (...args: any[]) => void) => {
                vaultHandlers[event] = cb;
                return { event };
            }),
            offref: jest.fn(),
            getAbstractFileByPath: jest.fn().mockReturnValue(null),
            create: jest
                .fn<(path: string, data: string) => Promise<unknown>>()
                .mockResolvedValue({}),
            createFolder: jest.fn<(path: string) => Promise<unknown>>().mockResolvedValue({}),
        },
        workspace: {
            getActiveViewOfType: jest.fn().mockReturnValue({
                file: { path: 'test.md' },
                editor: {
                    getValue: jest.fn().mockReturnValue(''),
                    setValue: jest.fn(),
                    getCursor: jest.fn().mockReturnValue({ line: 0, ch: 0 }),
                    setCursor: jest.fn(),
                },
            }),
            on: jest.fn().mockReturnValue({ unload: jest.fn() }),
            offref: jest.fn(),
        },
    };
    const mockManifest = {
        dir: '/test/plugin/dir',
        id: 'obsidian-ee',
        name: 'Obsidian E2E',
        version: '0.1.0',
    };
    const plugin = new CollabPlugin(mockApp as any, mockManifest as any);
    return { plugin, vaultHandlers, app: mockApp };
}

async function startedPlugin() {
    const ctx = createMockPlugin();
    await ctx.plugin.onload();
    await ctx.plugin.startSession('owner');
    return ctx;
}

/** The onManifestPaths callback main.ts registered with the client. */
function manifestCallback(): (paths: string[]) => Promise<void> | void {
    const calls = (mockClientInstance.onManifestPaths as jest.Mock).mock.calls;
    expect(calls.length).toBeGreaterThan(0);
    return calls[0][0] as (paths: string[]) => Promise<void> | void;
}

describe('CollabPlugin vault sync wiring', () => {
    let consoleSpy: ReturnType<typeof jest.spyOn>;

    beforeEach(() => {
        consoleSpy = jest.spyOn(console, 'error').mockImplementation(() => {});
        jest.spyOn(console, 'log').mockImplementation(() => {});
        jest.spyOn(console, 'warn').mockImplementation(() => {});
        jest.clearAllMocks();
        mockCompile.mockResolvedValue(mockWasmModule as WebAssembly.Module);
        mockWasmInit.mockResolvedValue(undefined);
        mockClientInstance.connect.mockResolvedValue(undefined);
        MockWasmVaultSync.mockImplementation(() => mockVaultSyncInstance);
        MockCollabClient.mockImplementation(() => mockClientInstance);
        mockVaultSyncInstance.handle_created.mockReturnValue(makeAction('created', 'notes/x.md'));
        mockVaultSyncInstance.handle_deleted.mockReturnValue(makeAction('deleted', 'notes/x.md'));
        mockVaultSyncInstance.handle_renamed.mockReturnValue(
            makeAction('renamed', 'old.md', 'new.md')
        );
    });

    afterEach(() => {
        jest.restoreAllMocks();
    });

    it('startSession constructs a WasmVaultSync and passes it (plus the manifest doc id) to the client', async () => {
        await startedPlugin();
        expect(MockWasmVaultSync).toHaveBeenCalledTimes(1);
        expect(MockCollabClient).toHaveBeenCalledWith(
            expect.objectContaining({
                vaultSync: mockVaultSyncInstance,
                manifestDocId: MANIFEST_ID,
                role: 'owner',
            })
        );
    });

    it('startSession registers vault create/delete/rename handlers', async () => {
        const { app } = await startedPlugin();
        const events = (app.vault.on as jest.Mock).mock.calls.map((c: any[]) => c[0]);
        expect(events).toEqual(expect.arrayContaining(['create', 'delete', 'rename']));
    });

    it('a vault create routes through handle_created and sends exactly one manifest update', async () => {
        const { vaultHandlers } = await startedPlugin();
        vaultHandlers['create']({ path: 'notes/x.md' });
        expect(mockVaultSyncInstance.handle_created).toHaveBeenCalledWith('notes/x.md');
        expect(mockClientInstance.sendManifestUpdate).toHaveBeenCalledTimes(1);
        expect(mockClientInstance.sendManifestUpdate).toHaveBeenCalledWith(
            new Uint8Array([9, 9, 9])
        );
    });

    it('a vault delete routes through handle_deleted and sends a manifest update', async () => {
        const { vaultHandlers } = await startedPlugin();
        vaultHandlers['delete']({ path: 'notes/x.md' });
        expect(mockVaultSyncInstance.handle_deleted).toHaveBeenCalledWith('notes/x.md');
        expect(mockClientInstance.sendManifestUpdate).toHaveBeenCalledTimes(1);
    });

    it('a vault rename routes through handle_renamed(old, new) and sends a manifest update', async () => {
        const { vaultHandlers } = await startedPlugin();
        vaultHandlers['rename']({ path: 'notes/new.md' }, 'notes/old.md');
        expect(mockVaultSyncInstance.handle_renamed).toHaveBeenCalledWith(
            'notes/old.md',
            'notes/new.md'
        );
        expect(mockClientInstance.sendManifestUpdate).toHaveBeenCalledTimes(1);
    });

    it('an IGNORED (out-of-scope) create sends NOTHING', async () => {
        mockVaultSyncInstance.handle_created.mockReturnValue(
            makeAction('ignored', 'private/skip.md')
        );
        const { vaultHandlers } = await startedPlugin();
        vaultHandlers['create']({ path: 'private/skip.md' });
        expect(mockVaultSyncInstance.handle_created).toHaveBeenCalledWith('private/skip.md');
        expect(mockClientInstance.sendManifestUpdate).not.toHaveBeenCalled();
    });

    it('IDEMPOTENT: a second startSession registers no extra handlers and events fire once', async () => {
        const { plugin, vaultHandlers, app } = await startedPlugin();
        const callsAfterFirst = (app.vault.on as jest.Mock).mock.calls.length;
        await plugin.startSession('owner');
        expect((app.vault.on as jest.Mock).mock.calls.length).toBe(callsAfterFirst);
        expect(MockWasmVaultSync).toHaveBeenCalledTimes(1);

        vaultHandlers['create']({ path: 'notes/x.md' });
        expect(mockClientInstance.sendManifestUpdate).toHaveBeenCalledTimes(1);
    });

    it('a handler error is contained: it does not throw out of the vault event callback', async () => {
        mockVaultSyncInstance.handle_created.mockImplementation(() => {
            throw new Error('registry full');
        });
        const { vaultHandlers } = await startedPlugin();
        expect(() => vaultHandlers['create']({ path: 'notes/x.md' })).not.toThrow();
        expect(mockClientInstance.sendManifestUpdate).not.toHaveBeenCalled();
        expect(consoleSpy).toHaveBeenCalled();
    });

    it('stopSession unregisters vault handlers and frees the WasmVaultSync', async () => {
        const { plugin, app } = await startedPlugin();
        plugin.stopSession();
        expect((app.vault.offref as jest.Mock).mock.calls.length).toBe(3);
        expect(mockVaultSyncInstance.free).toHaveBeenCalledTimes(1);

        // A fresh session after stop re-registers cleanly (lifecycle is reusable).
        await plugin.startSession('owner');
        expect(MockWasmVaultSync).toHaveBeenCalledTimes(2);
    });

    it('frees the WasmSyncAction even when sendManifestUpdate throws', async () => {
        const action = makeAction('created', 'notes/x.md');
        mockVaultSyncInstance.handle_created.mockReturnValue(action);
        mockClientInstance.sendManifestUpdate.mockImplementationOnce(() => {
            throw new Error('send failed');
        });
        const { vaultHandlers } = await startedPlugin();
        expect(() => vaultHandlers['create']({ path: 'notes/x.md' })).not.toThrow();
        expect(action.free).toHaveBeenCalledTimes(1);
    });
});

describe('CollabPlugin remote file materialization (#32)', () => {
    let consoleSpy: ReturnType<typeof jest.spyOn>;

    beforeEach(() => {
        consoleSpy = jest.spyOn(console, 'error').mockImplementation(() => {});
        jest.spyOn(console, 'log').mockImplementation(() => {});
        jest.spyOn(console, 'warn').mockImplementation(() => {});
        jest.clearAllMocks();
        mockCompile.mockResolvedValue(mockWasmModule as WebAssembly.Module);
        mockWasmInit.mockResolvedValue(undefined);
        mockClientInstance.connect.mockResolvedValue(undefined);
        MockWasmVaultSync.mockImplementation(() => mockVaultSyncInstance);
        MockCollabClient.mockImplementation(() => mockClientInstance);
        mockVaultSyncInstance.handle_created.mockReturnValue(makeAction('created', 'notes/x.md'));
    });

    afterEach(() => {
        jest.restoreAllMocks();
    });

    it('registers an onManifestPaths callback with the client', async () => {
        await startedPlugin();
        expect(mockClientInstance.onManifestPaths).toHaveBeenCalledWith(expect.any(Function));
    });

    it('creates a missing remote path (and its parent folder) in the vault', async () => {
        const { app } = await startedPlugin();
        await manifestCallback()(['notes/x.md']);
        expect(app.vault.createFolder).toHaveBeenCalledWith('notes');
        expect(app.vault.create).toHaveBeenCalledWith('notes/x.md', '');
    });

    it('does NOT create a file that already exists in the vault', async () => {
        const { app } = await startedPlugin();
        (app.vault.getAbstractFileByPath as jest.Mock).mockReturnValue({ path: 'notes/x.md' });
        await manifestCallback()(['notes/x.md']);
        expect(app.vault.create).not.toHaveBeenCalled();
    });

    it('rejects traversal/absolute/backslash/empty paths: vault untouched, error surfaced', async () => {
        const { app } = await startedPlugin();
        await manifestCallback()(['../evil.md', '/abs.md', 'a\\b.md', 'a/../b.md', '']);
        expect(app.vault.create).not.toHaveBeenCalled();
        expect(app.vault.createFolder).not.toHaveBeenCalled();
        expect(consoleSpy).toHaveBeenCalled();
    });

    it('ECHO GUARD: materializing a remote path must not re-send a manifest update', async () => {
        const { app, vaultHandlers } = await startedPlugin();
        // Obsidian fires the vault 'create' event for programmatic creates too.
        (app.vault.create as jest.Mock).mockImplementation(async (path: unknown) => {
            vaultHandlers['create']({ path });
            return {};
        });
        await manifestCallback()(['notes/remote.md']);
        expect(mockClientInstance.sendManifestUpdate).not.toHaveBeenCalled();
    });

    it('a local create NOT triggered by materialization still sends a manifest update', async () => {
        const { vaultHandlers } = await startedPlugin();
        vaultHandlers['create']({ path: 'notes/local.md' });
        expect(mockClientInstance.sendManifestUpdate).toHaveBeenCalledTimes(1);
    });
});
