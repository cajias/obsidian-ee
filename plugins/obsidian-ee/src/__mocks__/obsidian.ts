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
    vault?: {
        adapter?: DataAdapterMock;
        on?: () => unknown;
        offref?: () => void;
    };
}

/** The slice of Obsidian's DataAdapter the plugin uses for at-rest state. */
export interface DataAdapterMock {
    exists(path: string): Promise<boolean>;
    readBinary(path: string): Promise<ArrayBuffer>;
    writeBinary(path: string, data: ArrayBuffer): Promise<void>;
}

/** An in-memory DataAdapter, so tests can assert what reached disk. */
export function createAdapterMock(): DataAdapterMock & { files: Map<string, ArrayBuffer> } {
    const files = new Map<string, ArrayBuffer>();
    return {
        files,
        exists: (path) => Promise.resolve(files.has(path)),
        readBinary: (path) => {
            const data = files.get(path);
            return data ? Promise.resolve(data) : Promise.reject(new Error(`ENOENT: ${path}`));
        },
        writeBinary: (path, data) => {
            files.set(path, data);
            return Promise.resolve();
        },
    };
}

interface ManifestMock {
    id?: string;
    name?: string;
    version?: string;
    /** The plugin's own folder, where at-rest state lives. */
    dir?: string;
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
