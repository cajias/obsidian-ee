#!/usr/bin/env node
// PostToolUse hook: re-run xtask's design_integrity test after an edit to any
// file it constrains. It checks four invariants no linter can catch: the
// 8-scenario byte-frozen join key between 03-implementation-plan and
// 04-bdd-test-plan, each scenario declared exactly once in 04, each
// tests/features/*.feature staying a verbatim copy of its block in 04, and the
// residual ledger holding exactly 6 rows.
//
// The test already runs in CI, but only at push — so a reworded scenario name
// surfaced one or more sessions after the edit that caused it, with the context
// that produced it gone.
// No logic is duplicated here; this is a path filter around the same test CI
// runs, so the two can never disagree about what the invariants are. That
// single place is now xtask/tests/design_integrity.rs (it was
// tests/features/design-integrity-guard.sh until the guards were ported to
// Rust; the principle is unchanged, only the location).
//
// Cost: cargo is slower than the ~1s script it replaces on a cold target dir,
// and it can BLOCK on the cargo target-dir lock while another build runs in the
// same tree. That is the accepted price of one source of truth — do not
// "optimize" it by reimplementing the four invariants in JS here.
// Exits 2 (with stderr fed back to Claude) when an invariant breaks; 0 otherwise.
import { execFileSync } from 'node:child_process';
import path from 'node:path';

const GUARDED = /^(docs\/design\/aws-deploy\/0[34]-[a-z0-9-]+\.md|tests\/features\/[^/]+\.feature)$/;

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
  // Derive the root from the edited file, not CLAUDE_PROJECT_DIR: the latter
  // stays pinned at the main checkout while a session works in a worktree, so
  // every path would relativize to a ../../.. chain, GUARDED would never match,
  // and this hook would silently no-op on exactly the edits it exists to catch.
  // Same derivation as rustfmt-changed.mjs and rust-pub-guard.mjs.
  let root;
  try {
    root = execFileSync('git', ['-C', path.dirname(fp), 'rev-parse', '--show-toplevel'], {
      stdio: 'pipe',
    })
      .toString()
      .trim();
  } catch {
    root = process.env.CLAUDE_PROJECT_DIR || process.cwd();
  }
  const rel = path.relative(root, fp).split(path.sep).join('/');
  if (!GUARDED.test(rel)) process.exit(0);
  try {
    execFileSync('cargo', ['test', '--quiet', '-p', 'xtask', '--test', 'design_integrity'], {
      cwd: root,
      stdio: 'pipe',
    });
    process.exit(0);
  } catch (e) {
    const out = (e.stdout?.toString() || '') + (e.stderr?.toString() || '');
    process.stderr.write(`design-integrity check failed after editing ${rel}:\n${out}\n`);
    process.exit(2);
  }
});
