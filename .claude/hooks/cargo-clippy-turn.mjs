#!/usr/bin/env node
// Stop hook: run workspace clippy ONCE per turn instead of once per .rs edit.
// The per-edit version cost ~6.3s on every Rust edit and could blow its timeout
// on a cold cache; worse, the per-edit budget forced `-p <package>` scoping,
// which structurally cannot see cross-crate breakage. Here `--workspace` does.
// Exit contract (Stop hook): exit 0 ends the turn and BOTH streams go only to
// the debug log — Claude never sees them, so there is no point writing a message
// before an exit 0. Exit 2 prevents the turn from ending and feeds stderr to
// Claude. Any other non-zero is a non-blocking error and the turn ends anyway,
// so this hook only ever exits 0 or 2.
import { execFileSync } from 'node:child_process';

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
  // of a previous stop hook. The agent got one clear shot at the lint output;
  // nagging again risks burning turns on a lint it cannot or will not fix, and
  // pre-commit + CI remain the real gate. Block once, then get out of the way.
  if (payload?.stop_hook_active === true) process.exit(0);

  const root = payload?.cwd || process.env.CLAUDE_PROJECT_DIR || process.cwd();
  const git = (args) => execFileSync('git', ['-C', root, ...args], { stdio: 'pipe' }).toString();
  try {
    git(['rev-parse', '--show-toplevel']);
  } catch {
    process.exit(0);
  }

  // Fast bail when no Rust changed — this is what keeps the hook free on
  // non-Rust turns. Tracked changes vs HEAD (staged + unstaged) plus untracked
  // new files.
  let changed;
  try {
    changed = [
      ...git(['diff', '--name-only', '--diff-filter=ACM', 'HEAD']).split('\n'),
      ...git(['ls-files', '--others', '--exclude-standard']).split('\n'),
    ].filter((p) => p.endsWith('.rs'));
  } catch {
    process.exit(0);
  }
  if (changed.length === 0) process.exit(0);

  try {
    // `-D warnings` and `--all-features` mirror the real gate (xtask/src/main.rs:132
    // and the `clippy-check` alias in .cargo/config.toml:5) — clippy exits 0 on
    // warnings otherwise, which is the whole reason those flags are needed.
    // 10min timeout, not the per-edit 3min: this runs once per turn, so a
    // cold-cache full-workspace build must be able to finish.
    const args = ['clippy', '--workspace', '--all-targets', '--all-features', '--message-format', 'short'];
    execFileSync('cargo', [...args, '--', '-D', 'warnings'], { cwd: root, stdio: 'pipe', timeout: 600000 });
    process.exit(0);
  } catch (e) {
    // No cargo, or a timeout: silently defer to pre-commit/CI rather than block
    // a turn on an infrastructure hiccup. `e.killed` is undefined even on a real
    // timeout, so ETIMEDOUT is the only reliable discriminator.
    if (e.code === 'ENOENT' || e.code === 'ETIMEDOUT') process.exit(0);
    // Truncated so a wall of errors does not flood the context; `cargo lint`
    // gives the full list.
    const out = ((e.stdout?.toString() || '') + (e.stderr?.toString() || '')).split('\n').slice(0, 100).join('\n');
    process.stderr.write(
      `Workspace clippy failed — fix these before finishing (truncated; run \`cargo lint\` for the full list):\n${out}\n`,
    );
    process.exit(2);
  }
});
