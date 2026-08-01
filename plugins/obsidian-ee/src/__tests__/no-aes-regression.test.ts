/**
 * Cheapest durable regression guard for issue #28: assert the AES PSK surface
 * never creeps back into the plugin's runtime source. MLS is the sole crypto.
 *
 * Recurses the whole `src/` tree so a NEW runtime file can't smuggle AES back in
 * (the old hardcoded 3-file allowlist missed that). Excludes: `*.d.ts` (generated
 * type defs), the `__tests__/` directory (test files legitimately mention these
 * tokens inside "AES is gone" negation assertions), and the generated `wasm/` dir.
 */
import { describe, it, expect } from '@jest/globals';
import { readFileSync, readdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const srcDir = join(here, '..');

// Directories that legitimately carry these tokens or are generated.
const EXCLUDED_DIRS = new Set(['__tests__', 'wasm', '__mocks__']);

/** Recursively collect runtime `*.ts` source files under `dir`. */
function collectRuntimeFiles(dir: string): string[] {
    const files: string[] = [];
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
        if (entry.isDirectory()) {
            if (!EXCLUDED_DIRS.has(entry.name)) {
                files.push(...collectRuntimeFiles(join(dir, entry.name)));
            }
        } else if (entry.name.endsWith('.ts') && !entry.name.endsWith('.d.ts')) {
            files.push(join(dir, entry.name));
        }
    }
    return files;
}

const RUNTIME_FILES = collectRuntimeFiles(srcDir);

// Case-insensitive AES-path markers. `\baes\b` (whole word) so unrelated
// substrings can't false-positive; the others are unambiguous identifiers.
const FORBIDDEN: RegExp[] = [/set_encryption_key/i, /encryptionkey/i, /collabcore/i, /\baes\b/i];

describe('no AES regression in plugin runtime source', () => {
    it('discovers runtime source files to scan', () => {
        // Guard against a broken recursion silently scanning nothing.
        expect(RUNTIME_FILES.length).toBeGreaterThan(0);
    });

    it.each(RUNTIME_FILES)('%s contains no AES PSK surface tokens', (file) => {
        const contents = readFileSync(file, 'utf8');
        for (const pattern of FORBIDDEN) {
            expect(contents).not.toMatch(pattern);
        }
    });
});
