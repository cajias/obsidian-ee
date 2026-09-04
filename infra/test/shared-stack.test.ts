import assert from 'node:assert/strict';
import { test } from 'node:test';
import * as cdk from 'aws-cdk-lib';
import { Template } from 'aws-cdk-lib/assertions';
import {
  buildApp,
  ENV,
  ENV_NAMES,
  REPO_SLUG,
  foreignPolicyAttachments,
  iamMatches,
  effectivePolicyStatements,
  resourceTypeCensus,
  toArray,
  trustDocumentStatements,
} from './helpers';
import { TEMPLATE_GOLDENS } from './template-goldens';

const { app, shared, relays } = buildApp();
const template = Template.fromStack(shared);

/** The four principals RelayShared is allowed to create — see the pin below. */
const EXPECTED_ROLE_NAMES = [
  'obsidian-ee-ecr-push',
  ...ENV_NAMES.map((envName) => `obsidian-ee-deploy-${envName}`),
];

function roleLogicalId(roleName: string): string {
  const found = Object.entries(template.findResources('AWS::IAM::Role')).find(
    ([, role]) => role.Properties?.RoleName === roleName,
  );
  assert.ok(found, `expected a role named ${roleName}`);
  return found![0];
}

// Every OIDC trust golden below pins `repo:${REPO_SLUG}:...` — expected and
// actual both built from the same constant. That bounds the SHAPE of the trust
// document but says nothing about the VALUE, so `REPO_SLUG = '*'` in
// bin/app.ts moves both sides together: `repo:*:*` on the push role and
// `repo:*:environment:<env>` on all three deploy roles, every assertion still
// green, every role assumable from any GitHub repository on earth.
//
// helpers.ts now imports REPO_SLUG from bin/app.ts rather than re-declaring it,
// so this reads the shipped literal. Pin the value, and pin the shape too —
// the shape check survives a deliberate re-slug at bootstrap and still refuses
// a wildcard.
//
// Any deliberate repository change must update this literal consciously.
test('the shipped repository slug is exactly one owner/repo, never a wildcard', () => {
  // Widened to `string` deliberately. `assert.equal` carries an assertion
  // signature, so comparing the imported literal type directly would narrow it
  // to `never` and turn a changed slug into a ts-node COMPILE error — the
  // assertions below would never run, and the failure would name the file
  // instead of this invariant.
  const shipped: string = REPO_SLUG;
  assert.match(
    shipped,
    /^[A-Za-z0-9._-]+\/[A-Za-z0-9._-]+$/,
    'REPO_SLUG must be a single owner/repo pair — the OIDC `sub` claim is glob-matched, so any metacharacter widens who may assume the CI roles',
  );
  assert.ok(!shipped.includes('*'), 'REPO_SLUG must never contain a wildcard');
  assert.equal(shipped, 'cajias/obsidian-ee');
});

// 04-bdd-test-plan.md Integration tier, "Synth snapshot" row (Tool: CDK
// synth) — also proven at the CLI level by tests/features/run-m2.sh's
// `cdk list` check; asserted here too so a stack rename fails fast in-process.
test('stack set is RelayShared plus the three Relay environment stacks', () => {
  assert.equal(shared.stackName, 'RelayShared');
  for (const envName of ENV_NAMES) {
    assert.equal(relays[envName].stackName, `Relay-${envName}`);
  }
  const stackIds = app.node.children.filter((c) => cdk.Stack.isStack(c)).map((c) => c.node.id);
  assert.deepEqual(
    stackIds.sort(),
    ['RelayShared', 'Relay-dev', 'Relay-staging', 'Relay-prod'].sort(),
  );
});

// SC3: tag immutability.
test('registry repository declares immutable tags and scan on push', () => {
  template.hasResourceProperties('AWS::ECR::Repository', {
    ImageTagMutability: 'IMMUTABLE',
    ImageScanningConfiguration: { ScanOnPush: true },
  });
});

// T3 / SC4: every CI credential is OIDC-issued; no long-lived standing keys.
test('CI credentials are OIDC-issued only: no IAM users or access keys', () => {
  template.hasResourceProperties('AWS::IAM::OIDCProvider', {
    Url: 'https://token.actions.githubusercontent.com',
    ClientIdList: ['sts.amazonaws.com'],
  });
  template.resourceCountIs('AWS::IAM::User', 0);
  template.resourceCountIs('AWS::IAM::AccessKey', 0);

  // The native L1 replaces CDK's custom-resource L2, whose provider lambda held
  // iam:*OpenIDConnectProvider on "*" and fetched the issuer cert with
  // RejectUnauthorized:false. Neither may come back.
  template.resourceCountIs('Custom::AWSCDKOpenIdConnectProvider', 0);
  template.resourceCountIs('AWS::Lambda::Function', 0);
  assert.deepEqual(
    Object.keys(resourceTypeCensus(template)).filter((type) => type.startsWith('Custom::')),
    [],
    'RelayShared must synthesize no custom resources',
  );
});

// SC4 blast radius. The complete-trust-document golden below bounds WHO may assume each deploy
// role; this one bounds WHAT the assumed role may then do. ssm:SendCommand
// with AWS-RunShellScript is arbitrary root shell on the target instance, so
// the `ssm:resourceTag/RelayEnv` condition is the only thing standing between
// dev's OIDC role and the prod relay. Asserting the conditioned statement
// EXISTS would not catch its deletion (CDK then merges the two SendCommand
// statements into one unconditioned grant), so bound the role's total reach:
// across every policy attached to it, exactly one Allow may reach an EC2
// instance with SendCommand, and its condition must be exactly the tag match.
test('each deploy role may SendCommand only to its own environment instances', () => {
  // A concrete instance ARN to probe reach with: `*`, `arn:aws:ec2:*:*:*` and
  // `...:instance/*` all match it, so a widened Resource cannot slip past the
  // way an exact-string comparison would let it.
  const someInstance = `arn:aws:ec2:${ENV.region}:${ENV.account}:instance/i-0123456789abcdef0`;

  for (const envName of ENV_NAMES) {
    // `effectivePolicyStatements` walks ALL FIVE attachment paths — including
    // `new ManagedPolicy({ roles: [deployRole] })`, which leaves the role's own
    // ManagedPolicyArns null and which a hand-rolled scan here used to miss.
    const statements = effectivePolicyStatements(
      template,
      roleLogicalId(`obsidian-ee-deploy-${envName}`),
    );

    // An Allow on NotAction/NotResource, or an opaque managed-policy ARN whose
    // statements are not in this template, has reach no positive scan can bound.
    assert.deepEqual(
      statements.filter((statement) => 'UNBOUNDED' in statement),
      [],
      `obsidian-ee-deploy-${envName} must carry no unbounded grant (NotAction/NotResource, or a managed policy this template cannot read)`,
    );

    const reaching = statements.filter((statement): statement is Record<string, unknown> => {
      if ('UNBOUNDED' in statement || statement.Effect !== 'Allow') return false;
      // Glob semantics on both halves: `ssm:*` grants SendCommand just as
      // surely as naming it, and `*` names every instance in the account.
      return (
        toArray(statement.Action).some((a) => iamMatches(String(a), 'ssm:SendCommand')) &&
        toArray(statement.Resource).some((r) => typeof r === 'string' && iamMatches(r, someInstance))
      );
    });

    assert.equal(
      reaching.length,
      1,
      `obsidian-ee-deploy-${envName} must have exactly one ssm:SendCommand grant reaching an EC2 instance, found ${reaching.length}`,
    );
    assert.deepEqual(
      reaching[0].Condition,
      { StringEquals: { 'ssm:resourceTag/RelayEnv': envName } },
      `obsidian-ee-deploy-${envName}'s SendCommand grant must be confined to instances tagged RelayEnv=${envName}`,
    );
  }
});

// EXHAUSTIVE BOUND — a golden literal, not a probe.
//
// The SendCommand bound above enumerates one action and one attachment shape,
// which is why every review round found another way past it: ssm:StartSession
// against the same instance is an equivalent root shell (the instance role
// already carries the ssmmessages channel actions), and
// `new ManagedPolicy({ roles: [deployRole] })` attaches a policy while the
// role's own ManagedPolicyArns stays null. `effectivePolicyStatements` reads
// each role's COMPLETE effective surface from all five attachment paths, so
// this deep-equal fails on ANY added statement, ANY extra action and ANY
// widened resource, named or not.
//
// Any deliberate permission change must update this literal consciously —
// that edit is the review checkpoint.
test('each deploy role has exactly the two-statement SSM surface and nothing else', () => {
  for (const envName of ENV_NAMES) {
    assert.deepEqual(
      effectivePolicyStatements(template, roleLogicalId(`obsidian-ee-deploy-${envName}`)),
      [
        // Accepted residual: ssm:GetCommandInvocation declares no resource
        // types in the AWS service authorization reference. Read-only.
        { Action: ['ssm:GetCommandInvocation'], Effect: 'Allow', Resource: ['*'] },
        // The only grant that reaches an instance, and only instances tagged
        // for this environment.
        {
          Action: ['ssm:SendCommand'],
          Condition: { StringEquals: { 'ssm:resourceTag/RelayEnv': envName } },
          Effect: 'Allow',
          Resource: [`arn:aws:ec2:${ENV.region}:${ENV.account}:instance/*`],
        },
        // The document half of the same call: run-shell only.
        {
          Action: ['ssm:SendCommand'],
          Effect: 'Allow',
          Resource: [`arn:aws:ssm:${ENV.region}::document/AWS-RunShellScript`],
        },
      ],
      `obsidian-ee-deploy-${envName}'s effective permission surface must be exactly the SendCommand pair plus GetCommandInvocation`,
    );
  }
});

// EXHAUSTIVE BOUND — same treatment for the push role, whose accepted R5
// ref-wide trust makes bounding WHAT it may do the load-bearing half.
// Any deliberate permission change must update this literal consciously.
test("the ECR push role's complete effective permission surface is registry-only", () => {
  assert.deepEqual(effectivePolicyStatements(template, roleLogicalId('obsidian-ee-ecr-push')), [
    // Push and retag, scoped to the one shared repository.
    {
      Action: [
        'ecr:BatchCheckLayerAvailability',
        'ecr:BatchGetImage',
        'ecr:CompleteLayerUpload',
        'ecr:DescribeImages',
        'ecr:GetDownloadUrlForLayer',
        'ecr:InitiateLayerUpload',
        'ecr:PutImage',
        'ecr:UploadLayerPart',
      ],
      Effect: 'Allow',
      Resource: [{ 'Fn::GetAtt': ['RelayRepo971E060D', 'Arn'] }],
    },
    // ecr:GetAuthorizationToken declares no resource types: "*" is the only
    // expressible resource.
    { Action: ['ecr:GetAuthorizationToken'], Effect: 'Allow', Resource: ['*'] },
  ]);
});

// Both trust guards above are keyed by RoleName, so a role named anything else
// — a wildcard-sub principal with ssm:*/ec2:* on "*", say — would be invisible
// to every one of them. Pin the principal SET so a new one cannot appear
// unnoticed: the count, and the exact names.
test('RelayShared creates exactly the four expected IAM principals', () => {
  template.resourceCountIs('AWS::IAM::Role', EXPECTED_ROLE_NAMES.length);
  const names = Object.values(template.findResources('AWS::IAM::Role')).map(
    (role) => role.Properties?.RoleName,
  );
  assert.deepEqual(
    [...names].sort(),
    [...EXPECTED_ROLE_NAMES].sort(),
    'RelayShared must contain exactly the push role and the three env deploy roles',
  );
});

// EXHAUSTIVE BOUND — a golden literal, not a probe. (Axis A1: Principal.)
//
// A Match.arrayWith/objectLike probe of the sub/aud conditions bounds statement
// count, Effect, Action and Condition but never reads `Principal`, so pointing
// the WebIdentityPrincipal at
// `oidc-provider/evil.example.com` with the conditions untouched keeps it green
// while all three roles trust a different issuer entirely. `sub`/`aud` are
// claims of whatever token the named provider issues — pin the provider or the
// conditions mean nothing.
//
// `trustDocumentStatements` returns the COMPLETE normalized statement list, so
// this deep-equal fails on a swapped Principal, an added statement, a changed
// Action, a widened Condition, or an unexpected key such as NotPrincipal.
//
// Any deliberate trust change must update this literal consciously — that edit
// is the review checkpoint.
test("each deploy role's complete trust document names the GitHub provider and its own environment", () => {
  // The Fn::GetAtt below points at a logical id; pin what that id resolves to,
  // or the same evil-issuer swap just moves into the provider resource.
  const resources = (template.toJSON().Resources ?? {}) as Record<string, any>;
  template.resourceCountIs('AWS::IAM::OIDCProvider', 1);
  assert.equal(resources.GithubOidc?.Type, 'AWS::IAM::OIDCProvider');
  assert.equal(resources.GithubOidc?.Properties?.Url, 'https://token.actions.githubusercontent.com');

  for (const envName of ENV_NAMES) {
    assert.deepEqual(
      trustDocumentStatements(template, roleLogicalId(`obsidian-ee-deploy-${envName}`)),
      [
        {
          Action: ['sts:AssumeRoleWithWebIdentity'],
          Condition: {
            StringEquals: {
              'token.actions.githubusercontent.com:aud': 'sts.amazonaws.com',
              'token.actions.githubusercontent.com:sub': `repo:${REPO_SLUG}:environment:${envName}`,
            },
          },
          Effect: 'Allow',
          Principal: { Federated: { 'Fn::GetAtt': ['GithubOidc', 'Arn'] } },
        },
      ],
      `obsidian-ee-deploy-${envName}'s trust document must be exactly one GithubOidc web-identity statement scoped to the ${envName} GitHub environment`,
    );
  }
});

// EXHAUSTIVE BOUND — a golden literal, not a probe. (Axis A1/A2: the push role.)
//
// A Match.arrayWith/objectLike probe of the documented R5 ref-wide statement
// proves only that it EXISTS: a second statement with `AnyPrincipal()` passes
// alongside it and lets anyone on the internet push the images every relay then
// runs as root. Bounding the whole document is what makes the accepted R5
// residual a residual rather than an open door.
//
// Any deliberate trust change must update this literal consciously.
test("the ECR push role's complete trust document is exactly the one R5 ref-wide statement", () => {
  assert.deepEqual(trustDocumentStatements(template, roleLogicalId('obsidian-ee-ecr-push')), [
    {
      Action: ['sts:AssumeRoleWithWebIdentity'],
      Condition: {
        // R5, deliberate: all refs of the repo, bounded by an immutable
        // registry and by the env-scoped deploy roles one layer down.
        StringEquals: { 'token.actions.githubusercontent.com:aud': 'sts.amazonaws.com' },
        StringLike: { 'token.actions.githubusercontent.com:sub': `repo:${REPO_SLUG}:*` },
      },
      Effect: 'Allow',
      Principal: { Federated: { 'Fn::GetAtt': ['GithubOidc', 'Arn'] } },
    },
  ]);
});

// EXHAUSTIVE BOUND — a golden literal, not a probe. (Axis A5: resource policy.)
//
// The tag-immutability test above uses hasResourceProperties, a PARTIAL match:
// an added `RepositoryPolicyText` granting a foreign account ecr:PutImage /
// ecr:BatchGetImage passes it unseen, and a resource-based policy needs no IAM
// role in this template — so none of the role bounds sees it either. Deep-equal
// the repository's complete Properties instead, which fails on any added key.
//
// Any deliberate registry change must update this literal consciously.
test('the registry repository carries no resource-based policy', () => {
  const repositories = template.findResources('AWS::ECR::Repository');
  assert.equal(Object.keys(repositories).length, 1, 'RelayShared must define exactly one registry');
  assert.deepEqual(Object.values(repositories)[0].Properties, {
    ImageScanningConfiguration: { ScanOnPush: true },
    ImageTagMutability: 'IMMUTABLE',
    LifecyclePolicy: {
      LifecyclePolicyText:
        '{"rules":[{"rulePriority":1,"description":"Retain release images (the rollback target)","selection":{"tagStatus":"tagged","tagPrefixList":["v"],"countType":"imageCountMoreThan","countNumber":100},"action":{"type":"expire"}},{"rulePriority":2,"description":"Expire dev/staging churn","selection":{"tagStatus":"any","countType":"imageCountMoreThan","countNumber":25},"action":{"type":"expire"}}]}',
    },
    RepositoryName: 'obsidian-ee/collab-relay',
  });
});

// EXHAUSTIVE BOUND — a golden literal, not a probe. (Axis A7: the resource set.)
//
// The test above deep-equals the ECR repository's Properties, which closes
// `RepositoryPolicyText` — but `AWS::ECR::RegistryPolicy` is a SEPARATE
// resource type that test never looks at, needs no IAM role, and can grant a
// foreign account ecr:PutImage on the registry every relay pulls its root
// process from. Naming that one type would just move the hole to the next one,
// so pin the whole census: type -> count over every resource in the stack.
//
// Any deliberate resource addition must update this literal consciously —
// that edit is the review checkpoint.
//
// The full template golden below catches it as an opaque diff; this names it.
test('RelayShared: complete resource-type census', () => {
  assert.deepEqual(resourceTypeCensus(template), {
    'AWS::ECR::Repository': 1,
    'AWS::IAM::OIDCProvider': 1,
    'AWS::IAM::Policy': 4,
    'AWS::IAM::Role': 4,
    'AWS::Route53::HostedZone': 1,
  });
});

// EXHAUSTIVE BOUND — the policy -> principal REVERSE edge. (Axis A8.)
//
// See `foreignPolicyAttachments`: every IAM bound in this file walks
// role -> policy, so none of them sees a policy attached to an EXTRA principal.
// `attachToRole(iam.Role.fromRoleName(...))` synthesizes no resource, so the
// census above is unchanged too, while a deploy role's tag-scoped SSM
// SendCommand — a root shell on the matching relay — reaches an arbitrary
// named role. The full template golden below catches it as an opaque diff;
// this names it.
test('RelayShared: every policy attaches only to bounded in-template roles', () => {
  assert.deepEqual(foreignPolicyAttachments(template), []);
});

// EXHAUSTIVE BOUND — the whole template, the backstop beneath every targeted
// assertion above. (Axis A9: everything no axis names.)
//
// The census bounds which resource TYPES exist and the goldens above bound the
// properties each named test asks about — but no role's own `Properties` were
// ever bounded, so a silent `maxSessionDuration` on the deploy roles shipped at
// full green. Naming that property would only move the hole to the property
// after it, so pin everything.
//
// Any deliberate infrastructure change must regenerate test/template-goldens.ts
// consciously — that diff IS the review artifact: `npm run regen-goldens`.
test('RelayShared: complete synthesized template', () => {
  assert.deepStrictEqual(template.toJSON(), TEMPLATE_GOLDENS['RelayShared']);
});
