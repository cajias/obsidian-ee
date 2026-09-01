#!/usr/bin/env node
// TaskCompleted hook: enforce "proof on disk or it isn't done" — refuse to close
// a task that CLAIMS to change files while nothing landed on disk.
// FAILS OPEN BY DESIGN. Most tasks legitimately produce zero repo changes
// (reviews, audits, "shepherd PR #79 to green", "check CI status"), and a
// previous fail-closed hook in this repo blocked nearly every edit. So three
// gates must ALL agree before this blocks, and every ambiguous case resolves to
// allow — when in any doubt, exit 0.
// SCOPE: this catches "the agent did nothing". It does NOT catch "the agent
// claimed a false verification" — that is not mechanically detectable and is
// deliberately not attempted here.
// Exit contract (TaskCompleted): exit 2 leaves the task NOT completed and feeds
// stderr back to Claude. Exit 0 completes it and BOTH streams go only to the
// debug log, so writing a message before an exit 0 is dead output. Any other
// non-zero is a non-blocking error, so this hook only ever exits 0 or 2.
import { execFileSync } from 'node:child_process';

// Verbs that promise a file change, anchored to a line start: task subjects are
// imperative, so the first word is the action. Stem + suffix group keeps it
// tight — "add" matches "adds/added/adding" but not "address".
const CLAIMS_EDIT =
  /^\s*(?:add|writ|creat|implement|fix|port|renam|mov|delet|remov|refactor|updat|register|wir|extract)(?:e|es|ed|ing|s|ten)?\b/im;
// Overrides the verb gate. Prefixes so "analyzed"/"summary" hit, but only at a
// whitespace boundary so a flag or tool name ("cargo-check") is not a match.
// A task titled "review and fix X" is genuinely ambiguous, and for a BLOCKING
// gate ambiguity must resolve to allow.
const NON_EDIT =
  /(?:^|\s)(?:review|audit|investigat|shepherd|watch|check|verif|report|analyz|research|triage|poll|monitor|merge|plan|recommend|summar)/i;

let raw = '';
process.stdin.on('data', (c) => (raw += c));
process.stdin.on('end', () => {
  try {
    let payload;
    try {
      payload = JSON.parse(raw);
    } catch {
      process.exit(0);
    }
    const subject = payload?.task_subject || '';
    const text = `${subject}\n${payload?.task_description || ''}`;
    if (!CLAIMS_EDIT.test(text)) process.exit(0);
    if (NON_EDIT.test(text)) process.exit(0);

    const root = payload?.cwd || process.env.CLAUDE_PROJECT_DIR || process.cwd();
    const git = (args) =>
      execFileSync('git', ['-C', root, ...args], { stdio: 'pipe' }).toString().trim();
    try {
      git(['rev-parse', '--show-toplevel']);
    } catch {
      process.exit(0);
    }
    // Any modified or untracked file is evidence of work.
    if (git(['status', '--porcelain'])) process.exit(0);
    // So is a commit not yet on the base branch. Skipped, not failed, when
    // origin/main does not resolve (fresh repo, different remote layout).
    try {
      if (git(['log', '--oneline', 'origin/main..HEAD'])) process.exit(0);
    } catch {
      /* no origin/main — this probe simply does not apply */
    }

    process.stderr.write(
      `Task "${subject}" claims to change files, but it closed with no commits, no modified files and no untracked files.\n` +
        `Project rule: proof on disk or it isn't done — an idle notification is not proof of work.\n` +
        `Either produce the artifact (then confirm with \`git status --short\` / \`git diff --stat\`), or re-word the task if it was not a code task.\n`,
    );
    process.exit(2);
  } catch {
    // A broken gate must never wedge task tracking.
    process.exit(0);
  }
});
