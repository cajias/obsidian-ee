/*
 * security-audit — reusable, READ-ONLY security audit for the obsidian-ee workspace.
 *
 * WHAT IT IS
 *   A three-phase Claude Code Workflow that makes the ad-hoc security review durable:
 *     1. Find   — fan out per-subsystem security finders (DIMENSIONS + false-positive EXCLUSIONS,
 *                 the security-review reviewer's rules, reused verbatim).
 *     2. Verify — adversarially confirm each finding (VERDICT_SCHEMA, confidence>=8 gate).
 *     3. Report — return the confirmed findings as structured data.
 *
 * READ-ONLY: this workflow ONLY finds, verifies, and reports. It does NOT edit code
 *   (workflow subagents cannot persist edits anyway). The durable artifact is the
 *   `required_regression_test` attached to every confirmed finding.
 *
 * DURABLE DIRECTIVE (per CLAUDE.md "Trust-boundary & crypto invariants"):
 *   Every CONFIRMED trust-boundary / crypto finding MUST leave behind a RED-first,
 *   negative-path regression test — a test that asserts the attacker case is REJECTED,
 *   is RED before the fix and GREEN after. A fix without such a test is NOT done.
 *   The verify agent (which already read the cited code) articulates that exact test in
 *   `required_regression_test`; the human/orchestrator adds it RED before fixing.
 *
 * HOW TO RUN
 *   Workflow({ name: 'security-audit' })
 *
 * RETURNS
 *   { confirmed: [ { title, file, line, severity, category, description, exploit_scenario,
 *                    recommendation, verify_reasoning, verify_confidence,
 *                    required_regression_test } ],
 *     raw_count }
 */

export const meta = {
  name: 'security-audit',
  description: 'READ-ONLY three-phase security audit of the obsidian-ee workspace: fan out finders per subsystem, adversarially verify each finding against the security-review false-positive rules, and report only confirmed high-confidence findings — each carrying the RED-first negative-path regression test that must be added before any fix.',
  whenToUse: 'Run a full security audit of the obsidian-ee repo (crypto, relay auth, protocol deserialization, WASM bindings, CLI connection, filesystem watcher, TS plugin, infra/CI). Analysis only — it reports vulnerabilities and the exact regression test each fix requires; it does not edit code.',
  phases: [
    { title: 'Find', detail: 'security finders per subsystem' },
    { title: 'Verify', detail: 'adversarially confirm each finding, drop false positives' },
    { title: 'Report', detail: 'return confirmed findings, each with its required RED-first regression test' },
  ],
}

// Finder/verify subagents inherit the session's working directory (the repo
// root), so a relative reference is correct and portable across machines and
// checkouts — no hardcoded absolute path. Pass an absolute path as the Workflow
// `args` string to point the audit at a repo elsewhere.
const REPO = (typeof args === 'string' && args.trim()) || 'the current repository (your working directory)'

const FINDINGS_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['findings'],
  properties: {
    findings: {
      type: 'array',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['title', 'file', 'line', 'severity', 'category', 'description', 'exploit_scenario', 'recommendation', 'confidence'],
        properties: {
          title: { type: 'string' },
          file: { type: 'string', description: 'repo-relative path' },
          line: { type: 'integer' },
          severity: { type: 'string', enum: ['HIGH', 'MEDIUM', 'LOW'] },
          category: { type: 'string' },
          description: { type: 'string' },
          exploit_scenario: { type: 'string' },
          recommendation: { type: 'string' },
          confidence: { type: 'integer', description: '1-10' },
        },
      },
    },
  },
}

const VERDICT_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['is_real', 'confidence', 'reasoning', 'adjusted_severity', 'required_regression_test'],
  properties: {
    is_real: { type: 'boolean' },
    confidence: { type: 'integer', description: '1-10 after adversarial scrutiny' },
    reasoning: { type: 'string' },
    adjusted_severity: { type: 'string', enum: ['HIGH', 'MEDIUM', 'LOW', 'NONE'] },
    required_regression_test: {
      type: 'string',
      description: 'When is_real is true: the exact RED-first, negative-path regression test the human/orchestrator MUST add before fixing — name the test file/module, the crafted attacker input to feed, and the concrete assertion that the attacker case is REJECTED (error/None/no-write), such that it FAILS (RED) against current code and PASSES (GREEN) only after the fix. When is_real is false: empty string.',
    },
  },
}

const EXCLUSIONS = `
HARD EXCLUSIONS (do NOT report these — the official reviewer discards them):
1. Denial of Service / resource exhaustion / memory / CPU / file-descriptor / panic-only availability issues.
2. Secrets stored on disk if otherwise secured.
3. Rate limiting / service overload.
4. Lack of input validation on non-security-critical fields without proven security impact.
5. Lack of hardening / best-practice-only observations. Only concrete vulnerabilities.
6. Theoretical race conditions / timing attacks (only concrete ones).
7. Vulnerabilities from outdated third-party libraries (managed separately).
8. Memory safety (buffer overflow, UAF) in Rust or other memory-safe languages — IMPOSSIBLE, never report.
9. Findings only in unit/integration test files or test-only code.
10. Log spoofing / logging non-secret, non-PII data (logging URLs is safe).
11. SSRF that only controls the path (must control host or protocol).
12. Regex injection / regex DoS.
13. Findings in documentation / markdown files.
14. Lack of audit logs.
15. Client-side JS/TS lacking auth/permission checks — the backend is responsible; do NOT flag missing server-side-style checks in client code.
16. XSS in React/Angular unless dangerouslySetInnerHTML / bypassSecurityTrustHtml / innerHTML-style sinks are used.
PRECEDENTS: env vars & CLI flags are TRUSTED (attacker cannot set them). UUIDs are unguessable. Logging plaintext high-value secrets IS a vuln; logging URLs is safe. Only obvious, concrete MEDIUMs count.
FOCUS: concrete, exploitable vulnerabilities with a clear attack path where UNTRUSTED input (network messages from relay/peers, malicious document content, hostile file paths, crafted ciphertext) reaches a sensitive sink.
`

const DIMENSIONS = [
  { key: 'core-crypto', prompt: `In ${REPO}, review crate crates/collab-core — focus on the MLS/encryption and key-management code and the CRDT apply path. Hunt concrete crypto vulnerabilities: hardcoded/placeholder/all-zero keys used for real encryption, weak or absent nonce/IV handling, nonce reuse, missing AEAD authentication, key material logged or exposed, decryption that trusts attacker-controlled length/type fields, and any path where crafted ciphertext or a crafted CRDT update from an untrusted peer reaches an unsafe sink. Read the actual .rs files. Note: a prior fix (commit ac3d3c1) made the plugin fail closed on a placeholder key — check the Rust side has no equivalent gap.` },
  { key: 'relay-auth', prompt: `In ${REPO}, review crate crates/collab-relay (WebSocket relay). Focus on: the optional bearer-token authentication (bypass, constant-time compare, empty/placeholder token accepted), authorization of which clients can subscribe/publish to which documents/topics, whether the "zero-knowledge" claim holds (does the relay ever see or log plaintext/keys), the offline message queue routing (can a client receive another group's messages), and any place a crafted network message controls a sensitive operation. Read the actual .rs files.` },
  { key: 'proto-deser', prompt: `In ${REPO}, review crate crates/collab-proto (protocol message types). Focus on deserialization of untrusted network bytes: unchecked length/index/type fields, integer casts that let a malicious peer drive an out-of-bounds or logic bypass downstream, missing validation at the trust boundary, and any decode path that can be steered to a dangerous action. Read the actual .rs files.` },
  { key: 'wasm-bindings', prompt: `In ${REPO}, review crate crates/collab-wasm (WASM bindings for the browser/Obsidian client). Focus on: the public insert/delete/apply API bounds (a known issue: delete has no bounds check and an over-range delete traps unreachable and poisons the core), any binding that exposes key material or plaintext to JS in an unsafe way, and data-exposure across the WASM boundary. Distinguish a genuine security vuln from a pure panic/DoS (panic-only availability is EXCLUDED). Read the actual .rs files.` },
  { key: 'cli-conn', prompt: `In ${REPO}, review crate crates/collab-cli. Focus on: how it handles server-supplied data, TLS/cert validation on the WS connection (accepting invalid certs), bearer-token/secret handling, and any place untrusted server data reaches a sensitive sink. The reconnect lifecycle itself is a robustness concern (excluded unless it has a security impact). Read the actual .rs files.` },
  { key: 'watcher-fs', prompt: `In ${REPO}, review crate crates/collab-watcher (filesystem watcher / local document sync). Focus on PATH TRAVERSAL and unsafe file operations: does a document name/id or relay-supplied path get joined into a filesystem path without sanitization (../ escape, absolute-path override, symlink following) allowing read/write outside the vault? Any place network-sourced or document-sourced strings become file paths. Read the actual .rs files.` },
  { key: 'plugin-ts', prompt: `In ${REPO}, review the TypeScript Obsidian plugin under plugins/obsidian-ee/ (src/*.ts). Focus on CONCRETE issues: encryption key derivation/storage (all-zeros or placeholder key used to encrypt, weak KDF, key logged), config validation (a prior fix added an all-zeros key guard in validateConfig — verify it's real and complete), DOM-based XSS via innerHTML/insertAdjacentHTML/outerHTML/document.write where relay- or peer-supplied document content is injected into the DOM, and unsafe handling of messages received from the relay. Remember: missing client-side auth checks are NOT vulnerabilities; only flag real injection/crypto sinks. Read the actual .ts files.` },
  { key: 'infra', prompt: `In ${REPO}, review infrastructure & tooling: docker/ (Dockerfiles, docker-compose), scripts/*.sh, .github/workflows/*.yml, xtask/, and Cargo.toml/deny.toml. Focus on CONCRETE, triggerable issues only: hardcoded secrets/credentials/tokens committed in files, command injection in shell scripts or CI workflows where UNTRUSTED input (PR title/body, issue text, branch name, artifact contents) reaches a shell, and pwn-request / untrusted-checkout patterns in GitHub Actions with a specific attack path. Per the rules, most shell-script and CI findings are NOT exploitable — only report with a concrete untrusted-input path.` },
]

phase('Find')
phase('Verify') // Find -> Verify run per-item in the pipeline below (no barrier): each finder's output is adversarially verified as it arrives.

const results = await pipeline(
  DIMENSIONS,
  (d) => agent(
    `You are a senior security engineer doing a focused review. ${d.prompt}\n\n${EXCLUSIONS}\n\nReport ONLY findings you are >=80% confident are real, concrete, exploitable vulnerabilities newly relevant to this codebase. It is far better to return an empty findings array than to report noise. For each finding give the exact repo-relative file path and line, a concrete exploit scenario, and a minimal root-cause fix recommendation.`,
    { label: `find:${d.key}`, phase: 'Find', schema: FINDINGS_SCHEMA, effort: 'high' },
  ),
  (found, d) => {
    const list = (found && found.findings) || []
    if (list.length === 0) return []
    return parallel(list.map((f) => () =>
      agent(
        `You are an adversarial security reviewer whose job is to REFUTE weak findings. A finder in subsystem "${d.key}" reported this potential vulnerability in ${REPO}:\n\n` +
        `Title: ${f.title}\nFile: ${f.file}:${f.line}\nSeverity: ${f.severity}\nCategory: ${f.category}\nDescription: ${f.description}\nExploit scenario: ${f.exploit_scenario}\n\n` +
        `Independently READ the cited file and surrounding code. Determine whether this is a REAL, concrete, exploitable vulnerability with untrusted input reaching a sensitive sink — or a false positive.\n\n${EXCLUSIONS}\n\n` +
        `Apply the exclusions strictly: if the finding falls under any HARD EXCLUSION (especially: panic-only/DoS, memory-safety-in-Rust, test-only code, docs, missing client-side auth, trusted env/CLI input), set is_real=false. Default to is_real=false when uncertain. Only is_real=true with confidence>=8 if you can articulate a concrete attack path a security team would act on.\n\n` +
        `If (and only if) is_real=true, fill required_regression_test: describe the EXACT RED-first, negative-path regression test the human/orchestrator MUST add before fixing. Per CLAUDE.md "Trust-boundary & crypto invariants", this test asserts the attacker case is REJECTED and must be RED (fail) against the current code and GREEN (pass) only after the fix. Name the concrete test file/module for this crate, the crafted attacker input to feed (malicious ciphertext / crafted network message / traversal path / all-zeros key / etc.), and the exact assertion (returns Err / None / rejects / does not write outside the vault). If is_real=false, set required_regression_test to an empty string.`,
        { label: `verify:${d.key}:${(f.file || '').split('/').pop()}`, phase: 'Verify', schema: VERDICT_SCHEMA, effort: 'high' },
      ).then((v) => ({ ...f, verdict: v }))
    ))
  },
)

phase('Report')

const confirmed = results
  .flat()
  .filter(Boolean)
  .filter((f) => f.verdict && f.verdict.is_real && f.verdict.confidence >= 8)
  .map((f) => ({
    title: f.title,
    file: f.file,
    line: f.line,
    severity: f.verdict.adjusted_severity !== 'NONE' ? f.verdict.adjusted_severity : f.severity,
    category: f.category,
    description: f.description,
    exploit_scenario: f.exploit_scenario,
    recommendation: f.recommendation,
    verify_reasoning: f.verdict.reasoning,
    verify_confidence: f.verdict.confidence,
    required_regression_test: f.verdict.required_regression_test,
  }))

const allRaw = results.flat().filter(Boolean)
log(`Finders raw: ${allRaw.length} candidate findings; confirmed after adversarial verify: ${confirmed.length}. Each confirmed finding requires a RED-first negative-path regression test before it is fixed.`)

return { confirmed, raw_count: allRaw.length }
