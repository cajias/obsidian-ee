import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { webcrypto } from 'node:crypto';
import init, { CollabCore } from '../../wasm/collab_wasm';

// getrandom (wasm-pack --target web) calls crypto.getRandomValues, not OS entropy.
// Guard thin hosts so encrypt() never surfaces a getrandom RuntimeError.
if (!(globalThis as { crypto?: Crypto }).crypto) {
    (globalThis as { crypto?: Crypto }).crypto = webcrypto as unknown as Crypto;
}

let initialized = false;

// ESM under ts-jest: __dirname is unavailable, so resolve from import.meta.url.
const here = dirname(fileURLToPath(import.meta.url));

/** Load + init the REAL committed WASM artifact, mirroring main.ts:87-104. */
export async function loadRealWasm(): Promise<{ CollabCore: typeof CollabCore }> {
    if (!initialized) {
        const wasmPath = join(here, '..', '..', 'wasm', 'collab_wasm_bg.wasm');
        const bytes = readFileSync(wasmPath);
        const module = await WebAssembly.compile(bytes);
        await init({ module_or_path: module });
        initialized = true;
    }
    return { CollabCore };
}

export async function newCore(): Promise<InstanceType<typeof CollabCore>> {
    const { CollabCore: Core } = await loadRealWasm();
    return new Core();
}
