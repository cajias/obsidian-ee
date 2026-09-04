#!/usr/bin/env node
// PostToolUse hook: re-run tests/features/design-integrity-guard.sh after an
// edit to any file that guard constrains. It checks three invariants no linter
// can catch: the 8-scenario byte-frozen join key between 03-implementation-plan
// and 04-bdd-test-plan, each tests/features/*.feature staying a verbatim copy
// of its block in 04, and the residual ledger holding exactly 6 rows.
//
// The guard already runs in integration.yml, but only at push — so a reworded
// scenario name surfaced one or more sessions after the edit that caused it,
// with the context that produced it gone. The script costs about a second.
// No logic is duplicated here; this is a path filter around the CI script, so
// the two can never disagree about what the invariants are.
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
  const root = process.env.CLAUDE_PROJECT_DIR || process.cwd();
  const rel = path.relative(root, fp).split(path.sep).join('/');
  if (!GUARDED.test(rel)) process.exit(0);
  try {
    execFileSync('bash', ['tests/features/design-integrity-guard.sh'], {
      cwd: root,
      stdio: 'pipe',
    });
    process.exit(0);
  } catch (e) {
    const out = (e.stdout?.toString() || '') + (e.stderr?.toString() || '');
    process.stderr.write(`design-integrity-guard failed after editing ${rel}:\n${out}\n`);
    process.exit(2);
  }
});
