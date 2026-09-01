#!/usr/bin/env node
// PostToolUse hook: flag newly-added `pub` items in workspace-internal crates
// that no other crate references. CLAUDE.md: keep internal APIs `pub(crate)` so
// rustc's `dead_code` lint can flag them — `pub` items in a workspace-internal
// crate are never reported as dead, so neither rustc nor CI catches this.
// This is a cross-crate-reference test, not a ban on `pub`: a `pub` item that
// something else actually uses passes.
// Exits 2 (with stderr fed back to Claude) when unreferenced new public surface
// appears; exits 0 otherwise, including on any unexpected error — a broken
// guard must never block edits.
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import path from 'node:path';

const DECL = /^\+\s*pub\s+(fn|struct|enum|trait|const|static|type|mod)\s+([A-Za-z_][A-Za-z0-9_]*)/;
const SEARCH = ['crates/*/src', 'crates/*/tests', 'xtask/src', 'tests/e2e-tests'];

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
  const root = process.env.CLAUDE_PROJECT_DIR || process.cwd();
  const rel = path.relative(root, fp).split(path.sep).join('/');
  if (!rel.startsWith('crates/')) process.exit(0);
  // collab-wasm's `#[wasm_bindgen] pub` surface is consumed by TypeScript, not
  // by Rust, so a cross-crate Rust grep would always false-positive there.
  if (rel.startsWith('crates/collab-wasm/')) process.exit(0);
  // Test code is exempt. Heuristic, deliberately cheap: skip whole files under
  // a `tests` segment, and skip declarations sitting after the file's first
  // `#[cfg(test)]` attribute (i.e. inside the trailing test module).
  if (rel.split('/').includes('tests')) process.exit(0);
  try {
    const text = readFileSync(fp, 'utf8');
    const cfgTest = text.indexOf('#[cfg(test)]');
    let diff = '';
    try {
      diff = execFileSync('git', ['diff', '-U0', '--', fp], { cwd: root, stdio: 'pipe' }).toString();
    } catch {
      diff = '';
    }
    // No diff means the file is untracked (or unchanged) — treat all of it as new.
    const lines = diff
      ? diff.split('\n').filter((l) => l.startsWith('+') && !l.startsWith('+++'))
      : text.split('\n').map((l) => `+${l}`);
    const names = [];
    for (const line of lines) {
      const m = DECL.exec(line);
      if (!m) continue;
      const at = text.indexOf(m[0].slice(1).trim());
      if (cfgTest >= 0 && at > cfgTest) continue;
      if (!names.includes(m[2])) names.push(m[2]);
    }
    const orphans = names.filter((name) => {
      let hits = '';
      try {
        hits = execFileSync('git', ['grep', '-w', '-l', '--untracked', '-e', name, '--', ...SEARCH], {
          cwd: root,
          stdio: 'pipe',
        }).toString();
      } catch {
        return true; // git grep exits 1 with no matches
      }
      return hits.split('\n').filter((f) => f && f !== rel).length === 0;
    });
    if (orphans.length === 0) process.exit(0);
    process.stderr.write(
      `rust-pub-guard: new \`pub\` items in ${rel} are referenced by no other crate: ${orphans.join(', ')}.\n` +
        'CLAUDE.md: keep internal-crate APIs `pub(crate)` (not `pub`) so rustc\'s `dead_code` lint flags unused ' +
        'items. Narrow each to `pub(crate)`, or delete it if it is speculative public surface "for later".\n'
    );
    process.exit(2);
  } catch {
    process.exit(0);
  }
});
