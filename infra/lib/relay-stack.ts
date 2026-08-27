import * as fs from 'fs';
import * as path from 'path';
import * as cdk from 'aws-cdk-lib';
import * as ec2 from 'aws-cdk-lib/aws-ec2';
import * as ecr from 'aws-cdk-lib/aws-ecr';
import * as iam from 'aws-cdk-lib/aws-iam';
import * as route53 from 'aws-cdk-lib/aws-route53';
import { Construct } from 'constructs';

export interface RelayStackProps extends cdk.StackProps {
  /** dev | staging | prod. Drives the hostname, the RelayEnv tag, and the token path. */
  readonly envName: string;
  /** Apex domain; the published host is `relay-<env>.collab.<domain>`. */
  readonly domain: string;
  /** The delegated zone from SharedStack. */
  readonly zone: route53.IHostedZone;
  /** The shared registry, for the instance role's pull permission. */
  readonly ecrRepo: ecr.IRepository;
  /** Existing (default) VPC the instance lives in — see bin/app.ts. */
  readonly vpcId: string;
  /** One public subnet of that VPC, and its AZ. */
  readonly subnetId: string;
  readonly availabilityZone: string;
}

/** Pinned aarch64 compose plugin: AL2023's repos do not carry docker-compose. */
const COMPOSE_PLUGIN_VERSION = 'v2.29.7';
/**
 * Published sha256 of docker-compose-linux-aarch64 at that tag (GitHub release
 * assets `docker-compose-linux-aarch64.sha256` and `checksums.txt` agree).
 * Hardcoded rather than fetched: a release asset is mutable, so downloading the
 * checksum from the same mutable release verifies nothing — the binary runs as
 * root on every boot, and unreviewed root code is exactly what a re-uploaded
 * asset would give us.
 */
const COMPOSE_PLUGIN_SHA256 = '6e9fbd5daa20dca5d7d89145081ae8155d68ef2928b497d9f85b54fe0f9dbb2c';
/** AL2023 arm64 root device — named so the root volume can be encrypted. */
const ROOT_DEVICE_NAME = '/dev/xvda';
const ASSETS_DIR = path.join(__dirname, '..', 'assets');

/**
 * The same three assets are rendered once per environment, so an un-cached read
 * hits the disk nine times per synth for three distinct files. Only the
 * substitution below varies by environment; the bytes do not.
 */
const assetSources = new Map<string, string>();
function readAssetOnce(name: string): string {
  let source = assetSources.get(name);
  if (source === undefined) {
    source = fs.readFileSync(path.join(ASSETS_DIR, name), 'utf8');
    assetSources.set(name, source);
  }
  return source;
}

/** Everything one environment owns: instance, SG, role, EIP, DNS, user-data. */
export class RelayStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: RelayStackProps) {
    super(scope, id, props);

    const { envName } = props;

    // Referenced by attributes rather than looked up: `Vpc.fromLookup` makes a
    // live AWS call at synth time, and synth must succeed with no credentials
    // (M2 gate, CI). The ids are bootstrap placeholders set in bin/app.ts.
    const vpc = ec2.Vpc.fromVpcAttributes(this, 'DefaultVpc', {
      vpcId: props.vpcId,
      availabilityZones: [props.availabilityZone],
      publicSubnetIds: [props.subnetId],
    });

    // G6: only the web ports are open. No 22 (deploys and break-glass both go
    // through SSM, D3), and no UDP 443 — HTTP/3 stays off (residual R6).
    const securityGroup = new ec2.SecurityGroup(this, 'RelaySg', {
      vpc,
      description: `Relay ${envName}: web ports only`,
      allowAllOutbound: true,
    });
    securityGroup.addIngressRule(ec2.Peer.anyIpv4(), ec2.Port.tcp(80), 'ACME HTTP challenge and redirect');
    securityGroup.addIngressRule(ec2.Peer.anyIpv4(), ec2.Port.tcp(443), 'TLS / WebSocket');

    const instanceRole = new iam.Role(this, 'RelayInstanceRole', {
      assumedBy: new iam.ServicePrincipal('ec2.amazonaws.com'),
      description: `Relay ${envName} instance: SSM agent channel, registry pull, own token only`,
    });
    // Deliberately NOT AmazonSSMManagedInstanceCore: that managed policy grants
    // ssm:GetParameter* on Resource "*", which unions over the scoped grant
    // below and would let any instance read every environment's token (a
    // SecureString under the default alias/aws/ssm key does not block it — the
    // key policy allows any principal in the account that has the SSM
    // permission). These are the agent actions Run Command and instance
    // registration actually need: the Session Manager / message channel, plus
    // the association and document path the agent polls on its own (without
    // ListInstanceAssociations and friends the agent AccessDenied-loops even
    // though Run Command itself works). Not one of them is ssm:GetParameter*,
    // so the own-token-only invariant below still holds for the whole role.
    // The ssmmessages and ec2messages actions define no resource types at all
    // (AWS service authorization reference), so "*" is the only expressible
    // resource; ssm:UpdateInstanceInformation does define one, but scoping it
    // to this stack's own instance ARN would create a role -> instance -> role
    // cycle, so it keeps the agent-baseline "*".
    instanceRole.addToPolicy(
      new iam.PolicyStatement({
        actions: [
          'ssm:UpdateInstanceInformation',
          'ssm:ListInstanceAssociations',
          'ssm:ListAssociations',
          'ssm:DescribeAssociation',
          'ssm:UpdateInstanceAssociationStatus',
          'ssm:GetDocument',
          'ssm:DescribeDocument',
          'ssm:GetManifest',
          'ssm:PutInventory',
          'ssm:PutComplianceItems',
          'ssm:GetDeployablePatchSnapshotForInstance',
          'ssmmessages:CreateControlChannel',
          'ssmmessages:CreateDataChannel',
          'ssmmessages:OpenControlChannel',
          'ssmmessages:OpenDataChannel',
          'ec2messages:AcknowledgeMessage',
          'ec2messages:DeleteMessage',
          'ec2messages:FailMessage',
          'ec2messages:GetEndpoint',
          'ec2messages:GetMessages',
          'ec2messages:SendReply',
        ],
        resources: ['*'],
      }),
    );
    instanceRole.addToPolicy(
      new iam.PolicyStatement({ actions: ['ecr:GetAuthorizationToken'], resources: ['*'] }),
    );
    instanceRole.addToPolicy(
      new iam.PolicyStatement({
        actions: ['ecr:BatchCheckLayerAvailability', 'ecr:BatchGetImage', 'ecr:GetDownloadUrlForLayer'],
        resources: [props.ecrRepo.repositoryArn],
      }),
    );
    // Exactly this environment's token parameter — never a wildcard across
    // environments (invariant 4), and the ONLY ssm:GetParameter* grant this
    // role carries from any source, inline or managed. The SecureString itself is created
    // out-of-band by the maintainer (Bootstrap runbook item 5); CloudFormation
    // cannot create SecureString parameters, and the delivery path only reads.
    instanceRole.addToPolicy(
      new iam.PolicyStatement({
        actions: ['ssm:GetParameter'],
        resources: [
          `arn:aws:ssm:${this.region}:${this.account}:parameter/relay/${envName}/auth-token`,
        ],
      }),
    );

    const userData = this.buildUserData(props);
    const instance = new ec2.Instance(this, 'RelayInstance', {
      vpc,
      vpcSubnets: { subnets: [vpc.publicSubnets[0]] },
      instanceType: ec2.InstanceType.of(ec2.InstanceClass.T4G, ec2.InstanceSize.MICRO),
      machineImage: ec2.MachineImage.latestAmazonLinux2023({
        cpuType: ec2.AmazonLinuxCpuType.ARM_64,
      }),
      securityGroup,
      role: instanceRole,
      // IMDSv1's unauthenticated 169.254.169.254 GET turns any SSRF in the
      // proxied stack into role-credential theft; v2's token handshake doesn't.
      // CFN's AWS::EC2::Instance has no MetadataOptions, so CDK expresses this
      // as a launch template the instance references.
      requireImdsv2: true,
      // deploy.sh writes the decrypted auth token to /opt/relay/.env, and the
      // instance keeps no other volume — so the root volume must be encrypted.
      blockDevices: [
        {
          deviceName: ROOT_DEVICE_NAME,
          volume: ec2.BlockDeviceVolume.ebs(8, {
            encrypted: true,
            volumeType: ec2.EbsDeviceVolumeType.GP3,
          }),
        },
      ],
      userData,
    });
    // The deploy role's ssm:SendCommand condition matches on this tag.
    cdk.Tags.of(instance).add('RelayEnv', envName);

    // Without a resource signal CloudFormation waits only for `running`, so a
    // user-data failure — no route to the internet from the chosen subnet, a
    // 404 on the compose plugin, a checksum mismatch — still reports
    // CREATE_COMPLETE with no /opt/relay/deploy.sh on the host. The failure
    // then first surfaces at M4's deploy as an SSM "no such file", after the
    // EIP, the DNS record and a Let's Encrypt certificate have been consumed.
    // `addSignalOnExitCommand` installs an EXIT trap, so the FAILURE path
    // signals failure immediately instead of burning the whole timeout.
    userData.addSignalOnExitCommand(instance);
    (instance.node.defaultChild as ec2.CfnInstance).cfnOptions.creationPolicy = {
      resourceSignal: { count: 1, timeout: 'PT15M' },
    };

    const eip = new ec2.CfnEIP(this, 'RelayEip', { domain: 'vpc' });
    new ec2.CfnEIPAssociation(this, 'RelayEipAssociation', {
      allocationId: eip.attrAllocationId,
      instanceId: instance.instanceId,
    });

    new route53.ARecord(this, 'RelayDns', {
      zone: props.zone,
      recordName: `relay-${envName}`,
      target: route53.RecordTarget.fromIpAddresses(eip.attrPublicIp),
      ttl: cdk.Duration.minutes(5),
    });

    new cdk.CfnOutput(this, 'InstanceId', { value: instance.instanceId });
    new cdk.CfnOutput(this, 'Hostname', {
      value: `relay-${envName}.${props.zone.zoneName}`,
    });
  }

  /**
   * Installs Docker plus the pinned compose plugin and writes the three
   * /opt/relay files from the templated assets. It deliberately does NOT start
   * the relay: the first deploy run does, because only the deploy script knows
   * the image ref and can fetch the auth token.
   */
  private buildUserData(props: RelayStackProps): ec2.UserData {
    const userData = ec2.UserData.forLinux();
    userData.addCommands(
      'set -euxo pipefail',
      // AL2023 does not ship cfn-bootstrap, and the EXIT trap rendered above
      // these lines calls /opt/aws/bin/cfn-signal. Install it first: until it
      // exists a failure signals nothing and the stack waits out the full
      // CreationPolicy timeout instead of failing fast.
      'dnf install -y aws-cfn-bootstrap',
      'dnf install -y docker',
      'systemctl enable --now docker',
      'install -d -m 0755 /usr/libexec/docker/cli-plugins',
      `curl -fsSL https://github.com/docker/compose/releases/download/${COMPOSE_PLUGIN_VERSION}/docker-compose-linux-aarch64 -o /usr/libexec/docker/cli-plugins/docker-compose`,
      // A GitHub release asset can be re-uploaded under the same tag, so the
      // pin alone does not pin the bytes — and these bytes run as root. Verify
      // against the checksum reviewed at authoring time; `set -e` turns a
      // mismatch into a failed boot rather than a silently swapped binary.
      `echo "${COMPOSE_PLUGIN_SHA256}  /usr/libexec/docker/cli-plugins/docker-compose" | sha256sum -c -`,
      'chmod 0755 /usr/libexec/docker/cli-plugins/docker-compose',
      'install -d -m 0755 /opt/relay',
      writeFile('/opt/relay/docker-compose.yml', this.renderAsset('docker-compose.yml', props), 'COMPOSE'),
      writeFile('/opt/relay/Caddyfile', this.renderAsset('Caddyfile.tpl', props), 'CADDYFILE'),
      writeFile('/opt/relay/deploy.sh', this.renderAsset('deploy.sh.tpl', props), 'DEPLOYSH'),
      'chmod 0755 /opt/relay/deploy.sh',
    );
    return userData;
  }

  /** One substitution path for all three assets and all three environments. */
  private renderAsset(name: string, props: RelayStackProps): string {
    return readAssetOnce(name)
      .replace(/\{\{ENV\}\}/g, props.envName)
      .replace(/\{\{REGION\}\}/g, this.region)
      .replace(/<domain>/g, props.domain);
  }
}

/**
 * A quoted heredoc: the assets contain `${RELAY_IMAGE}` and `${RELAY_AUTH_TOKEN}`
 * placeholders that Compose resolves from /opt/relay/.env at deploy time. An
 * unquoted heredoc would expand them to empty at boot — a silent open relay.
 */
function writeFile(destination: string, contents: string, tag: string): string {
  const delimiter = `RELAY_${tag}_EOF`;
  return `cat <<'${delimiter}' > ${destination}\n${contents}\n${delimiter}`;
}
