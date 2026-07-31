/**
 * Cheapest durable regression guard for issue #28: assert the AES PSK surface
 * never creeps back into the plugin's runtime source. MLS is the sole crypto.
 *
 * Scoped to the runtime source files (not the whole tree) because the test files
 * legitimately mention these tokens inside "AES is gone" negation assertions
 * (mls-wasm.test.ts, collab-client.test.ts, main.test.ts). Guarding the runtime
 * files is what actually matters — they are where a real AES path would reappear.
 */
import { describe, it, expect } from '@jest/globals';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const srcDir = join(here, '..');

// The plugin's crypto-carrying runtime source (excludes generated wasm/*.d.ts).
const RUNTIME_FILES = ['collab-client.ts', 'main.ts', 'editor-sync.ts'];

// Case-insensitive AES-path markers. `\baes\b` (whole word) so unrelated
// substrings can't false-positive; the others are unambiguous identifiers.
const FORBIDDEN: RegExp[] = [/set_encryption_key/i, /encryptionkey/i, /collabcore/i, /\baes\b/i];

describe('no AES regression in plugin runtime source', () => {
    it.each(RUNTIME_FILES)('%s contains no AES PSK surface tokens', (file) => {
        const contents = readFileSync(join(srcDir, file), 'utf8');
        for (const pattern of FORBIDDEN) {
            expect(contents).not.toMatch(pattern);
        }
    });
});
