// Mock Obsidian API for testing

interface CommandDefinition {
    id: string;
    name: string;
    callback?: () => void;
}

interface AppMock {
    workspace?: {
        getActiveViewOfType?: () => unknown;
        on?: () => unknown;
        offref?: () => void;
    };
}

interface ManifestMock {
    id?: string;
    name?: string;
    version?: string;
}

interface EditorMock {
    getValue?: () => string;
    setValue?: (value: string) => void;
    getCursor?: () => { line: number; ch: number };
    setCursor?: (pos: { line: number; ch: number }) => void;
}

export class Plugin {
    app: AppMock = {};
    manifest: ManifestMock = {};

    addCommand(_command: CommandDefinition): void {}
    addRibbonIcon(_icon: string, _title: string, _callback: () => void): HTMLElement {
        return document.createElement('div');
    }
    registerEvent(_event: unknown): void {}
}

export class Notice {
    constructor(_message: string, _timeout?: number) {}
}

export class MarkdownView {
    editor: EditorMock = {};
}
