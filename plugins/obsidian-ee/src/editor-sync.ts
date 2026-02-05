import { MarkdownView, Editor } from 'obsidian';
import { CollabClient } from './collab-client';

/**
 * Wrap any throwable in an Error instance for consistent error handling.
 * WASM errors are plain objects with {type, message} structure.
 */
function wrapError(error: unknown): Error {
    if (error instanceof Error) {
        return error;
    }
    if (
        typeof error === 'object' &&
        error !== null &&
        'message' in error &&
        typeof (error as { message: unknown }).message === 'string'
    ) {
        return new Error((error as { message: string }).message);
    }
    return new Error(String(error));
}

export class EditorSync {
    private client: CollabClient;
    private editor: Editor | null = null;
    private isApplyingRemote = false;
    private debounceTimer: NodeJS.Timeout | null = null;
    private debounceMs = 100;
    private errorCallback: ((error: Error) => void) | null = null;

    constructor(client: CollabClient) {
        this.client = client;

        // Listen for remote updates
        this.client.onUpdate((text) => {
            this.applyRemoteUpdate(text);
        });
    }

    /**
     * Set an error callback to be notified of errors during sync operations
     */
    setErrorCallback(callback: (error: Error) => void): void {
        this.errorCallback = callback;
    }

    /**
     * Bind to an Obsidian MarkdownView's editor
     */
    bindToEditor(view: MarkdownView): void {
        this.editor = view.editor;

        // Initialize with current remote state
        const remoteText = this.client.getText();
        if (remoteText !== undefined && remoteText !== this.editor.getValue()) {
            this.isApplyingRemote = true;
            try {
                this.editor.setValue(remoteText);
            } catch (error) {
                console.error('[EditorSync] Error setting initial text:', error);
                if (this.errorCallback) {
                    this.errorCallback(wrapError(error));
                }
            } finally {
                this.isApplyingRemote = false;
            }
        }
    }

    /**
     * Handle local editor changes
     * Call this from the editor's change event
     */
    onLocalChange(): void {
        if (this.isApplyingRemote || !this.editor) return;

        // Debounce to avoid sending too many updates
        if (this.debounceTimer) {
            clearTimeout(this.debounceTimer);
        }

        this.debounceTimer = setTimeout(() => {
            this.sendLocalUpdate();
        }, this.debounceMs);
    }

    private sendLocalUpdate(): void {
        // Guard against execution after unbind - both editor and timer should be valid
        if (!this.editor) return;

        const text = this.editor.getValue();
        const sent = this.client.sendUpdate(text);

        if (!sent && this.errorCallback) {
            this.errorCallback(new Error('Failed to send update - changes may not be synced'));
        }
    }

    private applyRemoteUpdate(text: string): void {
        if (!this.editor) return;

        // Don't trigger local change events while applying remote
        this.isApplyingRemote = true;

        try {
            const currentText = this.editor.getValue();

            // Only update if text is different
            if (text !== currentText) {
                // Preserve cursor position if possible
                const cursor = this.editor.getCursor();

                this.editor.setValue(text);

                // Restore cursor (clamped to valid range)
                const lines = text.split('\n');
                const newLineCount = lines.length;
                // Handle empty document case - ensure targetLine is at least 0
                const targetLine = Math.max(0, Math.min(cursor.line, newLineCount - 1));
                const targetLineContent = lines.at(targetLine) ?? '';
                const newCursor = {
                    line: targetLine,
                    ch: Math.min(cursor.ch, targetLineContent.length),
                };
                this.editor.setCursor(newCursor);
            }
        } catch (error) {
            console.error('[EditorSync] Error applying remote update:', error);
            if (this.errorCallback) {
                this.errorCallback(wrapError(error));
            }
        } finally {
            this.isApplyingRemote = false;
        }
    }

    /**
     * Unbind from the current editor
     * Flushes any pending updates before disconnecting to prevent data loss
     */
    unbind(): void {
        // Clear timer first to prevent any queued callbacks from executing
        if (this.debounceTimer) {
            clearTimeout(this.debounceTimer);
            this.debounceTimer = null;
        }

        // Flush any pending changes while editor is still valid
        if (this.editor) {
            // Send final update synchronously before nulling editor
            this.sendLocalUpdate();
        }

        this.editor = null;
    }

    /**
     * Get the current editor text
     */
    getText(): string {
        return this.editor?.getValue() || '';
    }
}
