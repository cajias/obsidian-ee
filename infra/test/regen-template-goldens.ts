/**
 * Writes test/template-goldens.ts and test/asset-goldens.ts from the CURRENT
 * synth and the CURRENT assets/ bytes.
 *
 * Both goldens are generated data, never hand-edited: this script is the only
 * thing that may author those files, so a golden can never drift into a
 * hand-tuned approximation of what it is supposed to pin. It runs the SHIPPED
 * composition (`buildApp` -> `composeApp` in bin/app.ts) and encodes exactly
 * what the tests compare against — `Template.fromStack(stack).toJSON()` and the
 * verbatim bytes `readAsset` returns.
 *
 * Run it after any deliberate infrastructure or asset change, review the
 * resulting diff, and commit it with the change:
 *
 *   cd infra && npm run regen-goldens
 */
import * as fs from 'fs';
import * as path from 'path';
import { Template } from 'aws-cdk-lib/assertions';
import { buildApp, ENV_NAMES, readAsset } from './helpers';

const { shared, relays } = buildApp();

const stacks: Record<string, Template> = {
  RelayShared: Template.fromStack(shared),
};
for (const envName of ENV_NAMES) {
  stacks[`Relay-${envName}`] = Template.fromStack(relays[envName]);
}

const header = `/**
 * EXHAUSTIVE BOUND — the complete synthesized CloudFormation template of every
 * stack, verbatim, as a golden literal. GENERATED DATA: never hand-write or
 * reformat it.
 *
 * Nine rounds of review each bounded one more resource by name — IAM surfaces,
 * trust documents, instance Properties, tags, the ECR repo, the resource-type
 * census, rendered user-data, asset contents — and each round found one more
 * corner no probe had reached: the launch template's LaunchTemplateData (whose
 * KeyName and MetadataOptions the instance inherits, so an aspect could re-open
 * key-pair SSH and raise the IMDS hop limit at full green), the policy ->
 * principal REVERSE edge (attachToRole of a foreign, name-only role hands out
 * ssm:GetParameter on the relay auth token while synthesizing no new resource,
 * so even the census stays unchanged), and every role's own Properties (a
 * silent maxSessionDuration). Naming the next corner only moves the hole to the
 * corner after it.
 *
 * So this pins the WHOLE template per stack — every resource, every property,
 * every parameter, output and rule — which fails on any added, removed,
 * reordered or edited value, named by a test or not. It is deliberately
 * brittle, and it is the catch-all backstop BENEATH the targeted assertions,
 * not a replacement for them: those still give the precise, readable failure
 * message that says which invariant broke.
 *
 * A deliberate infrastructure change must regenerate this file consciously —
 * the resulting diff IS the review artifact.
 *
 * Regenerate with:
 *   cd infra && npm run regen-goldens
 */
export const TEMPLATE_GOLDENS: Record<string, unknown> = `;

const body = JSON.stringify(
  Object.fromEntries(Object.entries(stacks).map(([name, t]) => [name, t.toJSON()])),
  null,
  2,
);

fs.writeFileSync(
  path.join(__dirname, 'template-goldens.ts'),
  `${header}${body};\n`,
  'utf8',
);

// --- test/asset-goldens.ts ---------------------------------------------------

/** export name -> assets/ filename. */
const ASSET_GOLDENS: Record<string, string> = {
  DOCKER_COMPOSE_GOLDEN: 'docker-compose.yml',
  CADDYFILE_GOLDEN: 'Caddyfile.tpl',
  DEPLOY_SH_GOLDEN: 'deploy.sh.tpl',
};

const assetHeader = `/**
 * EXHAUSTIVE BOUND — the complete content of each infra/assets/ file, verbatim,
 * as a golden literal. GENERATED DATA: never hand-write or reformat it.
 *
 * These three files (docker-compose.yml, Caddyfile.tpl, deploy.sh.tpl) were the
 * last partially-asserted surface in the infra suite: only their
 * security-critical lines were checked (the \`:?\` fail-closed guards, the
 * no-scheme site address, the no-\`||\`-fallback token fetch). A probe like that
 * bounds only the substring it names — swap \`caddy:2\` for \`caddy:latest\`, add a
 * stray \`echo\`, drop a healthcheck, reorder a volume — and every targeted
 * assertion stays green because none of them was ever asked about that line.
 *
 * So this pins the WHOLE file, per asset, which fails on any added, removed,
 * reordered or edited byte. It is deliberately brittle. A deliberate change to
 * any of the three files must regenerate this one consciously — that diff IS
 * the review artifact.
 *
 * Each literal is the file split on '\\n'; the trailing empty array element
 * reproduces the file's trailing newline through \`.join('\\n')\`.
 *
 * Regenerate with:
 *   cd infra && npm run regen-goldens
 */
`;

const assetBody = Object.entries(ASSET_GOLDENS)
  .map(([exportName, fileName]) => {
    const lines = readAsset(fileName)
      .split('\n')
      .map((line) => `  ${JSON.stringify(line)},`)
      .join('\n');
    return `\nexport const ${exportName}: string = [\n${lines}\n].join("\\n");\n`;
  })
  .join('');

fs.writeFileSync(path.join(__dirname, 'asset-goldens.ts'), `${assetHeader}${assetBody}`, 'utf8');
