import { jest } from '@jest/globals';

/**
 * Shared `obsidian` module mock factory for main.test.ts and
 * vault-sync-plugin.test.ts. Both drive the same CollabPlugin surface
 * (Plugin/PluginSettingTab/Setting/Notice/MarkdownView), so the mock lives
 * once here rather than duplicated per test file — mirrors the
 * load-real-wasm.ts helper pattern.
 *
 * Call as `jest.unstable_mockModule('obsidian', mockObsidianModule)` — this
 * is a plain function call (unstable_mockModule isn't hoisted the way
 * jest.mock() is), so factoring it out behaves identically to an inline factory.
 */
export function mockObsidianModule() {
    return {
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
    };
}
