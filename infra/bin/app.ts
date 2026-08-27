#!/usr/bin/env node
import * as cdk from 'aws-cdk-lib';
import { SharedStack } from '../lib/shared-stack';
import { RelayStack } from '../lib/relay-stack';

// --- Bootstrap constants -----------------------------------------------------
// Everything below is set once by the maintainer during the bootstrap runbook
// in docs/aws-deployment-plan.md. They are literals, not context lookups, so
// `cdk synth` and `cdk list` succeed with no AWS credentials (the M2 gate and
// CI both run credential-free).
//
// They are EXPORTED because test/helpers.ts imports them. A test suite that
// re-declares its own copy pins its goldens against itself: `repo:${REPO_SLUG}:*`
// on both sides of every trust assertion means REPO_SLUG -> '*' here stays green
// while every OIDC role becomes assumable from any GitHub repository. One
// definition, imported by the tests, is what makes those goldens bind to what
// actually ships.

/** Bootstrap runbook item 3: the account and region hosting all three environments. */
export const ACCOUNT = '111111111111'; // PLACEHOLDER — replace at bootstrap
export const REGION = 'us-east-1'; // PLACEHOLDER — replace at bootstrap

/** Bootstrap runbook item 3: the apex domain whose `collab.` subdomain is delegated. */
export const DOMAIN = 'example.com'; // PLACEHOLDER — replace at bootstrap

/**
 * Bootstrap runbook item 3: the default VPC and one of its public subnets. Referenced by
 * id rather than `Vpc.fromLookup`, which would make a live AWS call at synth
 * time. `aws ec2 describe-vpcs --filters Name=isDefault,Values=true` and
 * `aws ec2 describe-subnets --filters Name=vpc-id,Values=<vpc>` supply these.
 */
export const VPC_ID = 'vpc-00000000000000000'; // PLACEHOLDER — replace at bootstrap
export const SUBNET_ID = 'subnet-00000000000000000'; // PLACEHOLDER — replace at bootstrap
export const AVAILABILITY_ZONE = `${REGION}a`;

/** The OIDC trust policies are written against this repository. */
export const REPO_SLUG = 'cajias/obsidian-ee';

export const ENVIRONMENTS = ['dev', 'staging', 'prod'] as const;
export type EnvName = (typeof ENVIRONMENTS)[number];
// -----------------------------------------------------------------------------

/**
 * THE composition — the ONE place in this repository that names the stack ids
 * and passes the per-stack props.
 *
 * It is exported because test/helpers.ts calls it. A test harness that
 * re-implements the wiring pins its goldens against a second hand-written
 * argument list, and the shipped one then drifts unwatched: `repoSlug: '*'`
 * here, or `envName: 'dev'` for all three stacks, would leave every assertion
 * in the suite green while shipping `repo:*:*` OIDC trust on the prod deploy
 * role and prod's instance reading dev's token. Importing the CONSTANTS is not
 * enough — the composition that consumes them has to be the same code.
 *
 * The `App` is constructed by the CALLER, not here: under the CDK CLI it must
 * be a bare `new cdk.App()` so the CLI's own cdk.json context injection
 * applies, while the in-process test harness passes that context explicitly.
 * `app.synth()` likewise stays with the caller — `Template.fromStack` synthes
 * on its own and must not write a cdk.out.
 */
export function composeApp(app: cdk.App): {
  shared: SharedStack;
  relays: Record<EnvName, RelayStack>;
} {
  const env: cdk.Environment = { account: ACCOUNT, region: REGION };

  const shared = new SharedStack(app, 'RelayShared', {
    env,
    domain: DOMAIN,
    repoSlug: REPO_SLUG,
    envNames: ENVIRONMENTS,
    description: 'Shared registry, OIDC provider, delegated zone, and CI roles',
  });

  const relays = {} as Record<EnvName, RelayStack>;
  for (const envName of ENVIRONMENTS) {
    relays[envName] = new RelayStack(app, `Relay-${envName}`, {
      env,
      envName,
      domain: DOMAIN,
      zone: shared.zone,
      ecrRepo: shared.ecrRepo,
      vpcId: VPC_ID,
      subnetId: SUBNET_ID,
      availabilityZone: AVAILABILITY_ZONE,
      description: `Relay ${envName}: one t4g.micro instance behind Caddy`,
    });
  }

  return { shared, relays };
}

/**
 * The CDK CLI entry point: nothing but App construction, `composeApp`, synth.
 * Guarded on `require.main` so importing this module (test/helpers.ts, for the
 * constants and `composeApp` above) does not construct a second App or write a
 * cdk.out as a side effect. Under `npx cdk list` / `cdk synth` the CLI runs
 * this file through ts-node's `Module.runMain`, so the guard is true and
 * behavior is unchanged.
 */
function main(): void {
  const app = new cdk.App();
  composeApp(app);
  app.synth();
}

if (require.main === module) main();
