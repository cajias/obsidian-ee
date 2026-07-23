#!/usr/bin/env node
// PostToolUse hook: whole-project type-check the plugin after a plugin .ts edit.
// Companion to eslint-changed.mjs — eslint never type-checks, so a green
// `npm test` can hide TypeScript breakage (e.g. an isolatedModules/transpile-only
// jest config, or a divergent tsconfig). This runs `tsc --noEmit` over the whole
// program so type regressions surface immediately.
// PERF: tsc is slower than single-file eslint (~a few seconds) because it must
// build the whole program — single-file `tsc` ignores tsconfig, so we can't
// scope it down. That latency is the deliberate cost of catching regressions a
// green test suite can hide. Keep it simple; no caching.
// Exits 2 (with stderr fed back to Claude) on type errors so the agent fixes
// them immediately; exits 0 otherwise. Silent/no-op for non-plugin-TS files.
import { execFileSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import path from 'node:path';

let raw = '';
process.stdin.on('data', (c) => (raw += c));
process.stdin.on('end', () => {
  let fp = '';
  try {
    fp = JSON.parse(raw)?.tool_input?.file_path ?? '';
  } catch {
    process.exit(0);
  }
  if (!fp) process.exit(0);
  const root = process.env.CLAUDE_PROJECT_DIR || process.cwd();
  const pluginSrc = path.join('plugins', 'obsidian-ee', 'src') + path.sep;
  const rel = path.relative(root, fp);
  if (!rel.startsWith(pluginSrc) || !fp.endsWith('.ts')) process.exit(0);
  const wasmDts = path.join(root, 'plugins', 'obsidian-ee', 'src', 'wasm', 'collab_wasm.d.ts');
  if (!existsSync(wasmDts)) {
    // WASM is built on demand (gitignored, decision B). Without it, tsc emits
    // spurious TS2307 module-not-found errors unrelated to the edit. Skip the
    // type-check until the artifact exists; run `./scripts/build-wasm.sh` first.
    process.stderr.write('typecheck-plugin: skipped — src/wasm not built (run ./scripts/build-wasm.sh). Not a type error.\n');
    process.exit(0);
  }
  const cwd = path.join(root, 'plugins', 'obsidian-ee');
  try {
    execFileSync('npx', ['tsc', '--noEmit'], { cwd, stdio: 'pipe' });
    process.exit(0);
  } catch (e) {
    const out = (e.stdout?.toString() || '') + (e.stderr?.toString() || '');
    process.stderr.write(`tsc found type errors (plugin):\n${out}\n`);
    process.exit(2);
  }
});
