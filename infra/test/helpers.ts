import * as fs from 'fs';
import * as path from 'path';
import * as cdk from 'aws-cdk-lib';
import { Template } from 'aws-cdk-lib/assertions';
import { ACCOUNT, composeApp, DOMAIN, ENVIRONMENTS, REGION, REPO_SLUG } from '../bin/app';

// CALLS bin/app.ts's composition — it does not reproduce it. `buildApp` below
// invokes the exported `composeApp`, the single function that names the stack
// ids and passes the per-stack props, so in-process assertions exercise
// literally the same wiring `cdk synth` runs.
//
// Re-implementing the argument list here would be a tautology even with the
// constants imported: the goldens would pin a second hand-written composition
// while the SHIPPED one drifts. `repoSlug: '*'` or `envName: 'dev'` in
// bin/app.ts must be able to turn this suite red, and it can only do that if
// this suite runs bin/app.ts's own code.
//
// Nothing below may re-declare a value bin/app.ts declares. Re-export instead,
// so a test that needs one is reading the shipped literal.

export { DOMAIN, REPO_SLUG };
export const ENV: cdk.Environment = { account: ACCOUNT, region: REGION };
export const ENV_NAMES = ENVIRONMENTS;

// The `cdk` CLI reads cdk.json's `context` and injects it into the app
// subprocess; a directly-constructed `App()` (this in-process test harness)
// does not pick it up on its own. Load it explicitly so feature flags (e.g.
// the IMDSv2 unique-launch-template-name flag) apply identically here and
// under `cdk synth` — otherwise the two can silently diverge.
const CDK_JSON_CONTEXT = JSON.parse(fs.readFileSync(path.join(__dirname, '..', 'cdk.json'), 'utf8')).context;

/**
 * IAM evaluates Action and Resource as case-insensitive GLOB patterns, not as
 * literals: `ssm:Get*` and `ssm:*` both grant `ssm:GetParameter`, and a
 * Resource of `*` matches every instance ARN. A test that scans a synthesized
 * policy with `startsWith`/equality therefore reads a WIDENED grant as an
 * absent one and stays green while the grant is unbounded. Every reach scan in
 * this suite must go through here.
 */
export function iamMatches(pattern: string, value: string): boolean {
  const escaped = pattern.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const regex = escaped.replace(/\\\*/g, '.*').replace(/\\\?/g, '.');
  return new RegExp(`^${regex}$`, 'i').test(value);
}

/**
 * Enumerating action names is the wrong shape for an IAM guard: every review
 * round finds another action (`ssm:GetParametersByPath`, `ssm:StartSession`,
 * `ssm:GetParameterHistory`, ...) or another attachment path
 * (`new ManagedPolicy({ roles: [role] })`, which leaves the role's own
 * `ManagedPolicyArns` null) that the enumeration skips. The only bound that
 * cannot be escaped is an exhaustive one, so this collects the COMPLETE
 * effective permission surface of one role — every Allow/Deny statement that
 * reaches it from EVERY attachment path CloudFormation offers — and callers
 * deep-equal the result against a golden literal.
 *
 * Collection paths:
 *   1. the role's own inline `Policies` property,
 *   2. `AWS::IAM::Policy` resources whose `Roles` list references the role,
 *   3. `AWS::IAM::RolePolicy` (L1 CfnRolePolicy) resources naming the role,
 *   4. `AWS::IAM::ManagedPolicy` resources whose `Roles` list references the role,
 *   5. the role's own `ManagedPolicyArns` — a `Ref` to an in-template
 *      ManagedPolicy contributes that policy's statements; anything else (an
 *      AWS-managed ARN such as AmazonSSMManagedInstanceCore, a Fn::Join, a Ref
 *      to something that is not a ManagedPolicy) is opaque to the template and
 *      is therefore reported as UNBOUNDED, which no golden literal can match.
 *
 * An Allow built on `NotAction`/`NotResource` grants everything it does NOT
 * list, so it too collapses to UNBOUNDED rather than being normalized.
 */
export interface UnboundedStatement {
  readonly UNBOUNDED: string;
}

export type EffectiveStatement = UnboundedStatement | Record<string, unknown>;

/** IAM renders a single-element list as a scalar, so every scan must accept both. */
export function toArray(value: unknown): unknown[] {
  return Array.isArray(value) ? value : value === undefined ? [] : [value];
}

/** The resources-lookup + type check both role walkers below open with. */
function getRole(template: Template, roleLogicalId: string) {
  const resources = (template.toJSON().Resources ?? {}) as Record<string, any>;
  const role = resources[roleLogicalId];
  if (role?.Type !== 'AWS::IAM::Role') {
    throw new Error(`${roleLogicalId} is not an AWS::IAM::Role in this template`);
  }
  return { resources, role };
}

/** Deterministic ordering so a golden literal is independent of synth order. */
function sortByJson<T>(values: readonly T[]): T[] {
  return [...values].sort((a, b) => {
    const left = JSON.stringify(a);
    const right = JSON.stringify(b);
    return left < right ? -1 : left > right ? 1 : 0;
  });
}

function normalizeStatement(statement: Record<string, any>): EffectiveStatement {
  if (statement?.Effect === 'Allow' && statement.NotAction !== undefined) {
    return { UNBOUNDED: 'Allow with NotAction grants every action it does not list' };
  }
  if (statement?.Effect === 'Allow' && statement.NotResource !== undefined) {
    return { UNBOUNDED: 'Allow with NotResource grants every resource it does not list' };
  }
  // Spread first so an unexpected key (Sid, Principal, ...) survives into the
  // comparison and breaks the golden rather than being silently dropped.
  const normalized: Record<string, unknown> = { ...statement };
  for (const key of ['Action', 'Resource', 'NotAction', 'NotResource'] as const) {
    if (statement[key] === undefined) delete normalized[key];
    else normalized[key] = sortByJson(toArray(statement[key]));
  }
  return normalized;
}

/**
 * The identity-policy sibling of `effectivePolicyStatements`: that one bounds
 * WHAT a role may do, this one bounds WHO may become it.
 *
 * `AssumeRolePolicyDocument` statements are OR'd, and every field of one is
 * load-bearing — `Principal` above all. A `Match.objectLike`/`arrayWith` probe
 * proves only that an acceptable statement EXISTS: swap the OIDC provider for
 * `oidc-provider/evil.example.com` with the conditions untouched, or append a
 * second statement with `Principal: "*"`, and the probe still passes while the
 * role now trusts a different issuer or the whole internet.
 *
 * So this returns the role's COMPLETE normalized trust statements — Principal
 * included, unknown keys (`NotPrincipal`, `Sid`, ...) preserved by the same
 * spread `normalizeStatement` uses — for callers to deep-equal against a golden
 * literal. Statement count, Principal, Action, Effect and Condition are then all
 * bounded at once.
 */
export function trustDocumentStatements(
  template: Template,
  roleLogicalId: string,
): EffectiveStatement[] {
  const { role } = getRole(template, roleLogicalId);
  const statements = toArray(role.Properties?.AssumeRolePolicyDocument?.Statement);
  return sortByJson(statements.map((s) => normalizeStatement(s as Record<string, any>)));
}

export function effectivePolicyStatements(
  template: Template,
  roleLogicalId: string,
): EffectiveStatement[] {
  const { resources, role } = getRole(template, roleLogicalId);
  const roleName =
    typeof role.Properties?.RoleName === 'string' ? (role.Properties.RoleName as string) : undefined;

  // A policy may name its role by `Ref`, by `Fn::GetAtt`, or (CfnRolePolicy)
  // by the literal role name.
  const referencesRole = (value: unknown): boolean => {
    if (typeof value === 'string') return roleName !== undefined && value === roleName;
    if (Array.isArray(value)) return value.some(referencesRole);
    if (value !== null && typeof value === 'object') {
      const object = value as Record<string, unknown>;
      if (object.Ref === roleLogicalId) return true;
      const getAtt = object['Fn::GetAtt'];
      if (Array.isArray(getAtt) && getAtt[0] === roleLogicalId) return true;
      return Object.values(object).some(referencesRole);
    }
    return false;
  };

  const raw: Record<string, any>[] = toArray(role.Properties?.Policies).flatMap((policy) =>
    toArray((policy as Record<string, any>)?.PolicyDocument?.Statement),
  ) as Record<string, any>[];
  const unbounded: EffectiveStatement[] = [];
  const managedPoliciesAlreadyCollected = new Set<string>();

  for (const [logicalId, resource] of Object.entries(resources)) {
    const type = resource?.Type;
    if (
      type !== 'AWS::IAM::Policy' &&
      type !== 'AWS::IAM::RolePolicy' &&
      type !== 'AWS::IAM::ManagedPolicy'
    )
      continue;
    if (!referencesRole(resource.Properties?.Roles ?? resource.Properties?.RoleName)) continue;
    if (type === 'AWS::IAM::ManagedPolicy') managedPoliciesAlreadyCollected.add(logicalId);
    raw.push(...(toArray(resource.Properties?.PolicyDocument?.Statement) as Record<string, any>[]));
  }

  for (const arn of toArray(role.Properties?.ManagedPolicyArns)) {
    const ref = (arn as Record<string, unknown> | null)?.Ref;
    const target = typeof ref === 'string' ? resources[ref] : undefined;
    if (target?.Type !== 'AWS::IAM::ManagedPolicy') {
      unbounded.push({
        UNBOUNDED: `ManagedPolicyArns entry ${JSON.stringify(arn)} is opaque: its statements are not in this template`,
      });
      continue;
    }
    if (managedPoliciesAlreadyCollected.has(ref as string)) continue;
    managedPoliciesAlreadyCollected.add(ref as string);
    raw.push(...(toArray(target.Properties?.PolicyDocument?.Statement) as Record<string, any>[]));
  }

  return sortByJson([...raw.map(normalizeStatement), ...unbounded]);
}

/**
 * EXHAUSTIVE BOUND helper. Every deep-equal in this suite is keyed to a
 * resource TYPE some test names — the ECR repository's Properties, a role's
 * policies, the instance's tags — so a resource of a type NO test names is
 * invisible to all of them at once. `AWS::ECR::RegistryPolicy` granting a
 * foreign account push/pull is the worked example: it is a separate resource
 * from `AWS::ECR::Repository`, and a resource-based policy needs no IAM role,
 * so neither the registry bound nor any role bound ever sees it.
 *
 * Naming that one type would only move the hole to the next type, so this pins
 * the whole census instead: type -> count over every resource in the stack.
 * An added resource of ANY type — a registry policy, a role, a Lambda, a bucket
 * — then fails by construction, named or not.
 */
export function resourceTypeCensus(template: Template): Record<string, number> {
  const census: Record<string, number> = {};
  for (const resource of Object.values(
    (template.toJSON().Resources ?? {}) as Record<string, any>,
  )) {
    const type = String(resource?.Type);
    census[type] = (census[type] ?? 0) + 1;
  }
  return census;
}

/**
 * Reads an `assets/` file from disk so an assertion binds to the bytes that
 * actually ship (lib/relay-stack.ts inlines these three files into user-data
 * verbatim), not to a copy restated in a test.
 */
export function readAsset(name: string): string {
  return fs.readFileSync(path.join(__dirname, '..', 'assets', name), 'utf8');
}

/**
 * The App is built here rather than inside `composeApp` for the one reason
 * `composeApp` documents: under the CDK CLI the App must be bare so the CLI
 * injects cdk.json's context itself, whereas in process nothing does, so the
 * context is passed explicitly (above) to keep the two synths identical.
 * Everything after that — every stack id, every prop — comes from bin/app.ts.
 */
export function buildApp() {
  const app = new cdk.App({ context: CDK_JSON_CONTEXT });
  const { shared, relays } = composeApp(app);
  return { app, shared, relays };
}

/**
 * REVERSE EDGE of `effectivePolicyStatements`, which walks role -> policy and
 * so never sees who ELSE a policy is attached to.
 *
 * `attachToRole` on an imported, name-only principal —
 * `(role.node.findChild('DefaultPolicy') as iam.Policy).attachToRole(
 *   iam.Role.fromRoleName(this, 'Foreign', 'attacker-controlled-role'))` —
 * appends that bare name to an existing Policy's `Roles` list. It synthesizes
 * NO resource, so `resourceTypeCensus` is unchanged; it touches no role this
 * suite bounds, so every effective-surface and trust golden is unchanged. Yet
 * the relay instance role's `ssm:GetParameter` on /relay/<env>/auth-token — and
 * in RelayShared the deploy roles' tag-scoped SSM `SendCommand` root shell —
 * now reaches an arbitrary principal.
 *
 * So walk the edge the other way. Every attachment target of every policy in
 * the template must be a `Ref` to an in-template `AWS::IAM::Role`; the census
 * already bounds how many roles exist and the role goldens bound each one, so
 * "Ref to an in-template role" is by construction "a principal this suite
 * already bounds". Anything else is returned as an offender: a literal
 * role-name string (the imported-principal case), an `Fn::GetAtt`, a `Ref` to a
 * non-role, an empty attachment list, or any `Users`/`Groups` entry — the same
 * escape through a different attachment field.
 */
export function foreignPolicyAttachments(template: Template): string[] {
  const resources = (template.toJSON().Resources ?? {}) as Record<string, any>;
  const offenders: string[] = [];

  for (const [logicalId, resource] of Object.entries(resources)) {
    const type = resource?.Type;
    if (type !== 'AWS::IAM::Policy' && type !== 'AWS::IAM::ManagedPolicy') continue;

    for (const field of ['Users', 'Groups'] as const) {
      for (const entry of toArray(resource.Properties?.[field])) {
        offenders.push(
          `${type} ${logicalId} attaches to ${field} entry ${JSON.stringify(entry)}; policies in this app attach to roles only`,
        );
      }
    }

    const roles = toArray(resource.Properties?.Roles);
    if (roles.length === 0) {
      offenders.push(`${type} ${logicalId} has an empty Roles list`);
    }
    for (const entry of roles) {
      const ref = (entry as Record<string, unknown> | null)?.Ref;
      if (typeof ref !== 'string' || resources[ref]?.Type !== 'AWS::IAM::Role') {
        offenders.push(
          `${type} ${logicalId} attaches to ${JSON.stringify(entry)}, which is not a Ref to an in-template AWS::IAM::Role this suite bounds`,
        );
      }
    }
  }

  return offenders;
}
