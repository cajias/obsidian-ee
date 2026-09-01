#!/usr/bin/env node
// PostToolUse hook: flag newly-added `pub` items in workspace-internal crates
// that nothing outside the owning crate's `src/` references. CLAUDE.md: keep
// internal APIs `pub(crate)` so rustc's `dead_code` lint can flag them — `pub`
// items in a workspace-internal crate are never reported as dead, so neither
// rustc nor CI catches this.
// This is a cross-crate-reference test, not a ban on `pub`: a `pub` item that
// another crate (or the owning crate's integration tests, which can only reach
// `pub` surface) actually uses passes.
// Exits 2 (with stderr fed back to Claude) when unreferenced new public surface
// appears; exits 0 otherwise, including on any unexpected error — a broken
// guard must never block edits.
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import path from 'node:path';

// `pub` + optional qualifiers (`async`, `unsafe`, `extern "C"`) + item keyword.
const DECL =
  /^\+\s*pub\s+(?:(?:async|unsafe|extern(?:\s+"[^"]*")?)\s+)*(fn|struct|enum|trait|const|static|type|mod|use)\s+(\S.*)/;
// Plain directory pathspecs: a wildcard like `crates/*/src` is wildmatched
// against the full path and would match nothing inside those directories.
const SEARCH = ['crates', 'xtask', 'tests'];

// `pub use a::b::Name;` / `... as Alias;` -> the bound name. Glob and grouped
// re-exports bind no single name, so they are skipped rather than guessed at.
const declName = (kw, rest) => {
  if (kw !== 'use') return /^[A-Za-z_][A-Za-z0-9_]*/.exec(rest)?.[0];
  const p = rest.replace(/;.*$/, '').trim();
  if (p.includes('*') || p.includes('{')) return undefined;
  const seg = p.split(/\s+/).pop().split('::').pop();
  return /^[A-Za-z_][A-Za-z0-9_]*$/.test(seg) ? seg : undefined;
};

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
  // stays pinned at the main checkout while a session works in a worktree.
  let root;
  try {
    root = execFileSync('git', ['-C', path.dirname(fp), 'rev-parse', '--show-toplevel'], { stdio: 'pipe' })
      .toString()
      .trim();
  } catch {
    root = process.env.CLAUDE_PROJECT_DIR || process.cwd();
  }
  const rel = path.relative(root, fp).split(path.sep).join('/');
  if (!rel.startsWith('crates/')) process.exit(0);
  // collab-wasm's `#[wasm_bindgen] pub` surface is consumed by TypeScript, not
  // by Rust, so a cross-crate Rust grep would always false-positive there.
  if (rel.startsWith('crates/collab-wasm/')) process.exit(0);
  // Test code is exempt only at whole-file granularity (a `tests` path
  // segment). A `pub` item inside an inline `#[cfg(test)] mod tests` does get
  // flagged; locating one needs byte-offset math that is wrong more often than
  // the false positive it saves.
  if (rel.split('/').includes('tests')) process.exit(0);
  try {
    let tracked = true;
    try {
      execFileSync('git', ['ls-files', '--error-unmatch', '--', fp], { cwd: root, stdio: 'pipe' });
    } catch {
      tracked = false;
    }
    // A tracked file with an empty diff has no added lines. Only an untracked
    // file is wholly new — treating "unchanged" as "new" flags the world.
    const lines = tracked
      ? execFileSync('git', ['diff', '-U0', '--', fp], { cwd: root, stdio: 'pipe' })
          .toString()
          .split('\n')
          .filter((l) => l.startsWith('+') && !l.startsWith('+++'))
      : readFileSync(fp, 'utf8')
          .split('\n')
          .map((l) => `+${l}`);
    const names = [];
    for (const line of lines) {
      const m = DECL.exec(line);
      const name = m && declName(m[1], m[2]);
      if (name && !names.includes(name)) names.push(name);
    }
    if (names.length === 0) process.exit(0);
    const ownSrc = `${rel.split('/').slice(0, 2).join('/')}/src/`;
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
      return hits.split('\n').filter((f) => f && !f.startsWith(ownSrc)).length === 0;
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
