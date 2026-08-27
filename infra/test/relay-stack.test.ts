import assert from 'node:assert/strict';
import { test } from 'node:test';
import { Match, Template } from 'aws-cdk-lib/assertions';
import {
  buildApp,
  ENV_NAMES,
  ENV,
  foreignPolicyAttachments,
  iamMatches,
  effectivePolicyStatements,
  readAsset,
  resourceTypeCensus,
  toArray,
  trustDocumentStatements,
} from './helpers';
import { TEMPLATE_GOLDENS } from './template-goldens';
import { expectedUserData } from './user-data-golden';
import { CADDYFILE_GOLDEN, DEPLOY_SH_GOLDEN, DOCKER_COMPOSE_GOLDEN } from './asset-goldens';

/**
 * Cross-stack `Fn::ImportValue` for SharedStack's ECR repository ARN. The
 * export name is CDK-generated; it appears verbatim in the golden bounds below
 * so a change to the cross-stack wiring has to be acknowledged there.
 */
const ECR_REPO_ARN_IMPORT = {
  'Fn::ImportValue': 'RelayShared:ExportsOutputFnGetAttRelayRepo971E060DArn3C32FA8A',
};

/** The SSM-agent baseline: Run Command + Session Manager channels, no ssm:GetParameter*. */
const SSM_AGENT_BASELINE_ACTIONS = [
  'ec2messages:AcknowledgeMessage',
  'ec2messages:DeleteMessage',
  'ec2messages:FailMessage',
  'ec2messages:GetEndpoint',
  'ec2messages:GetMessages',
  'ec2messages:SendReply',
  'ecr:GetAuthorizationToken',
  'ssm:DescribeAssociation',
  'ssm:DescribeDocument',
  'ssm:GetDeployablePatchSnapshotForInstance',
  'ssm:GetDocument',
  'ssm:GetManifest',
  'ssm:ListAssociations',
  'ssm:ListInstanceAssociations',
  'ssm:PutComplianceItems',
  'ssm:PutInventory',
  'ssm:UpdateInstanceAssociationStatus',
  'ssm:UpdateInstanceInformation',
  'ssmmessages:CreateControlChannel',
  'ssmmessages:CreateDataChannel',
  'ssmmessages:OpenControlChannel',
  'ssmmessages:OpenDataChannel',
];

/**
 * The synthesized IMDSv2 launch template name per environment — the only
 * instance property whose value is not shared by all three stacks (the
 * `@aws-cdk/aws-ec2:uniqueImdsv2TemplateName` feature flag derives it from the
 * stack name plus a construct-path hash). Transcribed from synth; the complete
 * `Properties` bound below reads it.
 */
const LAUNCH_TEMPLATE_NAMES: Record<string, string> = {
  dev: 'RelaydevRelayInstanceLaunchTemplate154AAAB8',
  staging: 'RelaystagingRelayInstanceLaunchTemplate20D419AC',
  prod: 'RelayprodRelayInstanceLaunchTemplateAF023CE9',
};

/** Read once each: every asset below is asserted on by two tests. */
const DOCKER_COMPOSE = readAsset('docker-compose.yml');
const CADDYFILE = readAsset('Caddyfile.tpl');
const DEPLOY_SH = readAsset('deploy.sh.tpl');

const { relays } = buildApp();

for (const envName of ENV_NAMES) {
  const template = Template.fromStack(relays[envName]);
  /** Rendered once per env: both the user-data test and the Properties golden read it. */
  const userDataGolden = expectedUserData(envName);

  // Singleton invariant, 01-logic-design.md.
  test(`Relay-${envName}: exactly one EC2 instance`, () => {
    template.resourceCountIs('AWS::EC2::Instance', 1);
  });

  // G6 / SC4: only the web ports are open — no SSH (22), no UDP (R6: no HTTP/3 yet).
  test(`Relay-${envName}: security group allows only TCP 80 and 443`, () => {
    const groups = template.findResources('AWS::EC2::SecurityGroup');
    const ingress = Object.values(groups).flatMap(
      (sg) => (sg.Properties?.SecurityGroupIngress ?? []) as Array<Record<string, unknown>>,
    );
    assert.ok(ingress.length > 0, 'expected at least one ingress rule');
    for (const rule of ingress) {
      assert.notEqual(rule.FromPort, 22, 'port 22 must never be open (D3: SSM only, no SSH)');
      assert.notEqual(rule.IpProtocol, 'udp', 'no UDP ingress (R6: HTTP/3 stays off in v1)');
    }
    // FromPort alone is not the rule: `Port.tcpRange(443, 65535)` renders
    // FromPort 443 / ToPort 65535 — 65k ports open to 0.0.0.0/0 behind a
    // FromPort that still reads as 443. Assert the whole range of each rule.
    const ports = ingress
      .map((rule) => [rule.FromPort as number, rule.ToPort as number])
      .sort((a, b) => a[0] - b[0]);
    assert.deepEqual(ports, [
      [80, 80],
      [443, 443],
    ]);

    // Standalone ingress resources (e.g. `connections.allowFrom(otherSg, ...)`)
    // wouldn't show up in the inline scan above — prove there are none.
    template.resourceCountIs('AWS::EC2::SecurityGroupIngress', 0);
  });

  // Invariant 4 / SC2: each instance role reads only its own token parameter.
  //
  // Asserting the scoped grant EXISTS is not enough: a managed policy such as
  // AmazonSSMManagedInstanceCore carries ssm:GetParameter* on Resource "*",
  // which unions over the scoped statement while leaving it (and any substring
  // scan of the template) perfectly intact. So this test bounds the role's
  // TOTAL reach instead — the union of every ssm:GetParameter* resource across
  // every attachment path is exactly the one own-environment ARN.
  test(`Relay-${envName}: instance role reads only its own token parameter`, () => {
    const ownArn = `arn:aws:ssm:${ENV.region}:${ENV.account}:parameter/relay/${envName}/auth-token`;

    template.hasResourceProperties('AWS::IAM::Policy', {
      PolicyDocument: Match.objectLike({
        Statement: Match.arrayWith([
          Match.objectLike({ Action: 'ssm:GetParameter', Resource: ownArn }),
        ]),
      }),
    });

    // Union every ssm:GetParameter* Allow reaching this role. Collection goes
    // through `effectivePolicyStatements`, which walks ALL FIVE attachment
    // paths — inline Policies, AWS::IAM::Policy, AWS::IAM::RolePolicy,
    // AWS::IAM::ManagedPolicy and the role's own ManagedPolicyArns — so a
    // managed policy carrying ssm:GetParameter* on "*" cannot hide from it.
    template.resourceCountIs('AWS::IAM::Role', 1);
    const [instanceRoleId] = Object.keys(template.findResources('AWS::IAM::Role'));

    const reach = new Set<string>();
    for (const statement of effectivePolicyStatements(template, instanceRoleId)) {
      // An Allow built on NotAction/NotResource grants everything EXCEPT what
      // it lists, and an opaque managed-policy ARN's statements are not in this
      // template at all. Unbounded by construction — surface it so the
      // deep-equal below rejects it.
      if ('UNBOUNDED' in statement) {
        reach.add(String(statement.UNBOUNDED));
        continue;
      }
      if (statement.Effect !== 'Allow') continue;
      // IAM matches Action as a glob: `ssm:Get*` and `ssm:*` both grant
      // ssm:GetParameter, and `startsWith` would skip both.
      const actions = toArray(statement.Action).map(String);
      if (
        !actions.some(
          (a) => iamMatches(a, 'ssm:GetParameter') || iamMatches(a, 'ssm:GetParameters'),
        )
      )
        continue;
      for (const resource of toArray(statement.Resource)) {
        reach.add(typeof resource === 'string' ? resource : JSON.stringify(resource));
      }
    }
    assert.deepEqual(
      [...reach],
      [ownArn],
      `Relay-${envName}'s total ssm:GetParameter* reach must be exactly its own token ARN`,
    );

    const rendered = JSON.stringify(template.toJSON());
    for (const other of ENV_NAMES.filter((e) => e !== envName)) {
      assert.ok(
        !rendered.includes(`relay/${other}/auth-token`),
        `Relay-${envName} must not reference relay/${other}/auth-token`,
      );
    }
  });

  // EXHAUSTIVE BOUND — a golden literal, not a probe.
  //
  // Every previous version of the guard above enumerated action names
  // ('ssm:GetParameter', 'ssm:GetParameters') and attachment paths, so each
  // review round found another escape: ssm:GetParametersByPath and
  // ssm:GetParameterHistory are the same read from an unnamed family member,
  // ssm:StartSession is an equivalent root shell the SendCommand bound never
  // saw, and `new ManagedPolicy({ roles: [role] })` attaches without ever
  // touching the role's own ManagedPolicyArns. `effectivePolicyStatements`
  // collects the role's COMPLETE effective surface from all five attachment
  // paths (inline Policies, AWS::IAM::Policy, AWS::IAM::RolePolicy,
  // AWS::IAM::ManagedPolicy, ManagedPolicyArns — where an AWS-managed ARN is
  // opaque and therefore reported UNBOUNDED), so this deep-equal fails on ANY
  // added statement, ANY extra action and ANY widened resource, whether or not
  // a test ever names it.
  //
  // Any deliberate permission change must update this literal consciously —
  // that edit is the review checkpoint.
  test(`Relay-${envName}: instance role's complete effective permission surface`, () => {
    // One role in the stack, so there is exactly one principal to bound.
    template.resourceCountIs('AWS::IAM::Role', 1);
    const [roleLogicalId] = Object.keys(template.findResources('AWS::IAM::Role'));

    assert.deepEqual(effectivePolicyStatements(template, roleLogicalId), [
      // SSM-agent baseline. These actions declare no resource types (or would
      // create a role -> instance -> role cycle), so "*" is the only
      // expressible resource; none of them is ssm:GetParameter*.
      { Action: SSM_AGENT_BASELINE_ACTIONS, Effect: 'Allow', Resource: ['*'] },
      // Registry pull, scoped to the one shared repository.
      {
        Action: [
          'ecr:BatchCheckLayerAvailability',
          'ecr:BatchGetImage',
          'ecr:GetDownloadUrlForLayer',
        ],
        Effect: 'Allow',
        Resource: [ECR_REPO_ARN_IMPORT],
      },
      // Invariant 4: exactly one parameter read, and its resource is exactly
      // this environment's own token ARN.
      {
        Action: ['ssm:GetParameter'],
        Effect: 'Allow',
        Resource: [
          `arn:aws:ssm:${ENV.region}:${ENV.account}:parameter/relay/${envName}/auth-token`,
        ],
      },
    ]);
  });

  // IMDSv2 only: v1's unauthenticated metadata GET makes any SSRF in the
  // proxied stack a role-credential leak. AWS::EC2::Instance has no
  // MetadataOptions, so CDK renders this as a launch template — assert both the
  // option AND that the instance actually references a launch template, since
  // an unattached one would satisfy the option check while changing nothing.
  test(`Relay-${envName}: instance metadata requires IMDSv2 tokens`, () => {
    template.hasResourceProperties('AWS::EC2::LaunchTemplate', {
      LaunchTemplateData: Match.objectLike({
        MetadataOptions: Match.objectLike({ HttpTokens: 'required' }),
      }),
    });
    template.hasResourceProperties('AWS::EC2::Instance', {
      LaunchTemplate: Match.anyValue(),
    });
  });

  // The root volume is the only volume: deploy.sh writes the decrypted auth
  // token to /opt/relay/.env on it, so it must be encrypted at rest.
  test(`Relay-${envName}: root volume is encrypted`, () => {
    const instances = Object.values(template.findResources('AWS::EC2::Instance'));
    assert.equal(instances.length, 1);
    const mappings = (instances[0].Properties?.BlockDeviceMappings ?? []) as Array<
      Record<string, any>
    >;
    assert.ok(mappings.length > 0, 'expected an explicit root block device mapping');
    for (const mapping of mappings) {
      assert.equal(
        mapping.Ebs?.Encrypted,
        true,
        `${mapping.DeviceName} must be encrypted (the auth token lands on it)`,
      );
    }
  });

  test(`Relay-${envName}: no standing IAM users or access keys`, () => {
    template.resourceCountIs('AWS::IAM::User', 0);
    template.resourceCountIs('AWS::IAM::AccessKey', 0);
  });

  // EXHAUSTIVE BOUND — a golden literal, not a probe. (Axis A2: trust document.)
  //
  // The surface bound above says WHAT this role may do; nothing said WHO may
  // become it. `assumedBy: new iam.CompositePrincipal(ServicePrincipal('ec2'),
  // ArnPrincipal('arn:aws:iam::999999999999:root'))` leaves every permission
  // assertion in this file untouched while handing the correctly-scoped role to
  // an arbitrary external account. `trustDocumentStatements` returns the
  // COMPLETE normalized statement list, so this fails on a second statement, an
  // added Principal entry, an added Condition, or a NotPrincipal.
  //
  // Any deliberate trust change must update this literal consciously.
  test(`Relay-${envName}: instance role's complete trust document allows only EC2`, () => {
    template.resourceCountIs('AWS::IAM::Role', 1);
    const [roleLogicalId] = Object.keys(template.findResources('AWS::IAM::Role'));
    assert.deepEqual(trustDocumentStatements(template, roleLogicalId), [
      {
        Action: ['sts:AssumeRole'],
        Effect: 'Allow',
        Principal: { Service: 'ec2.amazonaws.com' },
      },
    ]);
  });

  // EXHAUSTIVE BOUND — a golden literal, not a probe. (Axis A3: role → compute.)
  //
  // Bounding RelayInstanceRole bounds nothing on its own: the instance runs
  // under whatever `AWS::IAM::InstanceProfile.Roles` names. Repointing the
  // profile at, say, OrganizationAccountAccessRole orphans the bounded role and
  // boots the relay with admin, with every permission assertion above still
  // green. Assert the whole chain — one role, one profile naming exactly that
  // role, one instance naming exactly that profile — and that the launch
  // template does not smuggle in a second profile of its own.
  //
  // Any deliberate wiring change must update this consciously.
  test(`Relay-${envName}: the instance runs under exactly the bounded instance role`, () => {
    const roleIds = Object.keys(template.findResources('AWS::IAM::Role'));
    assert.equal(roleIds.length, 1, 'exactly one role, the one bounded above');
    const [roleLogicalId] = roleIds;

    const profiles = Object.entries(template.findResources('AWS::IAM::InstanceProfile'));
    assert.equal(profiles.length, 1, `Relay-${envName} must define exactly one instance profile`);
    const [profileLogicalId, profile] = profiles[0];
    assert.deepEqual(
      profile.Properties?.Roles,
      [{ Ref: roleLogicalId }],
      `Relay-${envName}'s instance profile must name exactly the bounded ${roleLogicalId}`,
    );

    const instances = Object.values(template.findResources('AWS::EC2::Instance'));
    assert.equal(instances.length, 1);
    assert.deepEqual(
      instances[0].Properties?.IamInstanceProfile,
      { Ref: profileLogicalId },
      `Relay-${envName}'s instance must run under exactly ${profileLogicalId}`,
    );

    // The IMDSv2 launch template is attached to this instance; a profile set in
    // its LaunchTemplateData would be a second, unbounded credential source.
    for (const [launchTemplateId, launchTemplate] of Object.entries(
      template.findResources('AWS::EC2::LaunchTemplate'),
    )) {
      assert.equal(
        launchTemplate.Properties?.LaunchTemplateData?.IamInstanceProfile,
        undefined,
        `${launchTemplateId} must not name an instance profile of its own`,
      );
    }
  });

  // EXHAUSTIVE BOUND — a golden literal, not a probe. (Axis A4: the resource tag.)
  //
  // The deploy role's `ssm:resourceTag/RelayEnv` condition is only half of a
  // two-part control; this tag is the other half, and nothing asserted it.
  // Hardcoding `'dev'` for all three environments keeps every IAM bound green
  // and hands the dev deploy role — dev's GitHub environment has no required
  // reviewer — AWS-RunShellScript root shell on the PROD instance.
  //
  // The whole tag set is pinned, so the `Name` tag's construct path is pinned
  // too: a construct rename fails here by design.
  test(`Relay-${envName}: the instance is tagged RelayEnv=${envName}`, () => {
    const instances = Object.values(template.findResources('AWS::EC2::Instance'));
    assert.equal(instances.length, 1);
    const tags = [...((instances[0].Properties?.Tags ?? []) as Array<Record<string, string>>)].sort(
      (a, b) => (a.Key < b.Key ? -1 : a.Key > b.Key ? 1 : 0),
    );
    assert.deepEqual(
      tags,
      [
        { Key: 'Name', Value: `Relay-${envName}/RelayInstance` },
        { Key: 'RelayEnv', Value: envName },
      ],
      `Relay-${envName}'s instance must carry exactly one RelayEnv tag, naming its own environment`,
    );
  });

  // EXHAUSTIVE BOUND — a golden literal, not a probe. (Axis A6: the boot script.)
  //
  // User-data is a root-privileged boot script and was the one shipped artifact
  // with no assertion of any kind on it. Dropping `|| true` onto the compose
  // plugin's `sha256sum -c -` leaves every IAM, tag, volume and DNS assertion
  // above green while a re-uploaded GitHub release asset runs as root on every
  // boot. A probe cannot close that: it bounds only the line it names, and the
  // next escape is a line it does not.
  //
  // So this pins the WHOLE rendered string — the boot commands from
  // lib/relay-stack.ts AND the three assets/ files it inlines verbatim. Assert
  // the shape first: a token-bearing user-data would render as a Fn::Join and
  // must fail loudly here rather than stringify into a passing comparison.
  //
  // Any deliberate boot-sequence or asset change must update the golden in
  // test/user-data-golden.ts consciously — that edit is the review checkpoint.
  test(`Relay-${envName}: complete rendered user-data`, () => {
    const instances = Object.values(template.findResources('AWS::EC2::Instance'));
    assert.equal(instances.length, 1);
    const userData = instances[0].Properties?.UserData as Record<string, unknown>;
    assert.deepEqual(
      Object.keys(userData ?? {}),
      ['Fn::Base64'],
      'user-data must be a plain Fn::Base64 literal — a Fn::Join means a token leaked into the boot script',
    );
    assert.equal(typeof userData['Fn::Base64'], 'string');
    assert.equal(userData['Fn::Base64'], userDataGolden);
  });

  // EXHAUSTIVE BOUND — a golden literal, not a probe. (Axis A5: the host itself.)
  //
  // Every bound above names ONE instance property — the tags, the block
  // devices, the instance profile, the user-data — so a property none of them
  // names is invisible to all of them at once, and the census cannot help:
  // `ImageId` is a scalar, not a resource, and an IMPORTED security group
  // (`SecurityGroup.fromSecurityGroupId`) synthesizes NO resource at all, so
  // `addSecurityGroup` appends a foreign group id to `SecurityGroupIds` while
  // the census is unchanged and the ingress scan — which walks only in-template
  // security groups — never sees it. The host could then sit in a group that
  // allows port 22, contradicting the no-SSH invariant. `machineImage:
  // genericLinux({...})` swapping the AL2023 SSM-latest lookup for an arbitrary
  // AMI, and a `keyName` re-opening key-pair SSH, are the same shape of escape.
  //
  // So this pins the WHOLE `Properties` object, which fails on any added,
  // removed or edited key — named by an existing test or not. The targeted
  // tests above are kept deliberately: they say WHICH invariant broke, this one
  // says THAT something changed.
  //
  // The literal is transcribed from the current synth (and cross-checked equal
  // to the CLI's cdk.out), never hand-composed. `UserData` delegates to the
  // hand-encoded user-data golden rather than restating it. Any deliberate
  // instance change must update this literal consciously — that edit is the
  // review checkpoint.
  test(`Relay-${envName}: the instance's complete Properties`, () => {
    const instances = Object.values(template.findResources('AWS::EC2::Instance'));
    assert.equal(instances.length, 1);
    assert.deepEqual(instances[0].Properties, {
      AvailabilityZone: 'us-east-1a',
      BlockDeviceMappings: [
        {
          DeviceName: '/dev/xvda',
          Ebs: { Encrypted: true, VolumeSize: 8, VolumeType: 'gp3' },
        },
      ],
      IamInstanceProfile: { Ref: 'RelayInstanceInstanceProfileDE8E6059' },
      // The AL2023 arm64 AMI resolved at deploy time through the public SSM
      // parameter CDK's `latestAmazonLinux2023` adds — a `Ref` to that
      // parameter, not a baked-in AMI id.
      ImageId: {
        Ref: 'SsmParameterValueawsserviceamiamazonlinuxlatestal2023amikernel61arm64C96584B6F00A464EAD1953AFF4B05118Parameter',
      },
      InstanceType: 't4g.micro',
      // The IMDSv2 launch template. Its name is the one property that differs
      // across the three stacks: the `uniqueImdsv2TemplateName` feature flag
      // derives it from the stack name plus a construct-path hash.
      LaunchTemplate: {
        LaunchTemplateName: LAUNCH_TEMPLATE_NAMES[envName],
        Version: { 'Fn::GetAtt': ['RelayInstanceLaunchTemplateCDBD5993', 'LatestVersionNumber'] },
      },
      // Exactly one group, this stack's own — the one the ingress scan bounds.
      SecurityGroupIds: [{ 'Fn::GetAtt': ['RelaySg14484F35', 'GroupId'] }],
      SubnetId: 'subnet-00000000000000000',
      Tags: [
        { Key: 'Name', Value: `Relay-${envName}/RelayInstance` },
        { Key: 'RelayEnv', Value: envName },
      ],
      UserData: { 'Fn::Base64': userDataGolden },
    });
  });

  // EXHAUSTIVE BOUND — a golden literal, not a probe. (Axis A7: the resource set.)
  //
  // See `resourceTypeCensus`: every other bound in this file is keyed to a
  // resource type it names, so a resource of an unnamed type is invisible to
  // all of them at once. Pin the whole census.
  //
  // Any deliberate resource addition must update this literal consciously.
  // The full template golden below catches it as an opaque diff; this names it.
  test(`Relay-${envName}: complete resource-type census`, () => {
    assert.deepEqual(resourceTypeCensus(template), {
      'AWS::EC2::EIP': 1,
      'AWS::EC2::EIPAssociation': 1,
      'AWS::EC2::Instance': 1,
      'AWS::EC2::LaunchTemplate': 1,
      'AWS::EC2::SecurityGroup': 1,
      'AWS::IAM::InstanceProfile': 1,
      'AWS::IAM::Policy': 1,
      'AWS::IAM::Role': 1,
      'AWS::Route53::RecordSet': 1,
    });
  });

  // EXHAUSTIVE BOUND — the policy -> principal REVERSE edge. (Axis A8.)
  //
  // See `foreignPolicyAttachments`: every IAM bound above walks role -> policy,
  // so none of them can see a policy that has been attached to an EXTRA
  // principal. `attachToRole(iam.Role.fromRoleName(...))` synthesizes no
  // resource, so the census above is unchanged too, while ssm:GetParameter on
  // /relay/${envName}/auth-token reaches an arbitrary named role. The full
  // template golden below catches it as an opaque diff; this names it.
  test(`Relay-${envName}: every policy attaches only to bounded in-template roles`, () => {
    assert.deepEqual(foreignPolicyAttachments(template), []);
  });

  // EXHAUSTIVE BOUND — the whole template, the backstop beneath every targeted
  // assertion above. (Axis A9: everything no axis names.)
  //
  // Each round of review bounded one more resource by name and the next round
  // found the next unbounded corner — most recently the launch template's
  // `LaunchTemplateData`, from which the instance (declaring neither) inherits
  // BOTH `KeyName` and `MetadataOptions`, so an aspect could re-open key-pair
  // SSH and raise the IMDS hop limit into the containers at full green. Naming
  // it would only move the hole to the property after it, so pin everything.
  //
  // Any deliberate infrastructure change must regenerate test/template-goldens.ts
  // consciously — that diff IS the review artifact: `npm run regen-goldens`.
  test(`Relay-${envName}: complete synthesized template`, () => {
    assert.deepStrictEqual(template.toJSON(), TEMPLATE_GOLDENS[`Relay-${envName}`]);
  });
}

// The three assets/ files below ship inside user-data (bound verbatim above),
// but the user-data golden fails as one opaque diff. The targeted tests below
// read the files from disk and name the specific security property each one
// carries, so a regression says WHICH invariant broke rather than "user-data
// changed". They express INTENT with a precise failure message; they do not
// bound everything else in the file.
//
// EXHAUSTIVE BOUND — the complete content of each asset file, per file rather
// than folded into the rendered-user-data string above, so the failing test
// name says which asset changed. Any deliberate edit to one of these three
// files — a changed image tag, a dropped guard, an added env var, a reordered
// line — must update the matching literal in test/asset-goldens.ts consciously.
test('docker-compose.yml: complete content', () => {
  assert.equal(DOCKER_COMPOSE, DOCKER_COMPOSE_GOLDEN);
});

test('Caddyfile.tpl: complete content', () => {
  assert.equal(CADDYFILE, CADDYFILE_GOLDEN);
});

test('deploy.sh.tpl: complete content', () => {
  assert.equal(DEPLOY_SH, DEPLOY_SH_GOLDEN);
});

// SC2 fail-closed. collab-relay's main.rs treats an empty RELAY_AUTH_TOKEN as
// "auth disabled", so an unset variable must abort `compose up` rather than
// start an open relay. Compose's `:?` form is the whole mechanism: dropping it
// to a bare `${RELAY_AUTH_TOKEN}` substitutes empty and starts an
// unauthenticated relay on the public internet.
test('the on-instance compose unit fails closed on an unset image or auth token', () => {
  const compose = DOCKER_COMPOSE;
  for (const variable of ['RELAY_AUTH_TOKEN', 'RELAY_IMAGE']) {
    assert.ok(
      compose.includes(`\${${variable}:?`),
      `docker-compose.yml must guard \${${variable}} with Compose's \`:?\` form, which aborts \`compose up\` when it is unset or empty`,
    );
  }
});

// Caddy turns automatic HTTPS ON only for a scheme-less (or https://) site
// address. Prefixing it with `http://` disables TLS entirely: the relay then
// serves cleartext on :80 — a port the security group already opens to
// 0.0.0.0/0 for the ACME challenge — and every bearer token travels in the
// clear. Nothing else in the suite would notice.
test('the Caddyfile site address carries no scheme, so automatic HTTPS stays on', () => {
  const lines = CADDYFILE
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line.length > 0 && !line.startsWith('#'));
  assert.ok(lines.length > 0, 'Caddyfile.tpl must declare a site address');
  const [siteAddress] = lines;
  assert.ok(
    !/^[A-Za-z][A-Za-z0-9+.-]*:\/\//.test(siteAddress),
    `the Caddyfile site address must not begin with a scheme (found ${JSON.stringify(siteAddress)}); an explicit http:// disables automatic HTTPS and serves the bearer token in cleartext`,
  );
});

// Both fail-closed guards — compose's `:?` and the non-empty check below — test
// for an EMPTY token, so neither fires on a non-empty WRONG one. A `|| echo
// changeme` fallback on the SSM fetch therefore satisfies both while every
// relay boots with a public constant as its bearer token. The fetch must have
// no fallback at all: `set -euo pipefail` is what turns a failed fetch into a
// failed deploy.
test('deploy.sh fetches the auth token with no fallback and refuses an empty one', () => {
  // Join backslash continuations first: the fetch spans two physical lines, and
  // the non-empty guard two lines below legitimately contains `||`.
  const logicalLines = DEPLOY_SH.replace(/\\\n[ \t]*/g, ' ').split('\n');

  const fetches = logicalLines.filter((line) => line.includes('aws ssm get-parameter'));
  assert.equal(fetches.length, 1, 'deploy.sh.tpl must fetch the auth token exactly once');
  assert.ok(
    !fetches[0].includes('||'),
    `the auth-token fetch must have no \`||\` fallback (found ${JSON.stringify(fetches[0])}); set -e must turn a failed fetch into a failed deploy, never a default token`,
  );

  assert.ok(
    logicalLines.some((line) =>
      /^\[ -n "\$\{RELAY_AUTH_TOKEN\}" \] \|\| \{.*exit 1; \}$/.test(line.trim()),
    ),
    'deploy.sh.tpl must retain the non-empty auth-token guard that exits before starting an unauthenticated relay',
  );
});

// Fix 1 guard: without `@aws-cdk/aws-ec2:uniqueImdsv2TemplateName`, CDK's
// IMDSv2 aspect names every stack's launch template identically
// ("RelayInstanceLaunchTemplate") — harmless within one stack, but launch
// template names are unique per account+region, so deploying all three
// Relay-* stacks (same account/region) fails on the 2nd/3rd with
// InvalidLaunchTemplateName.AlreadyExistsException.
test('Relay-dev/staging/prod: launch template names are pairwise distinct', () => {
  const names = new Set<string>();
  for (const envName of ENV_NAMES) {
    const templates = Template.fromStack(relays[envName]).findResources('AWS::EC2::LaunchTemplate');
    for (const lt of Object.values(templates)) {
      const name = (lt.Properties as Record<string, unknown> | undefined)?.LaunchTemplateName;
      assert.ok(typeof name === 'string' && name.length > 0, `${envName} launch template must have a name`);
      names.add(name as string);
    }
  }
  assert.equal(names.size, ENV_NAMES.length, `expected ${ENV_NAMES.length} distinct launch template names, got ${names.size}`);
});
