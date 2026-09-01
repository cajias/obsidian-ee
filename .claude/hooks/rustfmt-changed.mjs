#!/usr/bin/env node
// PostToolUse hook: rustfmt a just-edited .rs file. rustfmt rewrites the file in
// place — that is intended, it is the fast single-file equivalent of `cargo fmt`.
// Clippy used to run here too; it moved to the `Stop` hook
// (cargo-clippy-turn.mjs). Two reasons: ~6.3s on EVERY .rs edit is a tax the
// agent pays dozens of times per turn, and a per-edit budget forced `-p
// <package>` scoping that structurally cannot see cross-crate breakage. At Stop
// it runs once per turn over `--workspace`, which is both cheaper and stricter.
// Exits 2 (with stderr fed back to Claude) on a rustfmt parse error so the agent
// fixes it immediately; exits 0 otherwise, including when rustfmt is missing —
// a broken guard must never block edits.
import { execFileSync } from 'node:child_process';
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
  if (!fp || !fp.endsWith('.rs')) process.exit(0);
  // Derive the root from the edited file, not CLAUDE_PROJECT_DIR: the latter
  // stays pinned at the main checkout while a session works in a worktree, so
  // the reported path would point at a file that was never edited.
  let root;
  try {
    root = execFileSync('git', ['-C', path.dirname(fp), 'rev-parse', '--show-toplevel'], { stdio: 'pipe' })
      .toString()
      .trim();
  } catch {
    root = process.env.CLAUDE_PROJECT_DIR || process.cwd();
  }
  const rel = path.relative(root, fp).split(path.sep).join('/');
  // A file outside the root relativizes to a ../../.. chain — report it absolute.
  const label = rel.startsWith('..') ? fp : rel;
  try {
    execFileSync('rustfmt', ['--edition', '2021', fp], { stdio: 'pipe' });
    process.exit(0);
  } catch (e) {
    if (e.code === 'ENOENT') process.exit(0);
    // rustfmt otherwise only fails on a parse error — the file does not compile yet.
    const out = (e.stdout?.toString() || '') + (e.stderr?.toString() || '');
    process.stderr.write(`rustfmt could not parse ${label}:\n${out}\n`);
    process.exit(2);
  }
});
