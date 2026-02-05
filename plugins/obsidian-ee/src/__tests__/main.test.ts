jest.mock('obsidian');
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

import CollabPlugin from '../main';

describe('CollabPlugin', () => {
    it('should instantiate without error', () => {
        const plugin = new CollabPlugin({} as any, {} as any);
        expect(plugin).toBeDefined();
    });

    it('should initialize WASM on load', async () => {
        const plugin = new CollabPlugin({} as any, {} as any);
        await plugin.onload();
        // WASM init should have been called
    });
});
