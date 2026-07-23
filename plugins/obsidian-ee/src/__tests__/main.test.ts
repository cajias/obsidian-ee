import { Notice } from 'obsidian';

// Mock WebAssembly.compile for WASM loading
const mockWasmModule = {};
const mockCompile = jest.fn().mockResolvedValue(mockWasmModule);
(global as unknown as { WebAssembly: typeof WebAssembly }).WebAssembly = {
    ...WebAssembly,
    compile: mockCompile,
};

jest.mock('obsidian', () => ({
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
            return Promise.resolve({});
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

const mockWasmInit = jest.fn().mockResolvedValue(undefined);
const mockCollabCore = jest.fn().mockImplementation(() => ({
    insert: jest.fn(),
    delete: jest.fn(),
    get_text: jest.fn().mockReturnValue(''),
    encode_state: jest.fn().mockReturnValue(new Uint8Array()),
    encode_state_encrypted: jest.fn().mockReturnValue(new Uint8Array()),
    apply_update: jest.fn(),
    set_encryption_key: jest.fn(),
    free: jest.fn(),
}));

jest.mock('../wasm/collab_wasm', () => ({
    __esModule: true,
    default: mockWasmInit,
    CollabCore: mockCollabCore,
}));

jest.mock('../collab-client', () => ({
    CollabClient: jest.fn().mockImplementation(() => ({
        connect: jest.fn().mockResolvedValue(undefined),
        disconnect: jest.fn(),
        getText: jest.fn().mockReturnValue(''),
        sendUpdate: jest.fn(),
        onUpdate: jest.fn(),
        onError: jest.fn(),
        onDisconnect: jest.fn(),
    })),
}));

jest.mock('../editor-sync', () => ({
    EditorSync: jest.fn().mockImplementation(() => ({
        bindToEditor: jest.fn(),
        unbind: jest.fn(),
        onLocalChange: jest.fn(),
        getText: jest.fn().mockReturnValue(''),
        setErrorCallback: jest.fn(),
    })),
}));

import CollabPlugin from '../main';

// Helper to create a properly mocked plugin instance
function createMockPlugin(): CollabPlugin {
    const mockApp = {
        vault: {
            adapter: {
                readBinary: jest.fn().mockResolvedValue(new ArrayBuffer(8)),
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

describe('CollabPlugin', () => {
    let consoleSpy: jest.SpyInstance;
    let consoleWarnSpy: jest.SpyInstance;

    beforeEach(() => {
        consoleSpy = jest.spyOn(console, 'error').mockImplementation(() => {});
        consoleWarnSpy = jest.spyOn(console, 'warn').mockImplementation(() => {});
        jest.clearAllMocks();
        // Restore mocks after clearAllMocks
        mockCompile.mockResolvedValue(mockWasmModule);
        mockWasmInit.mockResolvedValue(undefined);
        mockCollabCore.mockImplementation(() => ({
            insert: jest.fn(),
            delete: jest.fn(),
            get_text: jest.fn().mockReturnValue(''),
            encode_state: jest.fn().mockReturnValue(new Uint8Array()),
            encode_state_encrypted: jest.fn().mockReturnValue(new Uint8Array()),
            apply_update: jest.fn(),
            set_encryption_key: jest.fn(),
            free: jest.fn(),
        }));
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
                    readBinary: jest.fn().mockResolvedValue(new ArrayBuffer(8)),
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

        // Verify WASM was initialized
        expect(mockWasmInit).toHaveBeenCalled();
        expect(mockCollabCore).toHaveBeenCalled();
        expect((plugin as any).collabCore).not.toBeNull();
        expect((plugin as any).wasmInitialized).toBe(true);
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

        it('should handle errors in collabCore.free gracefully', async () => {
            const plugin = createMockPlugin();
            await plugin.onload();

            // Access private collabCore and mock free to throw
            const collabCore = (plugin as any).collabCore;
            collabCore.free = jest.fn().mockImplementation(() => {
                throw new Error('free error');
            });

            // onunload should not throw
            expect(() => plugin.onunload()).not.toThrow();

            // Error should be logged
            expect(consoleSpy).toHaveBeenCalledWith(
                '[CollabPlugin] Error freeing WASM resources:',
                expect.any(Error)
            );
        });

        it('should continue cleanup even if stopSession fails', async () => {
            const plugin = createMockPlugin();
            await plugin.onload();

            const collabCore = (plugin as any).collabCore;
            const freeSpy = jest.spyOn(collabCore, 'free');

            // Mock stopSession to throw
            plugin.stopSession = jest.fn().mockImplementation(() => {
                throw new Error('stopSession error');
            });

            plugin.onunload();

            // free() should still be called despite stopSession failing
            expect(freeSpy).toHaveBeenCalled();
        });

        it('should set collabCore to null after freeing', async () => {
            const plugin = createMockPlugin();
            await plugin.onload();

            plugin.onunload();

            expect((plugin as any).collabCore).toBeNull();
        });
    });

    describe('startSession', () => {
        it('should warn about insecure placeholder encryption key', async () => {
            const plugin = createMockPlugin();
            // Add workspace mock while keeping vault adapter
            (plugin as any).app.workspace = {
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
            };
            (plugin as any).registerEvent = jest.fn();

            await plugin.onload();
            await plugin.startSession();

            // Should show console warning
            expect(consoleWarnSpy).toHaveBeenCalledWith(
                '[CollabPlugin] SECURITY WARNING: Using placeholder encryption key. ' +
                    'This is insecure and should only be used for development.'
            );

            // Should show Notice to user
            expect(Notice).toHaveBeenCalledWith(
                'Warning: Using insecure placeholder encryption key',
                expect.any(Number)
            );
        });

        it('should register onError and onDisconnect callbacks', async () => {
            const plugin = createMockPlugin();
            // Mock workspace to return an active view
            (plugin as any).app.workspace = {
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
            };
            (plugin as any).registerEvent = jest.fn();

            await plugin.onload();
            await plugin.startSession();

            const collabClient = (plugin as any).collabClient;
            expect(collabClient.onError).toHaveBeenCalledWith(expect.any(Function));
            expect(collabClient.onDisconnect).toHaveBeenCalledWith(expect.any(Function));
        });

        it('should register EditorSync error callback', async () => {
            const plugin = createMockPlugin();
            (plugin as any).app.workspace = {
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
            };
            (plugin as any).registerEvent = jest.fn();

            await plugin.onload();
            await plugin.startSession();

            const editorSync = (plugin as any).editorSync;
            expect(editorSync.setErrorCallback).toHaveBeenCalledWith(expect.any(Function));
        });

        it('should not start a second session while one is already active', async () => {
            const plugin = createMockPlugin();
            const onMock = jest.fn().mockReturnValue({ unload: jest.fn() });
            (plugin as any).app.workspace = {
                getActiveViewOfType: jest.fn().mockReturnValue({
                    file: { path: 'test.md' },
                    editor: {
                        getValue: jest.fn().mockReturnValue(''),
                        setValue: jest.fn(),
                        getCursor: jest.fn().mockReturnValue({ line: 0, ch: 0 }),
                        setCursor: jest.fn(),
                    },
                }),
                on: onMock,
                offref: jest.fn(),
            };
            (plugin as any).registerEvent = jest.fn();

            await plugin.onload();
            await plugin.startSession();

            const firstClient = (plugin as any).collabClient;
            const firstSync = (plugin as any).editorSync;
            const firstHandler = (plugin as any).editorChangeHandler;

            // F15: second start must be a no-op that warns, not orphan the first session.
            await plugin.startSession();

            expect(Notice).toHaveBeenCalledWith('Collaboration session already active');
            // First session's objects and handler are untouched.
            expect((plugin as any).collabClient).toBe(firstClient);
            expect((plugin as any).editorSync).toBe(firstSync);
            expect((plugin as any).editorChangeHandler).toBe(firstHandler);
        });

        it('should store editor change handler reference', async () => {
            const plugin = createMockPlugin();
            const mockHandler = { unload: jest.fn() };
            (plugin as any).app.workspace = {
                getActiveViewOfType: jest.fn().mockReturnValue({
                    file: { path: 'test.md' },
                    editor: {
                        getValue: jest.fn().mockReturnValue(''),
                        setValue: jest.fn(),
                        getCursor: jest.fn().mockReturnValue({ line: 0, ch: 0 }),
                        setCursor: jest.fn(),
                    },
                }),
                on: jest.fn().mockReturnValue(mockHandler),
                offref: jest.fn(),
            };
            (plugin as any).registerEvent = jest.fn();

            await plugin.onload();
            await plugin.startSession();

            expect((plugin as any).editorChangeHandler).toBe(mockHandler);
        });
    });

    describe('stopSession', () => {
        it('should unregister editor change handler', async () => {
            const plugin = createMockPlugin();
            const mockHandler = { unload: jest.fn() };
            const offrefMock = jest.fn();
            (plugin as any).app.workspace = {
                getActiveViewOfType: jest.fn().mockReturnValue({
                    file: { path: 'test.md' },
                    editor: {
                        getValue: jest.fn().mockReturnValue(''),
                        setValue: jest.fn(),
                        getCursor: jest.fn().mockReturnValue({ line: 0, ch: 0 }),
                        setCursor: jest.fn(),
                    },
                }),
                on: jest.fn().mockReturnValue(mockHandler),
                offref: offrefMock,
            };
            (plugin as any).registerEvent = jest.fn();

            await plugin.onload();
            await plugin.startSession();
            plugin.stopSession();

            expect(offrefMock).toHaveBeenCalledWith(mockHandler);
            expect((plugin as any).editorChangeHandler).toBeNull();
        });

        it('should free and null CollabCore to prevent memory leak', async () => {
            const plugin = createMockPlugin();
            const offrefMock = jest.fn();
            (plugin as any).app.workspace = {
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
                offref: offrefMock,
            };
            (plugin as any).registerEvent = jest.fn();

            await plugin.onload();
            const originalCollabCore = (plugin as any).collabCore;
            const freeSpy = jest.spyOn(originalCollabCore, 'free');

            await plugin.startSession();
            plugin.stopSession();

            expect(freeSpy).toHaveBeenCalled();
            // F16: CollabCore is nulled on stop; startSession re-creates it lazily.
            expect((plugin as any).collabCore).toBeNull();
        });

        it('should re-initialize CollabCore lazily on startSession after stop', async () => {
            const plugin = createMockPlugin();
            (plugin as any).app.workspace = {
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
            };
            (plugin as any).registerEvent = jest.fn();

            await plugin.onload();
            await plugin.startSession();
            plugin.stopSession();

            // Core was freed and nulled by stopSession.
            expect((plugin as any).collabCore).toBeNull();

            // A fresh startSession must lazily re-create the core and succeed.
            await plugin.startSession();
            expect((plugin as any).collabCore).not.toBeNull();
            expect((plugin as any).collabClient).not.toBeNull();
        });

        it('should handle errors when freeing CollabCore during stopSession', async () => {
            const plugin = createMockPlugin();
            const offrefMock = jest.fn();
            (plugin as any).app.workspace = {
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
                offref: offrefMock,
            };
            (plugin as any).registerEvent = jest.fn();

            await plugin.onload();
            await plugin.startSession();

            // Mock free to throw
            const collabCore = (plugin as any).collabCore;
            collabCore.free = jest.fn().mockImplementation(() => {
                throw new Error('free error');
            });

            // stopSession should not throw
            expect(() => plugin.stopSession()).not.toThrow();

            // Error should be logged
            expect(consoleSpy).toHaveBeenCalledWith(
                '[CollabPlugin] Error freeing WASM resources:',
                expect.any(Error)
            );

            // F16: CollabCore is nulled even when free() throws.
            expect((plugin as any).collabCore).toBeNull();
        });

        it('should call stopSession when disconnect callback is invoked', async () => {
            const plugin = createMockPlugin();
            let disconnectCallback: ((reason: string) => void) | null = null;
            const offrefMock = jest.fn();
            (plugin as any).app.workspace = {
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
                offref: offrefMock,
            };
            (plugin as any).registerEvent = jest.fn();

            // Capture the disconnect callback
            const { CollabClient } = require('../collab-client');
            CollabClient.mockImplementation(() => ({
                connect: jest.fn().mockResolvedValue(undefined),
                disconnect: jest.fn(),
                getText: jest.fn().mockReturnValue(''),
                sendUpdate: jest.fn(),
                onUpdate: jest.fn(),
                onError: jest.fn(),
                onDisconnect: jest.fn().mockImplementation((cb: (reason: string) => void) => {
                    disconnectCallback = cb;
                }),
            }));

            await plugin.onload();
            await plugin.startSession();

            const stopSessionSpy = jest.spyOn(plugin, 'stopSession');

            // Simulate disconnect
            expect(disconnectCallback).not.toBeNull();
            disconnectCallback!('max_retries_exceeded');

            expect(stopSessionSpy).toHaveBeenCalled();
            expect(Notice).toHaveBeenCalledWith('Collaboration disconnected: max_retries_exceeded');
        });
    });
});
