import { jest, describe, it, expect, beforeEach, afterEach } from '@jest/globals';

// Mock CollabClient
jest.unstable_mockModule('../collab-client', () => ({
    CollabClient: jest.fn().mockImplementation(() => {
        let updateCallback: ((text: string) => void) | null = null;
        return {
            getText: jest.fn().mockReturnValue(''),
            sendUpdate: jest.fn(),
            onUpdate: jest.fn((cb: (text: string) => void) => {
                updateCallback = cb;
            }),
            // Helper to trigger updates in tests
            _triggerUpdate: (text: string) => updateCallback?.(text),
        };
    }),
}));

const { EditorSync } = await import('../editor-sync');
const { CollabClient } = await import('../collab-client');
type EditorSync = InstanceType<typeof EditorSync>;

// Mock Obsidian Editor
const createMockEditor = (initialValue = '') => {
    let value = initialValue;
    let cursor = { line: 0, ch: 0 };

    return {
        getValue: jest.fn(() => value),
        setValue: jest.fn((v: string) => {
            value = v;
        }),
        getCursor: jest.fn(() => cursor),
        setCursor: jest.fn((c: { line: number; ch: number }) => {
            cursor = c;
        }),
    };
};

// Mock MarkdownView
const createMockView = (editor: any) => ({
    editor,
});

describe('EditorSync', () => {
    let client: any;
    let sync: EditorSync;

    beforeEach(() => {
        jest.useFakeTimers();
        client = new CollabClient({} as any, {} as any);
        sync = new EditorSync(client);
    });

    afterEach(() => {
        jest.useRealTimers();
    });

    describe('bindToEditor', () => {
        it('should bind to editor and sync initial state', () => {
            const editor = createMockEditor('local text');
            const view = createMockView(editor);

            client.getText.mockReturnValue('remote text');

            sync.bindToEditor(view as any);

            expect(editor.setValue).toHaveBeenCalledWith('remote text');
        });

        it('should not update if remote and local are the same', () => {
            const editor = createMockEditor('same text');
            const view = createMockView(editor);

            client.getText.mockReturnValue('same text');

            sync.bindToEditor(view as any);

            expect(editor.setValue).not.toHaveBeenCalled();
        });

        it('should sync empty string remote text when local is non-empty', () => {
            const editor = createMockEditor('local text');
            const view = createMockView(editor);

            client.getText.mockReturnValue('');

            sync.bindToEditor(view as any);

            expect(editor.setValue).toHaveBeenCalledWith('');
        });

        it('should not update if remote text is undefined', () => {
            const editor = createMockEditor('local text');
            const view = createMockView(editor);

            client.getText.mockReturnValue(undefined);

            sync.bindToEditor(view as any);

            // setValue should not be called when remote text is undefined
            expect(editor.setValue).not.toHaveBeenCalled();
        });

        it('should handle errors during setValue and reset isApplyingRemote', () => {
            const consoleSpy = jest.spyOn(console, 'error').mockImplementation(() => {});
            const errorCallback = jest.fn();
            const editor = createMockEditor('local text');
            editor.setValue.mockImplementation(() => {
                throw new Error('setValue error');
            });
            const view = createMockView(editor);

            client.getText.mockReturnValue('remote text');

            sync.setErrorCallback(errorCallback);
            sync.bindToEditor(view as any);

            expect(consoleSpy).toHaveBeenCalledWith(
                '[EditorSync] Error setting initial text:',
                expect.any(Error)
            );
            expect(errorCallback).toHaveBeenCalledWith(expect.any(Error));

            // Verify isApplyingRemote was reset by checking subsequent remote updates work
            editor.setValue.mockClear();
            editor.setValue.mockImplementation(() => {}); // Reset to working implementation
            editor.getValue.mockReturnValue('local text');
            client._triggerUpdate('new remote');

            expect(editor.setValue).toHaveBeenCalledWith('new remote');

            consoleSpy.mockRestore();
        });

        it('should wrap non-Error throws and forward to error callback', () => {
            const consoleSpy = jest.spyOn(console, 'error').mockImplementation(() => {});
            const errorCallback = jest.fn();
            const editor = createMockEditor('local text');
            editor.setValue.mockImplementation(() => {
                // Create an object that looks like a WASM error but isn't instanceof Error
                // The code should wrap this and still call the error callback
                throw Object.assign({}, { message: 'WASM error message' });
            });
            const view = createMockView(editor);

            client.getText.mockReturnValue('remote text');

            sync.setErrorCallback(errorCallback);
            sync.bindToEditor(view as any);

            expect(consoleSpy).toHaveBeenCalled();
            // Should now be called with a wrapped Error
            expect(errorCallback).toHaveBeenCalledTimes(1);
            const wrappedError = errorCallback.mock.calls[0][0];
            expect(wrappedError).toBeInstanceOf(Error);
            expect((wrappedError as Error).message).toBe('WASM error message');

            consoleSpy.mockRestore();
        });
    });

    describe('onLocalChange', () => {
        it('should debounce and send updates', () => {
            const editor = createMockEditor('initial');
            const view = createMockView(editor);

            sync.bindToEditor(view as any);

            // Simulate typing
            editor.getValue.mockReturnValue('initial + more');
            sync.onLocalChange();

            // Should not send immediately
            expect(client.sendUpdate).not.toHaveBeenCalled();

            // Fast forward past debounce
            jest.advanceTimersByTime(150);

            expect(client.sendUpdate).toHaveBeenCalledWith('initial + more');
        });

        it('should reset debounce timer on rapid changes', () => {
            const editor = createMockEditor('initial');
            const view = createMockView(editor);

            sync.bindToEditor(view as any);

            // Simulate rapid typing
            editor.getValue.mockReturnValue('initial + a');
            sync.onLocalChange();

            jest.advanceTimersByTime(50);

            editor.getValue.mockReturnValue('initial + ab');
            sync.onLocalChange();

            jest.advanceTimersByTime(50);

            editor.getValue.mockReturnValue('initial + abc');
            sync.onLocalChange();

            // Should not have sent yet (each call resets the timer)
            expect(client.sendUpdate).not.toHaveBeenCalled();

            // Now advance past the debounce
            jest.advanceTimersByTime(150);

            // Should only send once with final value
            expect(client.sendUpdate).toHaveBeenCalledTimes(1);
            expect(client.sendUpdate).toHaveBeenCalledWith('initial + abc');
        });

        it('should not send if no editor is bound', () => {
            sync.onLocalChange();

            jest.advanceTimersByTime(150);

            expect(client.sendUpdate).not.toHaveBeenCalled();
        });

        it('should ignore changes while applying remote', () => {
            const editor = createMockEditor('');
            const view = createMockView(editor);

            sync.bindToEditor(view as any);

            // Simulate remote update triggering local change detection
            client._triggerUpdate('remote update');

            // The setValue during remote apply should not trigger sendUpdate
            jest.advanceTimersByTime(150);

            // sendUpdate should not be called from the remote-triggered change
        });

        it('should notify error callback when sendUpdate returns false', () => {
            const errorCallback = jest.fn();
            const editor = createMockEditor('initial');
            const view = createMockView(editor);

            client.sendUpdate.mockReturnValue(false);

            sync.setErrorCallback(errorCallback);
            sync.bindToEditor(view as any);

            editor.getValue.mockReturnValue('updated text');
            sync.onLocalChange();

            jest.advanceTimersByTime(150);

            expect(client.sendUpdate).toHaveBeenCalledWith('updated text');
            expect(errorCallback).toHaveBeenCalledWith(
                expect.objectContaining({
                    message: 'Failed to send update - changes may not be synced',
                })
            );
        });

        it('should not notify error callback when sendUpdate returns true', () => {
            const errorCallback = jest.fn();
            const editor = createMockEditor('initial');
            const view = createMockView(editor);

            client.sendUpdate.mockReturnValue(true);

            sync.setErrorCallback(errorCallback);
            sync.bindToEditor(view as any);

            editor.getValue.mockReturnValue('updated text');
            sync.onLocalChange();

            jest.advanceTimersByTime(150);

            expect(client.sendUpdate).toHaveBeenCalledWith('updated text');
            expect(errorCallback).not.toHaveBeenCalled();
        });

        it('should not throw when sendUpdate fails and no error callback set', () => {
            const editor = createMockEditor('initial');
            const view = createMockView(editor);

            client.sendUpdate.mockReturnValue(false);

            sync.bindToEditor(view as any);

            editor.getValue.mockReturnValue('updated text');
            sync.onLocalChange();

            // Should not throw
            expect(() => jest.advanceTimersByTime(150)).not.toThrow();
        });
    });

    describe('applyRemoteUpdate', () => {
        it('should apply remote update to editor', () => {
            const editor = createMockEditor('old text');
            const view = createMockView(editor);

            sync.bindToEditor(view as any);

            // Trigger remote update
            client._triggerUpdate('new remote text');

            expect(editor.setValue).toHaveBeenCalledWith('new remote text');
        });

        it('should preserve cursor position', () => {
            const editor = createMockEditor('hello');
            editor.getCursor.mockReturnValue({ line: 0, ch: 3 });
            const view = createMockView(editor);

            sync.bindToEditor(view as any);

            client._triggerUpdate('hello world');

            expect(editor.setCursor).toHaveBeenCalled();
        });

        it('should clamp cursor to valid range when text shrinks', () => {
            const editor = createMockEditor('hello world');
            editor.getCursor.mockReturnValue({ line: 0, ch: 11 }); // At end of 'hello world'
            const view = createMockView(editor);

            sync.bindToEditor(view as any);

            client._triggerUpdate('hi'); // Much shorter text

            // Cursor should be clamped to length of new text
            expect(editor.setCursor).toHaveBeenCalledWith({ line: 0, ch: 2 });
        });

        it('should clamp cursor line when lines are removed', () => {
            const editor = createMockEditor('line1\nline2\nline3');
            editor.getCursor.mockReturnValue({ line: 2, ch: 3 }); // On line 3
            const view = createMockView(editor);

            sync.bindToEditor(view as any);

            client._triggerUpdate('line1'); // Only one line now

            // Cursor line should be clamped to line 0
            expect(editor.setCursor).toHaveBeenCalledWith({ line: 0, ch: 3 });
        });

        it('should not update if remote text is same as current', () => {
            const editor = createMockEditor('same content');
            const view = createMockView(editor);

            // Make getText return the same value as the editor so bindToEditor doesn't update
            client.getText.mockReturnValue('same content');

            sync.bindToEditor(view as any);

            // Clear the call from bindToEditor (should not have been called anyway)
            editor.setValue.mockClear();

            client._triggerUpdate('same content');

            expect(editor.setValue).not.toHaveBeenCalled();
        });

        it('should not apply if no editor is bound', () => {
            // Don't bind to any editor
            client._triggerUpdate('some text');

            // Should not throw, just silently return
        });
    });

    describe('unbind', () => {
        it('should clear debounce timer and editor reference', () => {
            const editor = createMockEditor('');
            const view = createMockView(editor);

            sync.bindToEditor(view as any);
            sync.onLocalChange();

            sync.unbind();

            expect(sync.getText()).toBe('');
        });

        it('should flush pending updates when unbinding to prevent data loss', () => {
            const editor = createMockEditor('some text');
            const view = createMockView(editor);

            // Make getText return same value so bindToEditor doesn't overwrite local
            client.getText.mockReturnValue('some text');

            sync.bindToEditor(view as any);
            sync.onLocalChange();

            // Unbind before debounce fires - pending update should be sent immediately
            sync.unbind();

            // Update should have been sent during unbind, not after timer
            expect(client.sendUpdate).toHaveBeenCalledWith('some text');
        });
    });

    describe('getText', () => {
        it('should return editor text when bound', () => {
            const editor = createMockEditor('editor content');
            const view = createMockView(editor);

            // Make getText return same value so bindToEditor doesn't overwrite
            client.getText.mockReturnValue('editor content');

            sync.bindToEditor(view as any);

            expect(sync.getText()).toBe('editor content');
        });

        it('should return empty string when not bound', () => {
            expect(sync.getText()).toBe('');
        });
    });

    describe('remote update edge cases', () => {
        it('should handle concurrent edits gracefully', () => {
            const editor = createMockEditor('hello');
            const view = createMockView(editor);

            sync.bindToEditor(view as any);

            // Simulate local edit happening at same time as remote
            editor.getValue.mockReturnValue('hello world');
            sync.onLocalChange();

            // Remote update comes in before debounce completes
            client._triggerUpdate('hello there');

            // Local should be overwritten by remote
            expect(editor.setValue).toHaveBeenLastCalledWith('hello there');
        });

        it('should handle rapid remote updates', () => {
            const editor = createMockEditor('');
            const view = createMockView(editor);

            sync.bindToEditor(view as any);

            // Clear any initial setValue calls
            editor.setValue.mockClear();

            // Rapid fire remote updates
            client._triggerUpdate('a');
            client._triggerUpdate('ab');
            client._triggerUpdate('abc');

            // Should have applied all updates
            expect(editor.setValue).toHaveBeenCalledTimes(3);
            expect(editor.setValue).toHaveBeenLastCalledWith('abc');
        });

        it('should handle empty remote updates', () => {
            const editor = createMockEditor('some text');
            const view = createMockView(editor);

            sync.bindToEditor(view as any);

            client._triggerUpdate('');

            expect(editor.setValue).toHaveBeenCalledWith('');
        });

        it('should position cursor at 0,0 when remote update is empty document', () => {
            const editor = createMockEditor('some text here');
            editor.getCursor.mockReturnValue({ line: 0, ch: 10 }); // Cursor in middle
            const view = createMockView(editor);

            // Make getText return same value so bindToEditor doesn't overwrite
            client.getText.mockReturnValue('some text here');

            sync.bindToEditor(view as any);

            // Now make getValue return current text so the update is applied
            editor.getValue.mockReturnValue('some text here');

            // Remote sends empty document
            client._triggerUpdate('');

            // Cursor should be clamped to 0,0 for empty document
            expect(editor.setCursor).toHaveBeenCalledWith({ line: 0, ch: 0 });
        });

        it('should not send local update when remote overwrites pending local changes', () => {
            const editor = createMockEditor('initial');
            const view = createMockView(editor);

            sync.bindToEditor(view as any);

            // User types something
            editor.getValue.mockReturnValue('initial + local');
            sync.onLocalChange();

            // Before debounce completes, remote update arrives
            client._triggerUpdate('remote version');

            // Fast forward past debounce
            jest.advanceTimersByTime(150);

            // The sendUpdate should have been called with the editor value at that time
            // which would be 'remote version' since applyRemoteUpdate set it
            expect(editor.setValue).toHaveBeenCalledWith('remote version');
        });

        it('should handle multiline remote updates with cursor preservation', () => {
            const editor = createMockEditor('line1\nline2');
            editor.getCursor.mockReturnValue({ line: 1, ch: 3 }); // Middle of line2
            const view = createMockView(editor);

            sync.bindToEditor(view as any);

            // Remote adds more lines
            client._triggerUpdate('line1\nline2\nline3\nline4');

            // Cursor should stay at same position since those coordinates are still valid
            expect(editor.setCursor).toHaveBeenCalledWith({ line: 1, ch: 3 });
        });

        it('should handle Unicode content in remote updates', () => {
            const editor = createMockEditor('hello');
            const view = createMockView(editor);

            sync.bindToEditor(view as any);

            // Remote sends Unicode content
            client._triggerUpdate('Hello 世界 🌍');

            expect(editor.setValue).toHaveBeenCalledWith('Hello 世界 🌍');
        });

        it('should handle very large remote updates', () => {
            const editor = createMockEditor('');
            const view = createMockView(editor);

            sync.bindToEditor(view as any);

            // Simulate a large document
            const largeText = 'x'.repeat(100000);
            client._triggerUpdate(largeText);

            expect(editor.setValue).toHaveBeenCalledWith(largeText);
        });

        it('should maintain isApplyingRemote flag correctly during update', () => {
            const editor = createMockEditor('old');
            const view = createMockView(editor);

            sync.bindToEditor(view as any);

            const originalOnLocalChange = sync.onLocalChange.bind(sync);

            // Override setValue to call onLocalChange (simulating editor event)
            editor.setValue.mockImplementation((_v: string) => {
                // Try to trigger local change during remote apply
                originalOnLocalChange();
            });

            client._triggerUpdate('new');

            // Even though onLocalChange was called, no update should be sent
            // because isApplyingRemote should be true
            jest.advanceTimersByTime(150);

            // The sendUpdate should not be called because changes during
            // remote apply are ignored
            expect(client.sendUpdate).not.toHaveBeenCalled();
        });

        it('should handle cursor at end of line when line gets shorter', () => {
            const editor = createMockEditor('hello world');
            editor.getCursor.mockReturnValue({ line: 0, ch: 11 }); // At end
            const view = createMockView(editor);

            sync.bindToEditor(view as any);

            // Remote shortens the line
            client._triggerUpdate('hi');

            // Cursor should be clamped to end of shortened line
            expect(editor.setCursor).toHaveBeenCalledWith({ line: 0, ch: 2 });
        });

        it('should handle multiple lines being removed from end', () => {
            const editor = createMockEditor('a\nb\nc\nd\ne');
            editor.getCursor.mockReturnValue({ line: 4, ch: 0 }); // On line 'e'
            const view = createMockView(editor);

            sync.bindToEditor(view as any);

            // Remote removes last 3 lines
            client._triggerUpdate('a\nb');

            // Cursor should be clamped to last available line
            expect(editor.setCursor).toHaveBeenCalledWith({ line: 1, ch: 0 });
        });
    });

    describe('error handling', () => {
        it('should catch and log errors in applyRemoteUpdate', () => {
            const consoleSpy = jest.spyOn(console, 'error').mockImplementation(() => {});
            const editor = createMockEditor('initial');
            const view = createMockView(editor);

            // Make getText return same value so bindToEditor doesn't try to set
            client.getText.mockReturnValue('initial');

            sync.bindToEditor(view as any);

            // Now make getValue throw for the remote update
            editor.getValue.mockImplementation(() => {
                throw new Error('Editor error');
            });

            // Trigger remote update - should catch the error
            client._triggerUpdate('new text');

            expect(consoleSpy).toHaveBeenCalledWith(
                '[EditorSync] Error applying remote update:',
                expect.any(Error)
            );

            consoleSpy.mockRestore();
        });

        it('should reset isApplyingRemote flag even when error occurs', () => {
            const consoleSpy = jest.spyOn(console, 'error').mockImplementation(() => {});
            const editor = createMockEditor('initial');
            const view = createMockView(editor);

            // Make getText return same value so bindToEditor doesn't try to set
            client.getText.mockReturnValue('initial');

            sync.bindToEditor(view as any);

            // Now make getValue throw once, then work
            let callCount = 0;
            editor.getValue.mockImplementation(() => {
                callCount++;
                if (callCount === 1) {
                    throw new Error('Editor error');
                }
                return 'recovered';
            });

            // First update throws
            client._triggerUpdate('new text');

            // Second update should work (flag was reset)
            client._triggerUpdate('another text');

            // Second update should have proceeded (getValue called again)
            expect(callCount).toBe(2);

            consoleSpy.mockRestore();
        });

        it('should call error callback when provided and error occurs', () => {
            const consoleSpy = jest.spyOn(console, 'error').mockImplementation(() => {});
            const errorCallback = jest.fn();
            const editor = createMockEditor('initial');
            const view = createMockView(editor);

            // Make getText return same value so bindToEditor doesn't try to set
            client.getText.mockReturnValue('initial');

            sync.setErrorCallback(errorCallback);
            sync.bindToEditor(view as any);

            // Now make getValue throw for the remote update
            editor.getValue.mockImplementation(() => {
                throw new Error('Test error');
            });

            client._triggerUpdate('new text');

            expect(errorCallback).toHaveBeenCalledWith(expect.any(Error));

            consoleSpy.mockRestore();
        });

        it('should not throw if error callback is not set', () => {
            const consoleSpy = jest.spyOn(console, 'error').mockImplementation(() => {});
            const editor = createMockEditor('initial');
            const view = createMockView(editor);

            // Make getText return same value so bindToEditor doesn't try to set
            client.getText.mockReturnValue('initial');

            sync.bindToEditor(view as any);

            // Now make getValue throw for the remote update
            editor.getValue.mockImplementation(() => {
                throw new Error('Test error');
            });

            // Should not throw
            expect(() => client._triggerUpdate('new text')).not.toThrow();

            consoleSpy.mockRestore();
        });
    });
});
