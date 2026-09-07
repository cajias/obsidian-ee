import { jest, describe, it, expect, beforeEach, afterEach } from '@jest/globals';
import { mockObsidianModule } from './helpers/mock-obsidian';
import {
    createCollabWasmMock,
    createMockClientInstance,
    mockCollabClientModule,
    mockEditorSyncModule,
} from './helpers/mock-collab-modules';

// Mock WebAssembly.compile for WASM loading
const mockWasmModule = {};
const mockCompile = jest
    .fn<(bytes: BufferSource) => Promise<WebAssembly.Module>>()
    .mockResolvedValue(mockWasmModule as WebAssembly.Module);
(global as unknown as { WebAssembly: typeof WebAssembly }).WebAssembly = {
    ...WebAssembly,
    compile: mockCompile,
};

jest.unstable_mockModule('obsidian', mockObsidianModule);

// MLS-only: main.ts needs the WASM module initialized (`init`) plus the vault
// sync surface (#32). The MLS doc classes are driven inside CollabClient, so no
// CollabCore export is mocked here.
const { wasmInit: mockWasmInit, moduleFactory: collabWasmModuleFactory } = createCollabWasmMock();
jest.unstable_mockModule('../wasm/collab_wasm', collabWasmModuleFactory);

jest.unstable_mockModule('../collab-client', mockCollabClientModule());

jest.unstable_mockModule('../editor-sync', mockEditorSyncModule());

const { Notice } = await import('obsidian');
const { default: CollabPlugin } = await import('../main');
type CollabPlugin = InstanceType<typeof CollabPlugin>;

/**
 * An in-memory DataAdapter. #93 stores the encrypted MLS snapshots and their
 * at-rest key through this, so tests can assert what actually reached disk —
 * above all that the key never lands in the same file as the blobs.
 */
function makeAdapter() {
    const files = new Map<string, ArrayBuffer>();
    return {
        files,
        exists: jest.fn((path: string) => Promise.resolve(files.has(path))),
        // Unknown paths fall back to a stub buffer: this same adapter serves
        // the wasm binary that `init` loads, which no test writes.
        readBinary: jest.fn((path: string) =>
            Promise.resolve(files.get(path) ?? new ArrayBuffer(8))
        ),
        writeBinary: jest.fn((path: string, data: ArrayBuffer) => {
            files.set(path, data);
            return Promise.resolve();
        }),
    };
}

// Helper to create a properly mocked plugin instance
function createMockPlugin(): CollabPlugin {
    const mockApp = {
        vault: {
            adapter: makeAdapter(),
            // Vault sync (#32) registers create/delete/rename handlers and
            // materializes remote paths.
            on: jest.fn().mockReturnValue({}),
            offref: jest.fn(),
            getAbstractFileByPath: jest.fn().mockReturnValue(null),
            create: jest.fn<() => Promise<unknown>>().mockResolvedValue({}),
            createFolder: jest.fn<() => Promise<unknown>>().mockResolvedValue({}),
        },
        workspace: {
            getActiveViewOfType: jest.fn(),
            on: jest.fn(),
            offref: jest.fn(),
        },
    };
    const mockManifest = {
        dir: '/test/plugin/dir',
        id: 'obsidian-ee',
        name: 'Obsidian E2E',
        version: '0.1.0',
    };
    return new CollabPlugin(mockApp as any, mockManifest as any);
}

// A workspace mock with an active markdown view + editor.
function mockWorkspaceWithView(overrides: Record<string, unknown> = {}) {
    return {
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
        ...overrides,
    };
}

// A plugin instance ready for startSession()/stopSession(): an active
// markdown view is open and registerEvent is stubbed, exactly what every
// startSession/stopSession test needs before it can drive the plugin.
function createReadyPlugin(workspaceOverrides: Record<string, unknown> = {}): CollabPlugin {
    const plugin = createMockPlugin();
    (plugin as any).app.workspace = mockWorkspaceWithView(workspaceOverrides);
    (plugin as any).registerEvent = jest.fn();
    return plugin;
}

describe('CollabPlugin', () => {
    let consoleSpy: ReturnType<typeof jest.spyOn>;
    let consoleWarnSpy: ReturnType<typeof jest.spyOn>;

    beforeEach(() => {
        consoleSpy = jest.spyOn(console, 'error').mockImplementation(() => {});
        consoleWarnSpy = jest.spyOn(console, 'warn').mockImplementation(() => {});
        jest.clearAllMocks();
        // Restore mocks after clearAllMocks
        mockCompile.mockResolvedValue(mockWasmModule as WebAssembly.Module);
        mockWasmInit.mockResolvedValue(undefined);
    });

    afterEach(() => {
        consoleSpy.mockRestore();
        consoleWarnSpy.mockRestore();
    });

    it('should instantiate without error', () => {
        const plugin = createMockPlugin();
        expect(plugin).toBeDefined();
    });

    it('should throw error when plugin directory is undefined', async () => {
        const mockApp = {
            vault: {
                adapter: {
                    readBinary: jest
                        .fn<() => Promise<ArrayBuffer>>()
                        .mockResolvedValue(new ArrayBuffer(8)),
                },
            },
            workspace: {
                getActiveViewOfType: jest.fn(),
                on: jest.fn(),
                offref: jest.fn(),
            },
        };
        // Create manifest with undefined dir to trigger the error path
        const mockManifest = {
            dir: undefined, // This triggers 'Plugin directory not found' error
            id: 'obsidian-ee',
            name: 'Obsidian E2E',
            version: '0.1.0',
        };
        const plugin = new CollabPlugin(mockApp as any, mockManifest as any);

        await plugin.onload();

        // Should show error notice and log error
        expect(consoleSpy).toHaveBeenCalledWith('Failed to initialize WASM:', expect.any(Error));
        expect(Notice).toHaveBeenCalledWith('Failed to load collaboration plugin');
        // WASM should not be initialized
        expect((plugin as any).wasmInitialized).toBe(false);
    });

    it('should initialize WASM on load', async () => {
        const plugin = createMockPlugin();
        await plugin.onload();

        // Verify WASM was initialized. The client owns the MLS document; main.ts
        // no longer holds a plaintext core.
        expect(mockWasmInit).toHaveBeenCalled();
        expect((plugin as any).wasmInitialized).toBe(true);
    });

    describe('loadSettings', () => {
        it('drops the legacy plaintext encryptionKey so it is never persisted back', async () => {
            const plugin = createMockPlugin();
            // A data.json written by the old AES-PSK plugin still carries the key.
            (plugin as any).loadData = jest.fn<() => Promise<unknown>>().mockResolvedValue({
                relayUrl: 'ws://localhost:8080',
                encryptionKey: '00'.repeat(32),
            });
            const saveData = jest
                .fn<(data: unknown) => Promise<void>>()
                .mockResolvedValue(undefined);
            (plugin as any).saveData = saveData;

            // loadSettings ALONE must purge the key from disk — nothing else is
            // guaranteed to call saveSettings before the user edits a setting.
            await plugin.loadSettings();

            expect(
                (plugin.settings as unknown as Record<string, unknown>).encryptionKey
            ).toBeUndefined();
            expect(saveData).toHaveBeenCalledTimes(1);
            expect(saveData.mock.calls[0][0]).not.toHaveProperty('encryptionKey');
            expect(saveData.mock.calls[0][0]).toHaveProperty('relayUrl', 'ws://localhost:8080');
        });

        it('does not rewrite data.json when it carries only known fields', async () => {
            const plugin = createMockPlugin();
            (plugin as any).loadData = jest.fn<() => Promise<unknown>>().mockResolvedValue({
                relayUrl: 'ws://localhost:8080',
            });
            const saveData = jest
                .fn<(data: unknown) => Promise<void>>()
                .mockResolvedValue(undefined);
            (plugin as any).saveData = saveData;

            await plugin.loadSettings();

            expect(saveData).not.toHaveBeenCalled();
        });
    });

    describe('onunload', () => {
        it('should handle errors in stopSession gracefully', async () => {
            const plugin = createMockPlugin();
            await plugin.onload();

            // Mock stopSession to throw
            plugin.stopSession = jest.fn().mockImplementation(() => {
                throw new Error('stopSession error');
            });

            // onunload should not throw
            expect(() => plugin.onunload()).not.toThrow();

            // Error should be logged
            expect(consoleSpy).toHaveBeenCalledWith(
                '[CollabPlugin] Error stopping session during unload:',
                expect.any(Error)
            );
        });
    });

    describe('startSession', () => {
        it('should start a session as owner (creating the MLS group)', async () => {
            const plugin = createReadyPlugin();

            const { CollabClient } = await import('../collab-client');

            await plugin.onload();
            await plugin.startSession('owner');

            // MLS-only: config carries a role, never an encryptionKey.
            expect(CollabClient).toHaveBeenCalled();
            const passedConfig = (CollabClient as jest.Mock).mock.calls[0][0] as { role: string };
            expect(passedConfig.role).toBe('owner');
            expect((plugin as any).collabClient).not.toBeNull();
        });

        it('should start a session as joiner', async () => {
            const plugin = createReadyPlugin();

            const { CollabClient } = await import('../collab-client');

            await plugin.onload();
            await plugin.startSession('joiner');

            const passedConfig = (CollabClient as jest.Mock).mock.calls[0][0] as { role: string };
            expect(passedConfig.role).toBe('joiner');
            expect((plugin as any).collabClient).not.toBeNull();
        });

        it('should not start a session when no markdown file is open', async () => {
            const plugin = createMockPlugin();
            (plugin as any).app.workspace = {
                getActiveViewOfType: jest.fn().mockReturnValue(null),
                on: jest.fn(),
                offref: jest.fn(),
            };
            (plugin as any).registerEvent = jest.fn();

            const { CollabClient } = await import('../collab-client');

            await plugin.onload();
            await plugin.startSession('owner');

            expect(Notice).toHaveBeenCalledWith('Please open a markdown file first');
            expect(CollabClient).not.toHaveBeenCalled();
            expect((plugin as any).collabClient).toBeNull();
        });

        it('should register onError and onDisconnect callbacks', async () => {
            const plugin = createReadyPlugin();

            await plugin.onload();
            await plugin.startSession('owner');

            const collabClient = (plugin as any).collabClient;
            expect(collabClient.onError).toHaveBeenCalledWith(expect.any(Function));
            expect(collabClient.onDisconnect).toHaveBeenCalledWith(expect.any(Function));
        });

        it('should register EditorSync error callback', async () => {
            const plugin = createReadyPlugin();

            await plugin.onload();
            await plugin.startSession('owner');

            const editorSync = (plugin as any).editorSync;
            expect(editorSync.setErrorCallback).toHaveBeenCalledWith(expect.any(Function));
        });

        it('should not start a second session while one is already active', async () => {
            const onMock = jest.fn().mockReturnValue({ unload: jest.fn() });
            const plugin = createReadyPlugin({ on: onMock });

            await plugin.onload();
            await plugin.startSession('owner');

            const firstClient = (plugin as any).collabClient;
            const firstSync = (plugin as any).editorSync;
            const firstHandler = (plugin as any).editorChangeHandler;

            // F15: second start must be a no-op that warns, not orphan the first session.
            await plugin.startSession('owner');

            expect(Notice).toHaveBeenCalledWith('Collaboration session already active');
            // First session's objects and handler are untouched.
            expect((plugin as any).collabClient).toBe(firstClient);
            expect((plugin as any).editorSync).toBe(firstSync);
            expect((plugin as any).editorChangeHandler).toBe(firstHandler);
        });

        it('should store editor change handler reference', async () => {
            const mockHandler = { unload: jest.fn() };
            const plugin = createReadyPlugin({ on: jest.fn().mockReturnValue(mockHandler) });

            await plugin.onload();
            await plugin.startSession('owner');

            expect((plugin as any).editorChangeHandler).toBe(mockHandler);
        });
    });

    describe('stopSession', () => {
        it('should unregister editor change handler', async () => {
            const mockHandler = { unload: jest.fn() };
            const offrefMock = jest.fn();
            const plugin = createReadyPlugin({
                on: jest.fn().mockReturnValue(mockHandler),
                offref: offrefMock,
            });

            await plugin.onload();
            await plugin.startSession('owner');
            plugin.stopSession();

            expect(offrefMock).toHaveBeenCalledWith(mockHandler);
            expect((plugin as any).editorChangeHandler).toBeNull();
        });

        it('should destroy and null the CollabClient (which frees the MLS document)', async () => {
            const plugin = createReadyPlugin();

            await plugin.onload();
            await plugin.startSession('owner');

            const client = (plugin as any).collabClient;
            const destroySpy = jest.spyOn(client, 'destroy');

            plugin.stopSession();

            // CollabClient owns and frees the MLS document in destroy();
            // disconnect() deliberately keeps an established group alive so a
            // later connect() resumes it.
            expect(destroySpy).toHaveBeenCalled();
            expect((plugin as any).collabClient).toBeNull();
            expect((plugin as any).editorSync).toBeNull();
        });

        it('should allow a fresh startSession after stop', async () => {
            const plugin = createReadyPlugin();

            await plugin.onload();
            await plugin.startSession('owner');
            plugin.stopSession();

            expect((plugin as any).collabClient).toBeNull();

            // A fresh startSession must construct a new client and succeed.
            await plugin.startSession('joiner');
            expect((plugin as any).collabClient).not.toBeNull();
        });

        it('should call stopSession when disconnect callback is invoked', async () => {
            const plugin = createReadyPlugin();
            let disconnectCallback: ((reason: string) => void) | null = null;

            // Capture the disconnect callback. Start from the full shared mock
            // surface (every method startSession() calls, incl. #32's
            // onManifestPaths/sendManifestUpdate) and override only onDisconnect —
            // a hand-rolled subset here would silently exercise startSession()'s
            // catch-and-stopSession error path instead of the real connected flow.
            const { CollabClient } = await import('../collab-client');
            (CollabClient as jest.Mock).mockImplementation(() => ({
                ...createMockClientInstance(),
                onDisconnect: jest
                    .fn<(cb: (reason: string) => void) => void>()
                    .mockImplementation((cb) => {
                        disconnectCallback = cb;
                    }),
            }));

            await plugin.onload();
            await plugin.startSession('owner');

            const stopSessionSpy = jest.spyOn(plugin, 'stopSession');

            // Simulate disconnect
            expect(disconnectCallback).not.toBeNull();
            disconnectCallback!('max_retries_exceeded');

            expect(stopSessionSpy).toHaveBeenCalled();
            expect(Notice).toHaveBeenCalledWith('Collaboration disconnected: max_retries_exceeded');
        });
    });
});

describe('persisted MLS state across stopSession/startSession (#93)', () => {
    // The CollabClient mock is module-scoped, so its call log accumulates
    // across every test in this file. Clearing here is what makes `calls[0]`
    // and `calls[1]` mean THIS test's two sessions.
    beforeEach(() => {
        jest.clearAllMocks();
        mockCompile.mockResolvedValue(mockWasmModule as WebAssembly.Module);
        mockWasmInit.mockResolvedValue(undefined);
    });

    /** The shared CollabClient constructor mock and the instance it returns. */
    async function clientMock() {
        const { CollabClient } = await import('../collab-client');
        const ctor = CollabClient as unknown as jest.Mock;
        return {
            ctor,
            instance: ctor.mock.results[0]?.value as ReturnType<typeof createMockClientInstance>,
        };
    }

    function adapterFiles(plugin: CollabPlugin): Map<string, ArrayBuffer> {
        return (plugin.app.vault.adapter as unknown as { files: Map<string, ArrayBuffer> }).files;
    }

    /**
     * `stopSession` is synchronous but its at-rest write is not, so the write
     * has to be allowed to land before the next session reads it. A macrotask
     * turn drains the whole promise chain, which chained microtasks do not.
     */
    async function settle(): Promise<void> {
        await new Promise((resolve) => setTimeout(resolve, 0));
    }

    // THE path-3 test. GIVEN a session started and stopped on a file, WHEN a
    // second session starts, THEN the new CollabClient is constructed with the
    // snapshot the first one handed back — so it resumes that group instead of
    // building a fresh epoch-0 one whose TOFU registration the relay refuses.
    it('hands the saved snapshot to the next session', async () => {
        const plugin = createMockPlugin();
        plugin.app.workspace = mockWorkspaceWithView() as never;
        await plugin.loadSettings();

        await plugin.startSession('owner');
        plugin.stopSession();
        await settle();
        await plugin.startSession('owner');

        const { ctor, instance } = await clientMock();
        expect(ctor.mock.calls.length).toBeGreaterThanOrEqual(2);
        const secondConfig = ctor.mock.calls[1][0] as { snapshots?: Record<string, Uint8Array> };
        expect(secondConfig.snapshots).toEqual(instance.snapshot.mock.results[0]?.value);
        expect(Object.keys(secondConfig.snapshots ?? {})).not.toHaveLength(0);
    });

    // The snapshot must be taken BEFORE destroy() frees the wasm handles;
    // afterwards it returns nothing and the persistence is silently useless.
    it('snapshots before destroying the client', async () => {
        const plugin = createMockPlugin();
        plugin.app.workspace = mockWorkspaceWithView() as never;
        await plugin.loadSettings();

        await plugin.startSession('owner');
        plugin.stopSession();

        const { instance } = await clientMock();
        expect(instance.snapshot).toHaveBeenCalled();
        expect(instance.snapshot.mock.invocationCallOrder[0]).toBeLessThan(
            instance.destroy.mock.invocationCallOrder[0]
        );
    });

    // NEGATIVE, and the one that matters most: this data sits in the vault,
    // often inside a synced folder. The at-rest key must never land in the same
    // file as the blobs it protects.
    it('never writes the at-rest key into the snapshot file', async () => {
        const plugin = createMockPlugin();
        plugin.app.workspace = mockWorkspaceWithView() as never;
        await plugin.loadSettings();

        await plugin.startSession('owner');
        plugin.stopSession();
        await settle();

        const files = adapterFiles(plugin);
        const keyBuf = files.get('/test/plugin/dir/mls-key.bin');
        const stateBuf = files.get('/test/plugin/dir/mls-state.json');
        expect(keyBuf).toBeDefined();
        expect(stateBuf).toBeDefined();

        const key = new Uint8Array(keyBuf!);
        expect(key).toHaveLength(32);

        // Scan the state file's BYTES for the key, and its re-serialized JSON for
        // the key as numbers. A substring search for `join(',')` alone is
        // vacuous: the file is pretty-printed, so a leaked key renders one
        // element per line and never contains the compact form — the assertion
        // stayed green with the key deliberately written into the body.
        const state = new Uint8Array(stateBuf!);
        const rawLeak = Array.from({ length: Math.max(0, state.length - key.length + 1) }).some(
            (_, i) => key.every((b, j) => state[i + j] === b)
        );
        expect(rawLeak).toBe(false);

        const compact = JSON.stringify(JSON.parse(new TextDecoder().decode(state)));
        expect(compact).not.toContain([...key].join(','));
    });

    // A first-ever session has nothing saved and must bootstrap fresh rather
    // than fail: a missing state file is the normal case, not an error.
    it('starts fresh when no snapshot has been saved', async () => {
        const plugin = createMockPlugin();
        plugin.app.workspace = mockWorkspaceWithView() as never;
        await plugin.loadSettings();

        await plugin.startSession('owner');

        const { ctor } = await clientMock();
        const config = ctor.mock.calls[0][0] as { snapshots?: Record<string, Uint8Array> };
        expect(config.snapshots).toEqual({});
    });

    // NEGATIVE — ending a session must be TOTAL. A throwing snapshot used to
    // skip destroy(), leaving the client non-null so startSession's
    // "already active" guard refused every restart. onDisconnect calls
    // stopSession, so this did not need the user to touch the stop command.
    it('destroys the client and permits a restart when snapshot() throws', async () => {
        const plugin = createMockPlugin();
        plugin.app.workspace = mockWorkspaceWithView() as never;
        await plugin.loadSettings();
        await plugin.startSession('owner');

        const { instance, ctor } = await clientMock();
        instance.snapshot.mockImplementation(() => {
            throw new Error('refusing all-zeros key');
        });

        expect(() => plugin.stopSession()).not.toThrow();
        expect(instance.destroy).toHaveBeenCalled();

        await settle();
        await plugin.startSession('owner');
        expect(ctor.mock.calls.length).toBeGreaterThanOrEqual(2);
    });

    // NEGATIVE — persistence failing must cost content resumption, never the
    // session. An unreadable key file used to throw straight out of
    // startSession, so a corrupt at-rest file meant no collaboration at all.
    it('still starts a session when the at-rest state cannot be read', async () => {
        const plugin = createMockPlugin();
        plugin.app.workspace = mockWorkspaceWithView() as never;
        await plugin.loadSettings();
        const adapter = plugin.app.vault.adapter as unknown as {
            exists: jest.Mock;
        };
        adapter.exists.mockImplementation(() => Promise.reject(new Error('disk on fire')));

        await plugin.startSession('owner');

        const { ctor } = await clientMock();
        expect(ctor).toHaveBeenCalled();
        const config = ctor.mock.calls[0][0] as {
            snapshots?: Record<string, Uint8Array>;
            snapshotKey?: Uint8Array;
        };
        expect(config.snapshots).toEqual({});
        expect(config.snapshotKey).toBeUndefined();
    });

    // The persisted user id is reused, so a restored group's MLS leaf identity
    // and the wire identity agree instead of drifting apart on every restart.
    it('reuses the persisted user id on the next session', async () => {
        const plugin = createMockPlugin();
        plugin.app.workspace = mockWorkspaceWithView() as never;
        await plugin.loadSettings();

        await plugin.startSession('owner');
        plugin.stopSession();
        await settle();
        await plugin.startSession('owner');

        const { ctor } = await clientMock();
        const first = ctor.mock.calls[0][0] as { userId: string };
        const second = ctor.mock.calls[1][0] as { userId: string };
        expect(second.userId).toBe(first.userId);
    });
});
