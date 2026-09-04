#!/usr/bin/env node
// PostToolUse hook: refuse two ways of silently weakening .claude/ config that
// a pre-PR review caught only by hand.
//
// 1. `permissions.allow` entries that auto-approve more than they read as.
//    `Bash(rtk git *)` shipped here looking like a read-only convenience. A
//    trailing `*` matches options too, and `rtk git` takes `-c <override>`
//    BEFORE its subcommand, so `rtk git -c core.fsmonitor=/tmp/x.sh status`
//    executed an arbitrary script with no prompt (verified). `Bash(rtk git
//    branch *)` then replaced it inside a set labelled "read-only", though
//    that subcommand's delete/rename flags mutate refs.
//    Rule: a rule ending in `*` is auto-approvable only if its FIXED prefix
//    names a subcommand on READ_ONLY. That admits `rtk git status *` and
//    refuses both `rtk git *` (no subcommand pins the parser, so an option can
//    be smuggled in) and `rtk git branch *` (a subcommand that writes).
//
// 2. Hooks that derive the project root from CLAUDE_PROJECT_DIR alone. It
//    stays pinned at the main checkout while a session works in a worktree, so
//    a path filter built on it never matches and the hook silently no-ops on
//    exactly the edits it exists to catch. design-guard.mjs shipped with this
//    bug. The siblings all derive from `git rev-parse --show-toplevel` first.
//
// Exits 2 (stderr fed back to Claude) on a violation; 0 otherwise.
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import path from 'node:path';

// Subcommands safe to auto-approve behind a wildcard. Deliberately short: this
// is an allowlist, so an unknown subcommand fails closed and needs a prompt.
const READ_ONLY = new Set([
  'read', 'cat', 'head', 'tail', 'status', 'log', 'diff', 'show', 'ls', 'list',
  'view', 'describe', 'version', 'help',
]);

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
  // Same git-toplevel derivation this hook enforces on its siblings.
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

  const problems =
    /^\.claude\/settings(\.local)?\.json$/.test(rel)
      ? checkPermissions(fp)
      : /^\.claude\/hooks\/[^/]+\.mjs$/.test(rel)
        ? checkHookRoot(fp)
        : [];

  if (problems.length === 0) process.exit(0);
  process.stderr.write(`${rel}:\n${problems.map((p) => `  - ${p}`).join('\n')}\n`);
  process.exit(2);
});

/** Flag allow-rules whose wildcard is not pinned by a read-only subcommand. */
function checkPermissions(fp) {
  let allow;
  try {
    allow = JSON.parse(readFileSync(fp, 'utf8'))?.permissions?.allow ?? [];
  } catch {
    return []; // Malformed JSON is the editor's problem, not this guard's.
  }
  const problems = [];
  for (const rule of allow) {
    const inner = /^Bash\((.*)\)$/.exec(rule)?.[1];
    if (!inner || !inner.trimEnd().endsWith('*')) continue;
    const fixed = inner.split(/\s+/).filter((t) => !t.includes('*'));
    if (fixed.some((t) => READ_ONLY.has(t))) continue;
    problems.push(
      `${rule} auto-approves a wildcard with no read-only subcommand pinning it. ` +
        `A trailing * matches options, so flags like \`-c core.fsmonitor=<script>\` ` +
        `(git) or \`--output=<file>\` can ride through unprompted. Name a subcommand ` +
        `from: ${[...READ_ONLY].join(', ')}.`,
    );
  }
  return problems;
}

/** Flag hooks that trust CLAUDE_PROJECT_DIR without a git-toplevel derivation. */
function checkHookRoot(fp) {
  let src;
  try {
    src = readFileSync(fp, 'utf8');
  } catch {
    return [];
  }
  if (!src.includes('CLAUDE_PROJECT_DIR') || src.includes('--show-toplevel')) return [];
  return [
    'derives the project root from CLAUDE_PROJECT_DIR without a ' +
      '`git rev-parse --show-toplevel` fallback. That variable stays pinned to the ' +
      'main checkout during a worktree session, so every path relativizes to a ../.. ' +
      'chain and the hook silently no-ops. Copy the derivation from rustfmt-changed.mjs.',
  ];
}
