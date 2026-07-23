#!/usr/bin/env node
// PostToolUse hook: lint a single just-edited plugin .ts file with eslint.
// Exits 2 (with stderr fed back to Claude) on lint errors so the agent fixes
// them immediately; exits 0 otherwise. Silent/no-op for non-plugin-TS files.
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
  if (!fp) process.exit(0);
  const root = process.env.CLAUDE_PROJECT_DIR || process.cwd();
  const pluginSrc = path.join('plugins', 'obsidian-ee', 'src') + path.sep;
  const rel = path.relative(root, fp);
  if (!rel.startsWith(pluginSrc) || !fp.endsWith('.ts')) process.exit(0);
  const cwd = path.join(root, 'plugins', 'obsidian-ee');
  try {
    execFileSync('npx', ['eslint', path.relative(cwd, fp)], { cwd, stdio: 'pipe' });
    process.exit(0);
  } catch (e) {
    const out = (e.stdout?.toString() || '') + (e.stderr?.toString() || '');
    process.stderr.write(`eslint found issues in ${rel}:\n${out}\n`);
    process.exit(2);
  }
});
