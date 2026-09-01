#!/usr/bin/env node
// Stop hook: type-check the plugin ONCE per turn instead of once per .ts edit.
// Companion to eslint-changed.mjs — eslint never type-checks, so a green
// `npm test` can hide TypeScript breakage (transpile-only jest, isolatedModules,
// a divergent tsconfig). `tsc --noEmit` over the whole program catches it.
// PERF: that measures 1904ms, and the per-edit version paid it on EVERY plugin
// .ts edit. It cannot be scoped down — single-file `tsc` ignores tsconfig — so
// batching once per turn is the only way to cut the cost. Same move as clippy.
// The WASM guard below is three-state, not two: absent .d.ts skips, STALE .d.ts
// blocks with the rebuild command (its "exists" says nothing about "current").
// Exit contract (Stop hook): exit 0 ends the turn and BOTH streams go only to
// the debug log — Claude never sees them, so there is no point writing a message
// before an exit 0. Exit 2 prevents the turn from ending and feeds stderr to
// Claude. Any other non-zero is a non-blocking error and the turn ends anyway,
// so this hook only ever exits 0 or 2.
import { execFileSync } from 'node:child_process';
import { existsSync, readdirSync, statSync } from 'node:fs';
import path from 'node:path';

let raw = '';
process.stdin.on('data', (c) => (raw += c));
process.stdin.on('end', () => {
  let payload;
  try {
    payload = JSON.parse(raw);
  } catch {
    process.exit(0);
  }
  // Sanctioned loop guard: true when Claude Code is already continuing because
  // of a previous stop hook. The agent got one clear shot at the type errors;
  // nagging again risks burning turns on an error it cannot or will not fix, and
  // pre-commit + CI remain the real gate. Block once, then get out of the way.
  if (payload?.stop_hook_active === true) process.exit(0);

  const root = payload?.cwd || process.env.CLAUDE_PROJECT_DIR || process.cwd();
  const git = (args) => execFileSync('git', ['-C', root, ...args], { stdio: 'pipe' }).toString();
  try {
    git(['rev-parse', '--show-toplevel']);
  } catch {
    process.exit(0);
  }

  // Fast bail when no plugin TypeScript changed — this is what keeps the hook
  // free on non-TS turns (no npx spawn at all). Tracked changes vs HEAD (staged
  // + unstaged) plus untracked new files.
  const pluginDir = path.join('plugins', 'obsidian-ee') + '/';
  let changed;
  try {
    changed = [
      ...git(['diff', '--name-only', '--diff-filter=ACM', 'HEAD']).split('\n'),
      ...git(['ls-files', '--others', '--exclude-standard']).split('\n'),
    ].filter((p) => p.startsWith(pluginDir) && p.endsWith('.ts'));
  } catch {
    process.exit(0);
  }
  if (changed.length === 0) process.exit(0);

  // WASM is built on demand (gitignored, decision B). Without the .d.ts, tsc
  // emits spurious TS2307 module-not-found errors unrelated to the edit, so skip
  // the type-check until `./scripts/build-wasm.sh` has produced it. Silent: an
  // exit-0 message would be invisible anyway.
  const wasmDts = path.join(root, 'plugins', 'obsidian-ee', 'src', 'wasm', 'collab_wasm.d.ts');
  if (!existsSync(wasmDts)) process.exit(0);

  // ...but "exists" is not "current". The .d.ts is generated from Rust that
  // changes far more often than the plugin TS, so a STALE artifact passes the
  // existence check and then yields phantom errors about the bindings rather
  // than about the edit — worse than an absent one, because it looks real. (Seen
  // for real: a 2026-07-31 .d.ts against 2026-08-01 vault-sync Rust, missing
  // WasmVaultSync, manifest_doc_id, WasmSyncAction, encrypt_bytes,
  // decrypt_bytes.) Block with the rebuild command instead of running tsc.
  const newestRs = Math.max(
    0,
    ...['collab-wasm', 'collab-core'].flatMap((crate) => {
      const dir = path.join(root, 'crates', crate);
      try {
        return readdirSync(dir, { recursive: true })
          .filter((f) => f.endsWith('.rs'))
          .map((f) => statSync(path.join(dir, f)).mtimeMs);
      } catch {
        return []; // crate missing or unreadable: nothing to compare against
      }
    }),
  );
  if (statSync(wasmDts).mtimeMs < newestRs) {
    process.stderr.write(
      'WASM bindings are stale: plugins/obsidian-ee/src/wasm/collab_wasm.d.ts is older than the Rust in crates/collab-wasm or crates/collab-core.\n' +
        'Skipping the type-check — against a stale .d.ts its errors would be phantom bindings errors, unrelated to the edit.\n' +
        'Run `./scripts/build-wasm.sh`, then the plugin type-check runs again.\n',
    );
    process.exit(2);
  }

  try {
    execFileSync('npx', ['tsc', '--noEmit'], {
      cwd: path.join(root, 'plugins', 'obsidian-ee'),
      stdio: 'pipe',
      timeout: 300000,
    });
    process.exit(0);
  } catch (e) {
    // No npx, or a timeout: silently defer to pre-commit/CI rather than block a
    // turn on an infrastructure hiccup. `e.killed` is undefined even on a real
    // timeout, so ETIMEDOUT is the only reliable discriminator.
    if (e.code === 'ENOENT' || e.code === 'ETIMEDOUT') process.exit(0);
    // Truncated so a wall of errors does not flood the context.
    const out = ((e.stdout?.toString() || '') + (e.stderr?.toString() || '')).split('\n').slice(0, 100).join('\n');
    process.stderr.write(
      `Plugin type errors — fix these before finishing (truncated; run \`npx tsc --noEmit\` in plugins/obsidian-ee for the full list):\n${out}\n`,
    );
    process.exit(2);
  }
});
