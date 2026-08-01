import { jest, describe, it, expect, beforeEach, afterEach } from '@jest/globals';

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
            // MLS-only: no key input. Relay URL is the only persisted setting.
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
            this.containerEl = {
                empty: jest.fn(),
                createEl: jest.fn(),
            };
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

// MLS-only: main.ts only needs the WASM module initialized (`init`); the MLS
// classes are driven inside CollabClient, so no CollabCore export is mocked here.
const mockWasmInit = jest.fn<() => Promise<void>>().mockResolvedValue(undefined);

jest.unstable_mockModule('../wasm/collab_wasm', () => ({
    __esModule: true,
    default: mockWasmInit,
}));

jest.unstable_mockModule('../collab-client', () => ({
    CollabClient: jest.fn().mockImplementation(() => ({
        connect: jest.fn<() => Promise<void>>().mockResolvedValue(undefined),
        disconnect: jest.fn(),
        getText: jest.fn().mockReturnValue(''),
        sendUpdate: jest.fn(),
        onUpdate: jest.fn(),
        onError: jest.fn(),
        onDisconnect: jest.fn(),
    })),
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

const { Notice } = await import('obsidian');
const { default: CollabPlugin } = await import('../main');
type CollabPlugin = InstanceType<typeof CollabPlugin>;

// Helper to create a properly mocked plugin instance
function createMockPlugin(): CollabPlugin {
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

            await plugin.loadSettings();
            await plugin.saveSettings();

            expect(
                (plugin.settings as unknown as Record<string, unknown>).encryptionKey
            ).toBeUndefined();
            expect(saveData).toHaveBeenCalledTimes(1);
            expect(saveData.mock.calls[0][0]).not.toHaveProperty('encryptionKey');
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
            const plugin = createMockPlugin();
            (plugin as any).app.workspace = mockWorkspaceWithView();
            (plugin as any).registerEvent = jest.fn();

            const { CollabClient } = await import('../collab-client');

            await plugin.onload();
            await plugin.startSession('owner');

            // MLS-only: config carries a role, never an encryptionKey.
            expect(CollabClient).toHaveBeenCalled();
            const passedConfig = (CollabClient as jest.Mock).mock.calls[0][0] as {
                role: string;
                encryptionKey?: unknown;
            };
            expect(passedConfig.role).toBe('owner');
            expect(passedConfig.encryptionKey).toBeUndefined();
            expect((plugin as any).collabClient).not.toBeNull();
        });

        it('should start a session as joiner', async () => {
            const plugin = createMockPlugin();
            (plugin as any).app.workspace = mockWorkspaceWithView();
            (plugin as any).registerEvent = jest.fn();

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
            const plugin = createMockPlugin();
            (plugin as any).app.workspace = mockWorkspaceWithView();
            (plugin as any).registerEvent = jest.fn();

            await plugin.onload();
            await plugin.startSession('owner');

            const collabClient = (plugin as any).collabClient;
            expect(collabClient.onError).toHaveBeenCalledWith(expect.any(Function));
            expect(collabClient.onDisconnect).toHaveBeenCalledWith(expect.any(Function));
        });

        it('should register EditorSync error callback', async () => {
            const plugin = createMockPlugin();
            (plugin as any).app.workspace = mockWorkspaceWithView();
            (plugin as any).registerEvent = jest.fn();

            await plugin.onload();
            await plugin.startSession('owner');

            const editorSync = (plugin as any).editorSync;
            expect(editorSync.setErrorCallback).toHaveBeenCalledWith(expect.any(Function));
        });

        it('should not start a second session while one is already active', async () => {
            const plugin = createMockPlugin();
            const onMock = jest.fn().mockReturnValue({ unload: jest.fn() });
            (plugin as any).app.workspace = mockWorkspaceWithView({ on: onMock });
            (plugin as any).registerEvent = jest.fn();

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
            const plugin = createMockPlugin();
            const mockHandler = { unload: jest.fn() };
            (plugin as any).app.workspace = mockWorkspaceWithView({
                on: jest.fn().mockReturnValue(mockHandler),
            });
            (plugin as any).registerEvent = jest.fn();

            await plugin.onload();
            await plugin.startSession('owner');

            expect((plugin as any).editorChangeHandler).toBe(mockHandler);
        });
    });

    describe('stopSession', () => {
        it('should unregister editor change handler', async () => {
            const plugin = createMockPlugin();
            const mockHandler = { unload: jest.fn() };
            const offrefMock = jest.fn();
            (plugin as any).app.workspace = mockWorkspaceWithView({
                on: jest.fn().mockReturnValue(mockHandler),
                offref: offrefMock,
            });
            (plugin as any).registerEvent = jest.fn();

            await plugin.onload();
            await plugin.startSession('owner');
            plugin.stopSession();

            expect(offrefMock).toHaveBeenCalledWith(mockHandler);
            expect((plugin as any).editorChangeHandler).toBeNull();
        });

        it('should disconnect and null the CollabClient (which frees the MLS document)', async () => {
            const plugin = createMockPlugin();
            (plugin as any).app.workspace = mockWorkspaceWithView();
            (plugin as any).registerEvent = jest.fn();

            await plugin.onload();
            await plugin.startSession('owner');

            const client = (plugin as any).collabClient;
            const disconnectSpy = jest.spyOn(client, 'disconnect');

            plugin.stopSession();

            // CollabClient owns and frees the MLS document in disconnect().
            expect(disconnectSpy).toHaveBeenCalled();
            expect((plugin as any).collabClient).toBeNull();
            expect((plugin as any).editorSync).toBeNull();
        });

        it('should allow a fresh startSession after stop', async () => {
            const plugin = createMockPlugin();
            (plugin as any).app.workspace = mockWorkspaceWithView();
            (plugin as any).registerEvent = jest.fn();

            await plugin.onload();
            await plugin.startSession('owner');
            plugin.stopSession();

            expect((plugin as any).collabClient).toBeNull();

            // A fresh startSession must construct a new client and succeed.
            await plugin.startSession('joiner');
            expect((plugin as any).collabClient).not.toBeNull();
        });

        it('should call stopSession when disconnect callback is invoked', async () => {
            const plugin = createMockPlugin();
            let disconnectCallback: ((reason: string) => void) | null = null;
            (plugin as any).app.workspace = mockWorkspaceWithView();
            (plugin as any).registerEvent = jest.fn();

            // Capture the disconnect callback
            const { CollabClient } = await import('../collab-client');
            (CollabClient as jest.Mock).mockImplementation(() => ({
                connect: jest.fn<() => Promise<void>>().mockResolvedValue(undefined),
                disconnect: jest.fn(),
                getText: jest.fn().mockReturnValue(''),
                sendUpdate: jest.fn(),
                onUpdate: jest.fn(),
                onError: jest.fn(),
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
