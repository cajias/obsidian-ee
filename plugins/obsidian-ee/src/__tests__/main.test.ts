import { Notice } from 'obsidian';

jest.mock('obsidian', () => ({
    Plugin: class {},
    Notice: jest.fn(),
    MarkdownView: class {},
}));

jest.mock('../wasm/collab_wasm', () => ({
    __esModule: true,
    default: jest.fn().mockResolvedValue(undefined),
    CollabCore: jest.fn().mockImplementation(() => ({
        insert: jest.fn(),
        delete: jest.fn(),
        get_text: jest.fn().mockReturnValue(''),
        encode_state: jest.fn().mockReturnValue(new Uint8Array()),
        apply_update: jest.fn(),
        free: jest.fn(),
    })),
}));

jest.mock('../collab-client', () => ({
    CollabClient: jest.fn().mockImplementation(() => ({
        connect: jest.fn().mockResolvedValue(undefined),
        disconnect: jest.fn(),
        getText: jest.fn().mockReturnValue(''),
        sendUpdate: jest.fn(),
        onUpdate: jest.fn(),
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

describe('CollabPlugin', () => {
    let consoleSpy: jest.SpyInstance;
    let consoleWarnSpy: jest.SpyInstance;

    beforeEach(() => {
        consoleSpy = jest.spyOn(console, 'error').mockImplementation(() => {});
        consoleWarnSpy = jest.spyOn(console, 'warn').mockImplementation(() => {});
        jest.clearAllMocks();
    });

    afterEach(() => {
        consoleSpy.mockRestore();
        consoleWarnSpy.mockRestore();
    });

    it('should instantiate without error', () => {
        const plugin = new CollabPlugin({} as any, {} as any);
        expect(plugin).toBeDefined();
    });

    it('should initialize WASM on load', async () => {
        const plugin = new CollabPlugin({} as any, {} as any);
        await plugin.onload();
        // WASM init should have been called
    });

    describe('onunload', () => {
        it('should handle errors in stopSession gracefully', async () => {
            const plugin = new CollabPlugin({} as any, {} as any);
            await plugin.onload();

            // Mock stopSession to throw
            const originalStopSession = plugin.stopSession.bind(plugin);
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
            const plugin = new CollabPlugin({} as any, {} as any);
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
            const plugin = new CollabPlugin({} as any, {} as any);
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
            const plugin = new CollabPlugin({} as any, {} as any);
            await plugin.onload();

            plugin.onunload();

            expect((plugin as any).collabCore).toBeNull();
        });
    });

    describe('startSession', () => {
        it('should warn about insecure placeholder encryption key', async () => {
            const plugin = new CollabPlugin({} as any, {} as any);
            // Mock workspace to return an active view
            (plugin as any).app = {
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
                },
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
    });
});
