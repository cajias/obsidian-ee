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
/**
 * A stand-in for Obsidian's `TextComponent`. The settings tab reads the live
 * `inputEl.value` and commits on blur/Enter (#97), so the fake has to be a real
 * `EventTarget` with a `value` — a bare `jest.fn()` would let a keystroke-leaking
 * settings tab pass its own test.
 */
export interface FakeTextComponent {
    inputEl: EventTarget & { value: string };
    setPlaceholder(placeholder: string): FakeTextComponent;
    setValue(value: string): FakeTextComponent;
    onChange(handler: (value: string) => unknown): FakeTextComponent;
    /** Simulate a keystroke: Obsidian updates the element, then fires onChange. */
    type(value: string): void;
    blur(): void;
    pressEnter(): void;
}

function createFakeText(): FakeTextComponent {
    const inputEl = new (class extends EventTarget {
        value = '';
    })();
    let onChangeHandler: ((value: string) => unknown) | null = null;
    const text: FakeTextComponent = {
        inputEl,
        setPlaceholder: () => text,
        // setValue seeds the field programmatically; Obsidian does NOT fire
        // onChange for it, and neither does this.
        setValue: (value: string) => {
            inputEl.value = value;
            return text;
        },
        onChange: (handler: (value: string) => unknown) => {
            onChangeHandler = handler;
            return text;
        },
        type: (value: string) => {
            inputEl.value = value;
            onChangeHandler?.(value);
        },
        blur: () => {
            inputEl.dispatchEvent(new Event('blur'));
        },
        pressEnter: () => {
            inputEl.dispatchEvent(Object.assign(new Event('keydown'), { key: 'Enter' }));
        },
    };
    return text;
}

/** A stand-in for one `new Setting(containerEl)` row, keyed by its display name. */
export interface FakeSetting {
    name: string;
    texts: FakeTextComponent[];
    setName(name: string): FakeSetting;
    setDesc(desc: string): FakeSetting;
    addText(cb: (text: FakeTextComponent) => unknown): FakeSetting;
}

/** Find a rendered settings row by name across every `new Setting(...)` call. */
export function findSetting(SettingMock: any, name: string): FakeSetting | undefined {
    return SettingMock.mock.results
        .map((result: { value: FakeSetting }) => result.value)
        .find((setting: FakeSetting) => setting?.name === name);
}

export function mockObsidianModule() {
    return {
        Plugin: class {
            app: any;
            manifest: any;
            // Captured so a test can drive the settings tab the plugin builds.
            settingTab: any = null;
            constructor(app: any, manifest: any) {
                this.app = app;
                this.manifest = manifest;
            }
            addCommand(_cmd: any): void {}
            addSettingTab(tab: any): void {
                this.settingTab = tab;
            }
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
        Setting: jest.fn().mockImplementation(() => {
            const setting: FakeSetting = {
                name: '',
                texts: [],
                setName: (name: string) => {
                    setting.name = name;
                    return setting;
                },
                setDesc: () => setting,
                // The old mock returned `this` WITHOUT invoking the callback, so
                // nothing in the settings tab was ever executed and any test of
                // it passed vacuously. Invoke it for real.
                addText: (cb: (text: FakeTextComponent) => unknown) => {
                    const text = createFakeText();
                    setting.texts.push(text);
                    cb(text);
                    return setting;
                },
            };
            return setting;
        }),
        Notice: jest.fn(),
        MarkdownView: class {},
    };
}
