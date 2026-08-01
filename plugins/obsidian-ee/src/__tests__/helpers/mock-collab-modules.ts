import { jest } from '@jest/globals';

/**
 * Shared `jest.unstable_mockModule` factories for '../wasm/collab_wasm',
 * '../collab-client', and '../editor-sync' — used by both main.test.ts and
 * vault-sync-plugin.test.ts (both drive the same CollabPlugin surface).
 * Mirrors the mock-obsidian.ts helper pattern: factored out once rather than
 * hand-rolled per test file.
 */

/** A fresh CollabClient instance surface (every method the plugin calls). */
export function createMockClientInstance() {
    return {
        connect: jest.fn<() => Promise<void>>().mockResolvedValue(undefined),
        disconnect: jest.fn(),
        getText: jest.fn().mockReturnValue(''),
        sendUpdate: jest.fn(),
        sendManifestUpdate: jest.fn(),
        onUpdate: jest.fn(),
        onError: jest.fn(),
        onDisconnect: jest.fn(),
        onManifestPaths: jest.fn(),
    };
}

/**
 * Mock '../collab-client'. Returns the constructed `instance` (so a test can
 * assert on/override its methods across the whole run) and the `CollabClient`
 * constructor mock alongside the ready-to-pass `moduleFactory`.
 */
export function createCollabClientMock() {
    const instance = createMockClientInstance();
    const CollabClient = jest.fn().mockImplementation(() => instance);
    return {
        instance,
        CollabClient,
        moduleFactory: () => ({
            CollabClient,
            // main.ts imports the real extractErrorMessage (not a CollabClient
            // method), so the mock must still provide it with the same behavior.
            extractErrorMessage: (error: unknown): string =>
                error instanceof Error ? error.message : String(error),
        }),
    };
}

/** Mock module factory for '../collab-client' when no instance reference is needed. */
export function mockCollabClientModule() {
    return createCollabClientMock().moduleFactory;
}

/** Mock module factory for '../editor-sync'. */
export function mockEditorSyncModule() {
    return () => ({
        EditorSync: jest.fn().mockImplementation(() => ({
            bindToEditor: jest.fn(),
            unbind: jest.fn(),
            onLocalChange: jest.fn(),
            getText: jest.fn().mockReturnValue(''),
            setErrorCallback: jest.fn(),
        })),
    });
}

/** A fresh WasmVaultSync instance surface (every method the plugin calls). */
export function createMockWasmVaultSyncInstance() {
    return {
        handle_created: jest.fn(),
        handle_deleted: jest.fn(),
        handle_renamed: jest.fn(),
        apply_remote_manifest: jest.fn(),
        list_files: jest.fn(),
        free: jest.fn(),
    };
}

/**
 * Mock '../wasm/collab_wasm'. Returns `wasmInit`, `vaultSyncInstance`, and the
 * `WasmVaultSync` constructor mock alongside the ready-to-pass `moduleFactory`
 * — a test needs these references for `mockResolvedValue`/`mockReturnValue`
 * overrides and call-count assertions.
 */
export function createCollabWasmMock(manifestDocId = '__vault_manifest__') {
    const wasmInit = jest.fn<() => Promise<void>>().mockResolvedValue(undefined);
    const vaultSyncInstance = createMockWasmVaultSyncInstance();
    const WasmVaultSync = jest.fn().mockImplementation(() => vaultSyncInstance);
    return {
        wasmInit,
        vaultSyncInstance,
        WasmVaultSync,
        moduleFactory: () => ({
            __esModule: true,
            default: wasmInit,
            WasmVaultSync,
            manifest_doc_id: jest.fn().mockReturnValue(manifestDocId),
        }),
    };
}
