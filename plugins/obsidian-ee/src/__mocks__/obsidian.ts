// Mock Obsidian API for testing
export class Plugin {
    app: any = {};
    manifest: any = {};

    addCommand(command: any): void {}
    addRibbonIcon(icon: string, title: string, callback: () => void): HTMLElement {
        return document.createElement('div');
    }
    registerEvent(event: any): void {}
}

export class Notice {
    constructor(message: string, timeout?: number) {}
}

export class MarkdownView {
    editor: any = {};
}
