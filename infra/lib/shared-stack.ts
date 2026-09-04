import * as cdk from 'aws-cdk-lib';
import * as ecr from 'aws-cdk-lib/aws-ecr';
import * as iam from 'aws-cdk-lib/aws-iam';
import * as route53 from 'aws-cdk-lib/aws-route53';
import { Construct } from 'constructs';

export interface SharedStackProps extends cdk.StackProps {
  /** Apex domain; the delegated zone is `collab.<domain>`. Set once in bin/app.ts. */
  readonly domain: string;
  /** GitHub repository slug the OIDC trust policies are written against. */
  readonly repoSlug: string;
  /** Environment names that each get a deploy role. */
  readonly envNames: readonly string[];
}

const OIDC_ISSUER = 'token.actions.githubusercontent.com';

/**
 * Account-wide singletons: the container registry, the GitHub OIDC trust
 * anchor, the delegated hosted zone, and the four CI roles.
 */
export class SharedStack extends cdk.Stack {
  public readonly ecrRepo: ecr.Repository;
  public readonly zone: route53.PublicHostedZone;

  constructor(scope: Construct, id: string, props: SharedStackProps) {
    super(scope, id, props);

    // Name is the contract M4's `aws ecr describe-images --repository-name
    // obsidian-ee/collab-relay` gate depends on; docs/aws-deployment-plan.md
    // is the single source of truth for the string.
    this.ecrRepo = new ecr.Repository(this, 'RelayRepo', {
      repositoryName: 'obsidian-ee/collab-relay',
      imageTagMutability: ecr.TagMutability.IMMUTABLE,
      imageScanOnPush: true,
      // Release images are the documented rollback target: the M4 runbook rolls
      // back with `deploy.yml -f environment=prod --ref v0.1.0`, which promotes
      // the digest that tag points at. A single `maxImageCount` rule counts them
      // together with every dev/staging `sha-` image, so ~25 dispatches expired
      // the release the runbook tells you to roll back to. Disjoint prefixes are
      // impossible here — a promoted image carries BOTH its `sha-` and `v` tags —
      // so priority ordering is the mechanism: per the ECR lifecycle docs a rule
      // "can never mark images that are marked by higher priority rules, but can
      // still identify them". So rule 2 still COUNTS releases toward its 25, it
      // just cannot expire them; effective dev-churn retention is 25 minus
      // whatever releases sit in the youngest 25. That is fine here (dev churn is
      // disposable) but it is why the release budget is the load-bearing number.
      // A prod dispatch refuses to rebuild a missing image, so an expiry past
      // this budget fails loudly rather than shipping an unvalidated digest.
      lifecycleRules: [
        {
          rulePriority: 1,
          description: 'Retain release images (the rollback target)',
          tagStatus: ecr.TagStatus.TAGGED,
          tagPrefixList: ['v'],
          maxImageCount: 100,
        },
        { rulePriority: 2, description: 'Expire dev/staging churn', maxImageCount: 25 },
      ],
    });

    this.zone = new route53.PublicHostedZone(this, 'CollabZone', {
      zoneName: `collab.${props.domain}`,
    });

    // Bootstrap runbook item 3 records these; item 4 adds them at the registrar.
    new cdk.CfnOutput(this, 'NameServers', {
      value: cdk.Fn.join(',', this.zone.hostedZoneNameServers ?? []),
      description: 'NS records to delegate collab.<domain> at the registrar',
    });
    new cdk.CfnOutput(this, 'EcrRepositoryUri', { value: this.ecrRepo.repositoryUri });

    // Runbook item 8's `vars.ECR_REPOSITORY` is the BARE repository name, not
    // the URI above: deploy.yml feeds it to `aws ecr describe-images
    // --repository-name` AND uses it as the image path segment after the
    // registry host, so the URI would break the first and double the host in
    // the second (deploy.sh then derives the wrong login host from `${IMAGE%%/*}`).
    new cdk.CfnOutput(this, 'RepositoryName', {
      value: this.ecrRepo.repositoryName,
      description: 'Bare ECR repository name — the value for the ECR_REPOSITORY repo variable',
    });

    // The L1 resource, not iam.OpenIdConnectProvider: the L2 is a CDK custom
    // resource, i.e. a Lambda holding iam:*OpenIDConnectProvider on "*" (it can
    // rewrite ANY provider in the account, including this one's trust anchor)
    // that fetches the issuer's certificate with RejectUnauthorized:false.
    // AWS::IAM::OIDCProvider needs neither. The thumbprint is a placeholder:
    // IAM verifies token.actions.githubusercontent.com's JWKS endpoint against
    // its library of trusted root CAs and ignores configured thumbprints for
    // such providers (id_roles_providers_create_oidc_verify-thumbprint), so
    // pinning a real SHA-1 here would only add a rotation liability.
    const oidcProvider = new iam.CfnOIDCProvider(this, 'GithubOidc', {
      url: `https://${OIDC_ISSUER}`,
      clientIdList: ['sts.amazonaws.com'],
      thumbprintList: ['ffffffffffffffffffffffffffffffffffffffff'],
    });

    // Residual R5, deliberate: the push role trusts ALL refs of the repository.
    // Dev builds run from arbitrary branches and prod from tags, and an
    // immutable registry bounds the blast radius of a push. Environment
    // protection lives one layer down, in the env-scoped deploy roles below.
    const pushRole = new iam.Role(this, 'EcrPushRole', {
      roleName: 'obsidian-ee-ecr-push',
      assumedBy: new iam.WebIdentityPrincipal(oidcProvider.attrArn, {
        StringEquals: { [`${OIDC_ISSUER}:aud`]: 'sts.amazonaws.com' },
        StringLike: { [`${OIDC_ISSUER}:sub`]: `repo:${props.repoSlug}:*` },
      }),
      description: 'GitHub Actions build job: push and retag relay images (R5: ref-wide trust)',
    });
    pushRole.addToPolicy(
      new iam.PolicyStatement({
        actions: ['ecr:GetAuthorizationToken'],
        resources: ['*'],
      }),
    );
    pushRole.addToPolicy(
      new iam.PolicyStatement({
        actions: [
          'ecr:BatchCheckLayerAvailability',
          'ecr:BatchGetImage',
          'ecr:CompleteLayerUpload',
          'ecr:DescribeImages',
          'ecr:GetDownloadUrlForLayer',
          'ecr:InitiateLayerUpload',
          'ecr:PutImage',
          'ecr:UploadLayerPart',
        ],
        resources: [this.ecrRepo.repositoryArn],
      }),
    );
    new cdk.CfnOutput(this, 'EcrPushRoleArn', { value: pushRole.roleArn });

    // One deploy role per environment, each trusting ONLY its own GitHub
    // environment — an exact sub match, never a wildcard. The staging role
    // therefore cannot be assumed in a run that has not passed staging's
    // required reviewer.
    for (const envName of props.envNames) {
      const deployRole = new iam.Role(this, `DeployRole${titleCase(envName)}`, {
        roleName: `obsidian-ee-deploy-${envName}`,
        assumedBy: new iam.WebIdentityPrincipal(oidcProvider.attrArn, {
          StringEquals: {
            [`${OIDC_ISSUER}:aud`]: 'sts.amazonaws.com',
            [`${OIDC_ISSUER}:sub`]: `repo:${props.repoSlug}:environment:${envName}`,
          },
        }),
        description: `GitHub Actions deploy job for the ${envName} environment`,
      });

      // Only the run-shell document, only against instances tagged for this
      // environment (tag-scoped, so no cross-stack instance-id cycle).
      deployRole.addToPolicy(
        new iam.PolicyStatement({
          actions: ['ssm:SendCommand'],
          resources: [`arn:aws:ssm:${this.region}::document/AWS-RunShellScript`],
        }),
      );
      deployRole.addToPolicy(
        new iam.PolicyStatement({
          actions: ['ssm:SendCommand'],
          resources: [`arn:aws:ec2:${this.region}:${this.account}:instance/*`],
          conditions: { StringEquals: { 'ssm:resourceTag/RelayEnv': envName } },
        }),
      );
      // Accepted residual, pinned by an API limit rather than by choice:
      // ssm:GetCommandInvocation declares no resource types in the AWS service
      // authorization reference, so "*" is the only expressible resource. Read-only.
      deployRole.addToPolicy(
        new iam.PolicyStatement({
          actions: ['ssm:GetCommandInvocation'],
          resources: ['*'],
        }),
      );

      // Bootstrap runbook item 8 sets this as the env-scoped AWS_DEPLOY_ROLE_ARN.
      new cdk.CfnOutput(this, `DeployRoleArn${titleCase(envName)}`, {
        value: deployRole.roleArn,
        description: `AWS_DEPLOY_ROLE_ARN for the ${envName} GitHub environment`,
      });
    }
  }
}

function titleCase(s: string): string {
  return s.charAt(0).toUpperCase() + s.slice(1);
}
