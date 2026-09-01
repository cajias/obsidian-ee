#!/usr/bin/env node
// PostToolUse hook: format + clippy the crate owning a just-edited .rs file.
// The other two hooks only cover the TypeScript plugin, so Rust edits (most of
// the source tree) got no feedback until pre-commit/CI. This closes that gap.
// rustfmt rewrites the edited file in place — that is intended, it is the fast
// single-file equivalent of `cargo fmt`.
// PERF: clippy is scoped to `-p <package>`, never the whole workspace — a
// per-edit hook has to stay fast, and the workspace build is minutes on a cold
// cache. Cross-crate breakage still surfaces at pre-commit/CI. A cold-cache
// clippy run can still exceed the timeout below; that case is reported and
// skipped rather than wedging the agent.
// Clippy flags mirror the real gate (`cargo clippy-check`, xtask lint):
// --all-targets --all-features -- -D warnings. Without `-D warnings` clippy
// exits 0 on the whole warning class this workspace configures, so the hook
// would pass code that then fails at pre-commit.
// Exits 2 (with stderr fed back to Claude) on rustfmt parse errors or clippy
// failures so the agent fixes them immediately; exits 0 otherwise, including
// when a toolchain binary is missing — a broken guard must never block edits.
import { execFileSync } from 'node:child_process';
import path from 'node:path';

// Path prefix -> cargo package name. Anything unlisted is a no-op.
const PACKAGES = [
  ['crates/collab-core/', 'collab-core'],
  ['crates/collab-relay/', 'collab-relay'],
  ['crates/collab-cli/', 'collab-cli'],
  ['crates/collab-proto/', 'collab-proto'],
  ['crates/collab-wasm/', 'collab-wasm'],
  ['crates/collab-watcher/', 'collab-watcher'],
  ['xtask/', 'xtask'],
  ['tests/e2e-tests/', 'e2e-tests'],
];

let raw = '';
process.stdin.on('data', (c) => (raw += c));
process.stdin.on('end', () => {
  let fp = '';
  try {
    fp = JSON.parse(raw)?.tool_input?.file_path ?? '';
  } catch {
    process.exit(0);
  }
  if (!fp || !fp.endsWith('.rs')) process.exit(0);
  // Derive the root from the edited file, not CLAUDE_PROJECT_DIR: the latter
  // stays pinned at the main checkout while a session works in a worktree, so
  // clippy would run against code that was never edited.
  let root;
  try {
    root = execFileSync('git', ['-C', path.dirname(fp), 'rev-parse', '--show-toplevel'], { stdio: 'pipe' })
      .toString()
      .trim();
  } catch {
    root = process.env.CLAUDE_PROJECT_DIR || process.cwd();
  }
  const rel = path.relative(root, fp).split(path.sep).join('/');
  if (rel.startsWith('..')) process.exit(0);
  const hit = PACKAGES.find(([prefix]) => rel.startsWith(prefix));
  if (!hit) process.exit(0);
  const pkg = hit[1];
  try {
    execFileSync('rustfmt', ['--edition', '2021', fp], { stdio: 'pipe' });
  } catch (e) {
    if (e.code === 'ENOENT') {
      process.stderr.write('cargo-check-changed: rustfmt not found on PATH — skipped.\n');
      process.exit(0);
    }
    // rustfmt otherwise only fails on a parse error — the file does not compile yet.
    const out = (e.stdout?.toString() || '') + (e.stderr?.toString() || '');
    process.stderr.write(`rustfmt could not parse ${rel}:\n${out}\n`);
    process.exit(2);
  }
  try {
    const args = ['clippy', '-p', pkg, '--all-targets', '--all-features', '--message-format', 'short'];
    execFileSync('cargo', [...args, '--', '-D', 'warnings'], { cwd: root, stdio: 'pipe', timeout: 180000 });
    process.exit(0);
  } catch (e) {
    if (e.code === 'ENOENT') {
      process.stderr.write('cargo-check-changed: cargo not found on PATH — skipped.\n');
      process.exit(0);
    }
    // Only the timeout above is benign. Any other signal death (OOM SIGKILL, a
    // stray `pkill cargo`) falls through — `e.killed` is undefined even on a
    // real timeout, so ETIMEDOUT is the only reliable discriminator.
    if (e.code === 'ETIMEDOUT') {
      process.stderr.write(`cargo-check-changed: clippy timed out for ${pkg} — skipped, not a lint error.\n`);
      process.exit(0);
    }
    const out = (e.stdout?.toString() || '') + (e.stderr?.toString() || '');
    process.stderr.write(`clippy failed for ${pkg} (${rel}):\n${out}\n`);
    process.exit(2);
  }
});
