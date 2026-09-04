/**
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
export const TEMPLATE_GOLDENS: Record<string, unknown> = {
  "RelayShared": {
    "Description": "Shared registry, OIDC provider, delegated zone, and CI roles",
    "Resources": {
      "RelayRepo971E060D": {
        "Type": "AWS::ECR::Repository",
        "Properties": {
          "ImageScanningConfiguration": {
            "ScanOnPush": true
          },
          "ImageTagMutability": "IMMUTABLE",
          "LifecyclePolicy": {
            "LifecyclePolicyText": "{\"rules\":[{\"rulePriority\":1,\"description\":\"Retain release images (the rollback target)\",\"selection\":{\"tagStatus\":\"tagged\",\"tagPrefixList\":[\"v\"],\"countType\":\"imageCountMoreThan\",\"countNumber\":100},\"action\":{\"type\":\"expire\"}},{\"rulePriority\":2,\"description\":\"Expire dev/staging churn\",\"selection\":{\"tagStatus\":\"any\",\"countType\":\"imageCountMoreThan\",\"countNumber\":25},\"action\":{\"type\":\"expire\"}}]}"
          },
          "RepositoryName": "obsidian-ee/collab-relay"
        },
        "UpdateReplacePolicy": "Retain",
        "DeletionPolicy": "Retain"
      },
      "CollabZoneC5E0737B": {
        "Type": "AWS::Route53::HostedZone",
        "Properties": {
          "Name": "collab.example.com."
        }
      },
      "GithubOidc": {
        "Type": "AWS::IAM::OIDCProvider",
        "Properties": {
          "ClientIdList": [
            "sts.amazonaws.com"
          ],
          "ThumbprintList": [
            "ffffffffffffffffffffffffffffffffffffffff"
          ],
          "Url": "https://token.actions.githubusercontent.com"
        }
      },
      "EcrPushRoleEB1E9B11": {
        "Type": "AWS::IAM::Role",
        "Properties": {
          "AssumeRolePolicyDocument": {
            "Statement": [
              {
                "Action": "sts:AssumeRoleWithWebIdentity",
                "Condition": {
                  "StringEquals": {
                    "token.actions.githubusercontent.com:aud": "sts.amazonaws.com"
                  },
                  "StringLike": {
                    "token.actions.githubusercontent.com:sub": "repo:cajias/obsidian-ee:*"
                  }
                },
                "Effect": "Allow",
                "Principal": {
                  "Federated": {
                    "Fn::GetAtt": [
                      "GithubOidc",
                      "Arn"
                    ]
                  }
                }
              }
            ],
            "Version": "2012-10-17"
          },
          "Description": "GitHub Actions build job: push and retag relay images (R5: ref-wide trust)",
          "RoleName": "obsidian-ee-ecr-push"
        }
      },
      "EcrPushRoleDefaultPolicyA4855448": {
        "Type": "AWS::IAM::Policy",
        "Properties": {
          "PolicyDocument": {
            "Statement": [
              {
                "Action": "ecr:GetAuthorizationToken",
                "Effect": "Allow",
                "Resource": "*"
              },
              {
                "Action": [
                  "ecr:BatchCheckLayerAvailability",
                  "ecr:BatchGetImage",
                  "ecr:CompleteLayerUpload",
                  "ecr:DescribeImages",
                  "ecr:GetDownloadUrlForLayer",
                  "ecr:InitiateLayerUpload",
                  "ecr:PutImage",
                  "ecr:UploadLayerPart"
                ],
                "Effect": "Allow",
                "Resource": {
                  "Fn::GetAtt": [
                    "RelayRepo971E060D",
                    "Arn"
                  ]
                }
              }
            ],
            "Version": "2012-10-17"
          },
          "PolicyName": "EcrPushRoleDefaultPolicyA4855448",
          "Roles": [
            {
              "Ref": "EcrPushRoleEB1E9B11"
            }
          ]
        }
      },
      "DeployRoleDev200F3129": {
        "Type": "AWS::IAM::Role",
        "Properties": {
          "AssumeRolePolicyDocument": {
            "Statement": [
              {
                "Action": "sts:AssumeRoleWithWebIdentity",
                "Condition": {
                  "StringEquals": {
                    "token.actions.githubusercontent.com:aud": "sts.amazonaws.com",
                    "token.actions.githubusercontent.com:sub": "repo:cajias/obsidian-ee:environment:dev"
                  }
                },
                "Effect": "Allow",
                "Principal": {
                  "Federated": {
                    "Fn::GetAtt": [
                      "GithubOidc",
                      "Arn"
                    ]
                  }
                }
              }
            ],
            "Version": "2012-10-17"
          },
          "Description": "GitHub Actions deploy job for the dev environment",
          "RoleName": "obsidian-ee-deploy-dev"
        }
      },
      "DeployRoleDevDefaultPolicyBE635A1D": {
        "Type": "AWS::IAM::Policy",
        "Properties": {
          "PolicyDocument": {
            "Statement": [
              {
                "Action": "ssm:SendCommand",
                "Effect": "Allow",
                "Resource": "arn:aws:ssm:us-east-1::document/AWS-RunShellScript"
              },
              {
                "Action": "ssm:SendCommand",
                "Condition": {
                  "StringEquals": {
                    "ssm:resourceTag/RelayEnv": "dev"
                  }
                },
                "Effect": "Allow",
                "Resource": "arn:aws:ec2:us-east-1:111111111111:instance/*"
              },
              {
                "Action": "ssm:GetCommandInvocation",
                "Effect": "Allow",
                "Resource": "*"
              }
            ],
            "Version": "2012-10-17"
          },
          "PolicyName": "DeployRoleDevDefaultPolicyBE635A1D",
          "Roles": [
            {
              "Ref": "DeployRoleDev200F3129"
            }
          ]
        }
      },
      "DeployRoleStaging9870CAB9": {
        "Type": "AWS::IAM::Role",
        "Properties": {
          "AssumeRolePolicyDocument": {
            "Statement": [
              {
                "Action": "sts:AssumeRoleWithWebIdentity",
                "Condition": {
                  "StringEquals": {
                    "token.actions.githubusercontent.com:aud": "sts.amazonaws.com",
                    "token.actions.githubusercontent.com:sub": "repo:cajias/obsidian-ee:environment:staging"
                  }
                },
                "Effect": "Allow",
                "Principal": {
                  "Federated": {
                    "Fn::GetAtt": [
                      "GithubOidc",
                      "Arn"
                    ]
                  }
                }
              }
            ],
            "Version": "2012-10-17"
          },
          "Description": "GitHub Actions deploy job for the staging environment",
          "RoleName": "obsidian-ee-deploy-staging"
        }
      },
      "DeployRoleStagingDefaultPolicyCE247E9F": {
        "Type": "AWS::IAM::Policy",
        "Properties": {
          "PolicyDocument": {
            "Statement": [
              {
                "Action": "ssm:SendCommand",
                "Effect": "Allow",
                "Resource": "arn:aws:ssm:us-east-1::document/AWS-RunShellScript"
              },
              {
                "Action": "ssm:SendCommand",
                "Condition": {
                  "StringEquals": {
                    "ssm:resourceTag/RelayEnv": "staging"
                  }
                },
                "Effect": "Allow",
                "Resource": "arn:aws:ec2:us-east-1:111111111111:instance/*"
              },
              {
                "Action": "ssm:GetCommandInvocation",
                "Effect": "Allow",
                "Resource": "*"
              }
            ],
            "Version": "2012-10-17"
          },
          "PolicyName": "DeployRoleStagingDefaultPolicyCE247E9F",
          "Roles": [
            {
              "Ref": "DeployRoleStaging9870CAB9"
            }
          ]
        }
      },
      "DeployRoleProdBDDD333C": {
        "Type": "AWS::IAM::Role",
        "Properties": {
          "AssumeRolePolicyDocument": {
            "Statement": [
              {
                "Action": "sts:AssumeRoleWithWebIdentity",
                "Condition": {
                  "StringEquals": {
                    "token.actions.githubusercontent.com:aud": "sts.amazonaws.com",
                    "token.actions.githubusercontent.com:sub": "repo:cajias/obsidian-ee:environment:prod"
                  }
                },
                "Effect": "Allow",
                "Principal": {
                  "Federated": {
                    "Fn::GetAtt": [
                      "GithubOidc",
                      "Arn"
                    ]
                  }
                }
              }
            ],
            "Version": "2012-10-17"
          },
          "Description": "GitHub Actions deploy job for the prod environment",
          "RoleName": "obsidian-ee-deploy-prod"
        }
      },
      "DeployRoleProdDefaultPolicy1C9C08BC": {
        "Type": "AWS::IAM::Policy",
        "Properties": {
          "PolicyDocument": {
            "Statement": [
              {
                "Action": "ssm:SendCommand",
                "Effect": "Allow",
                "Resource": "arn:aws:ssm:us-east-1::document/AWS-RunShellScript"
              },
              {
                "Action": "ssm:SendCommand",
                "Condition": {
                  "StringEquals": {
                    "ssm:resourceTag/RelayEnv": "prod"
                  }
                },
                "Effect": "Allow",
                "Resource": "arn:aws:ec2:us-east-1:111111111111:instance/*"
              },
              {
                "Action": "ssm:GetCommandInvocation",
                "Effect": "Allow",
                "Resource": "*"
              }
            ],
            "Version": "2012-10-17"
          },
          "PolicyName": "DeployRoleProdDefaultPolicy1C9C08BC",
          "Roles": [
            {
              "Ref": "DeployRoleProdBDDD333C"
            }
          ]
        }
      }
    },
    "Outputs": {
      "NameServers": {
        "Description": "NS records to delegate collab.<domain> at the registrar",
        "Value": {
          "Fn::Join": [
            ",",
            {
              "Fn::GetAtt": [
                "CollabZoneC5E0737B",
                "NameServers"
              ]
            }
          ]
        }
      },
      "EcrRepositoryUri": {
        "Value": {
          "Fn::Join": [
            "",
            [
              {
                "Fn::Select": [
                  4,
                  {
                    "Fn::Split": [
                      ":",
                      {
                        "Fn::GetAtt": [
                          "RelayRepo971E060D",
                          "Arn"
                        ]
                      }
                    ]
                  }
                ]
              },
              ".dkr.ecr.",
              {
                "Fn::Select": [
                  3,
                  {
                    "Fn::Split": [
                      ":",
                      {
                        "Fn::GetAtt": [
                          "RelayRepo971E060D",
                          "Arn"
                        ]
                      }
                    ]
                  }
                ]
              },
              ".",
              {
                "Ref": "AWS::URLSuffix"
              },
              "/",
              {
                "Ref": "RelayRepo971E060D"
              }
            ]
          ]
        }
      },
      "RepositoryName": {
        "Description": "Bare ECR repository name — the value for the ECR_REPOSITORY repo variable",
        "Value": {
          "Ref": "RelayRepo971E060D"
        }
      },
      "EcrPushRoleArn": {
        "Value": {
          "Fn::GetAtt": [
            "EcrPushRoleEB1E9B11",
            "Arn"
          ]
        }
      },
      "DeployRoleArnDev": {
        "Description": "AWS_DEPLOY_ROLE_ARN for the dev GitHub environment",
        "Value": {
          "Fn::GetAtt": [
            "DeployRoleDev200F3129",
            "Arn"
          ]
        }
      },
      "DeployRoleArnStaging": {
        "Description": "AWS_DEPLOY_ROLE_ARN for the staging GitHub environment",
        "Value": {
          "Fn::GetAtt": [
            "DeployRoleStaging9870CAB9",
            "Arn"
          ]
        }
      },
      "DeployRoleArnProd": {
        "Description": "AWS_DEPLOY_ROLE_ARN for the prod GitHub environment",
        "Value": {
          "Fn::GetAtt": [
            "DeployRoleProdBDDD333C",
            "Arn"
          ]
        }
      },
      "ExportsOutputFnGetAttRelayRepo971E060DArn3C32FA8A": {
        "Value": {
          "Fn::GetAtt": [
            "RelayRepo971E060D",
            "Arn"
          ]
        },
        "Export": {
          "Name": "RelayShared:ExportsOutputFnGetAttRelayRepo971E060DArn3C32FA8A"
        }
      },
      "ExportsOutputRefCollabZoneC5E0737B29A41A37": {
        "Value": {
          "Ref": "CollabZoneC5E0737B"
        },
        "Export": {
          "Name": "RelayShared:ExportsOutputRefCollabZoneC5E0737B29A41A37"
        }
      }
    },
    "Parameters": {
      "BootstrapVersion": {
        "Type": "AWS::SSM::Parameter::Value<String>",
        "Default": "/cdk-bootstrap/hnb659fds/version",
        "Description": "Version of the CDK Bootstrap resources in this environment, automatically retrieved from SSM Parameter Store. [cdk:skip]"
      }
    },
    "Rules": {
      "CheckBootstrapVersion": {
        "Assertions": [
          {
            "Assert": {
              "Fn::Not": [
                {
                  "Fn::Contains": [
                    [
                      "1",
                      "2",
                      "3",
                      "4",
                      "5"
                    ],
                    {
                      "Ref": "BootstrapVersion"
                    }
                  ]
                }
              ]
            },
            "AssertDescription": "CDK bootstrap stack version 6 required. Please run 'cdk bootstrap' with a recent version of the CDK CLI."
          }
        ]
      }
    }
  },
  "Relay-dev": {
    "Description": "Relay dev: one t4g.micro instance behind Caddy",
    "Resources": {
      "RelaySg14484F35": {
        "Type": "AWS::EC2::SecurityGroup",
        "Properties": {
          "GroupDescription": "Relay dev: web ports only",
          "SecurityGroupEgress": [
            {
              "CidrIp": "0.0.0.0/0",
              "Description": "Allow all outbound traffic by default",
              "IpProtocol": "-1"
            }
          ],
          "SecurityGroupIngress": [
            {
              "CidrIp": "0.0.0.0/0",
              "Description": "ACME HTTP challenge and redirect",
              "FromPort": 80,
              "IpProtocol": "tcp",
              "ToPort": 80
            },
            {
              "CidrIp": "0.0.0.0/0",
              "Description": "TLS / WebSocket",
              "FromPort": 443,
              "IpProtocol": "tcp",
              "ToPort": 443
            }
          ],
          "VpcId": "vpc-00000000000000000"
        }
      },
      "RelayInstanceRoleBB4CA3BD": {
        "Type": "AWS::IAM::Role",
        "Properties": {
          "AssumeRolePolicyDocument": {
            "Statement": [
              {
                "Action": "sts:AssumeRole",
                "Effect": "Allow",
                "Principal": {
                  "Service": "ec2.amazonaws.com"
                }
              }
            ],
            "Version": "2012-10-17"
          },
          "Description": "Relay dev instance: SSM agent channel, registry pull, own token only"
        }
      },
      "RelayInstanceRoleDefaultPolicy3A2ED09E": {
        "Type": "AWS::IAM::Policy",
        "Properties": {
          "PolicyDocument": {
            "Statement": [
              {
                "Action": [
                  "ec2messages:AcknowledgeMessage",
                  "ec2messages:DeleteMessage",
                  "ec2messages:FailMessage",
                  "ec2messages:GetEndpoint",
                  "ec2messages:GetMessages",
                  "ec2messages:SendReply",
                  "ecr:GetAuthorizationToken",
                  "ssm:DescribeAssociation",
                  "ssm:DescribeDocument",
                  "ssm:GetDeployablePatchSnapshotForInstance",
                  "ssm:GetDocument",
                  "ssm:GetManifest",
                  "ssm:ListAssociations",
                  "ssm:ListInstanceAssociations",
                  "ssm:PutComplianceItems",
                  "ssm:PutInventory",
                  "ssm:UpdateInstanceAssociationStatus",
                  "ssm:UpdateInstanceInformation",
                  "ssmmessages:CreateControlChannel",
                  "ssmmessages:CreateDataChannel",
                  "ssmmessages:OpenControlChannel",
                  "ssmmessages:OpenDataChannel"
                ],
                "Effect": "Allow",
                "Resource": "*"
              },
              {
                "Action": [
                  "ecr:BatchCheckLayerAvailability",
                  "ecr:BatchGetImage",
                  "ecr:GetDownloadUrlForLayer"
                ],
                "Effect": "Allow",
                "Resource": {
                  "Fn::ImportValue": "RelayShared:ExportsOutputFnGetAttRelayRepo971E060DArn3C32FA8A"
                }
              },
              {
                "Action": "ssm:GetParameter",
                "Effect": "Allow",
                "Resource": "arn:aws:ssm:us-east-1:111111111111:parameter/relay/dev/auth-token"
              }
            ],
            "Version": "2012-10-17"
          },
          "PolicyName": "RelayInstanceRoleDefaultPolicy3A2ED09E",
          "Roles": [
            {
              "Ref": "RelayInstanceRoleBB4CA3BD"
            }
          ]
        }
      },
      "RelayInstanceInstanceProfileDE8E6059": {
        "Type": "AWS::IAM::InstanceProfile",
        "Properties": {
          "Roles": [
            {
              "Ref": "RelayInstanceRoleBB4CA3BD"
            }
          ]
        }
      },
      "RelayInstance8298494B": {
        "Type": "AWS::EC2::Instance",
        "Properties": {
          "AvailabilityZone": "us-east-1a",
          "BlockDeviceMappings": [
            {
              "DeviceName": "/dev/xvda",
              "Ebs": {
                "Encrypted": true,
                "VolumeSize": 8,
                "VolumeType": "gp3"
              }
            }
          ],
          "IamInstanceProfile": {
            "Ref": "RelayInstanceInstanceProfileDE8E6059"
          },
          "ImageId": {
            "Ref": "SsmParameterValueawsserviceamiamazonlinuxlatestal2023amikernel61arm64C96584B6F00A464EAD1953AFF4B05118Parameter"
          },
          "InstanceType": "t4g.micro",
          "LaunchTemplate": {
            "LaunchTemplateName": "RelaydevRelayInstanceLaunchTemplate154AAAB8",
            "Version": {
              "Fn::GetAtt": [
                "RelayInstanceLaunchTemplateCDBD5993",
                "LatestVersionNumber"
              ]
            }
          },
          "SecurityGroupIds": [
            {
              "Fn::GetAtt": [
                "RelaySg14484F35",
                "GroupId"
              ]
            }
          ],
          "SubnetId": "subnet-00000000000000000",
          "Tags": [
            {
              "Key": "Name",
              "Value": "Relay-dev/RelayInstance"
            },
            {
              "Key": "RelayEnv",
              "Value": "dev"
            }
          ],
          "UserData": {
            "Fn::Base64": "#!/bin/bash\nfunction exitTrap(){\nexitCode=$?\n/opt/aws/bin/cfn-signal --stack Relay-dev --resource RelayInstance8298494B --region us-east-1 -e $exitCode || echo 'Failed to send Cloudformation Signal'\n}\ntrap exitTrap EXIT\nset -euxo pipefail\ndnf install -y aws-cfn-bootstrap\ndnf install -y docker\nsystemctl enable --now docker\ninstall -d -m 0755 /usr/libexec/docker/cli-plugins\ncurl -fsSL https://github.com/docker/compose/releases/download/v2.29.7/docker-compose-linux-aarch64 -o /usr/libexec/docker/cli-plugins/docker-compose\necho \"6e9fbd5daa20dca5d7d89145081ae8155d68ef2928b497d9f85b54fe0f9dbb2c  /usr/libexec/docker/cli-plugins/docker-compose\" | sha256sum -c -\nchmod 0755 /usr/libexec/docker/cli-plugins/docker-compose\ninstall -d -m 0755 /opt/relay\ncat <<'RELAY_COMPOSE_EOF' > /opt/relay/docker-compose.yml\n# On-instance compose unit, written to /opt/relay/docker-compose.yml by user-data.\n#\n# healthcheck, RUST_LOG, and port 8080 mirror docker/docker-compose.yml; keep both\n# in sync on edit. The differences from local dev are deliberate: the relay\n# publishes NO host ports (Caddy is the only thing on 80/443, and the relay is\n# reachable only on the compose network), the image comes from the registry\n# rather than a local build, and stop_grace_period is widened for production.\n#\n# RELAY_IMAGE and RELAY_AUTH_TOKEN come from /opt/relay/.env, written by deploy.sh\n# at deploy time. No top-level `version:` key: current Compose Specification.\n\nservices:\n  caddy:\n    # Deliberately mutable minor tag (not digest-pinned): Caddy receives security\n    # patches on each instance replacement (AMI refresh / re-deploy). Contrast\n    # with the relay image, which IS pinned — by ECR tag immutability plus\n    # same-digest promotion across environments — because that supply chain is\n    # ours to control end to end.\n    image: caddy:2\n    restart: unless-stopped\n    ports:\n      - \"80:80\"\n      - \"443:443\"\n    volumes:\n      # Caddy's default config path — no -c flag needed.\n      - /opt/relay/Caddyfile:/etc/caddy/Caddyfile:ro\n      # caddy_data holds the Let's Encrypt certificates. Persisted across\n      # container restarts, but NOT across instance replacement (residual R3,\n      # documented-accepted: LE re-issues, mind the 5-duplicate-certs/week limit).\n      - caddy_data:/data\n      - caddy_config:/config\n    depends_on:\n      - relay\n\n  relay:\n    image: ${RELAY_IMAGE:?RELAY_IMAGE must be set}\n    restart: unless-stopped\n    # The relay handles SIGINT only (STOPSIGNAL SIGINT in docker/Dockerfile.relay);\n    # 15s gives the graceful exit room to finish inside the grace window.\n    stop_grace_period: 15s\n    environment:\n      - RUST_LOG=info\n      # Fail-closed: an unset/empty RELAY_AUTH_TOKEN aborts `compose up` instead\n      # of silently starting an open relay (SC2) — collab-relay's main.rs treats\n      # an empty token as \"auth disabled\".\n      - RELAY_AUTH_TOKEN=${RELAY_AUTH_TOKEN:?RELAY_AUTH_TOKEN must be set; refusing to start an unauthenticated relay}\n      # RELAY_SUBSCRIBE_AUTHZ must stay unset/off — see the Environment variable\n      # constraint in 01-logic-design.md; it deadlocks the MLS bootstrap handshake.\n      # Admission control lives entirely in the Identify exchange.\n    healthcheck:\n      test: [\"CMD\", \"nc\", \"-z\", \"localhost\", \"8080\"]\n      interval: 10s\n      timeout: 5s\n      retries: 5\n      start_period: 5s\n\nvolumes:\n  caddy_data:\n  caddy_config:\n\nRELAY_COMPOSE_EOF\ncat <<'RELAY_CADDYFILE_EOF' > /opt/relay/Caddyfile\nrelay-dev.collab.example.com {\n\treverse_proxy relay:8080\n}\n\nRELAY_CADDYFILE_EOF\ncat <<'RELAY_DEPLOYSH_EOF' > /opt/relay/deploy.sh\n#!/usr/bin/env bash\n# /opt/relay/deploy.sh — run as root via SSM SendCommand (AWS-RunShellScript).\n# $1 is the full image ref, e.g. <acct>.dkr.ecr.<region>.amazonaws.com/obsidian-ee/collab-relay:sha-abc123def456\n# Immutable registry tags make a tag ref equivalent to a digest pin.\nset -euo pipefail\n\nIMAGE=\"${1:?usage: deploy.sh <full-image-ref>}\"\nREGION=\"us-east-1\"\nTOKEN_PARAM=\"/relay/dev/auth-token\"\n\n# Compose auto-loads .env from the project directory only. SSM RunShellScript's\n# working directory is not /opt/relay, and without this cd the ${RELAY_IMAGE} /\n# ${RELAY_AUTH_TOKEN} substitutions resolve empty; docker-compose.yml's\n# `:?` guards then abort `compose up` rather than starting an open relay (SC2).\ncd /opt/relay\n\n# Registry login. The registry host is the image ref's first path segment, so\n# this stays correct without baking the account id into the template.\naws ecr get-login-password --region \"$REGION\" \\\n  | docker login --username AWS --password-stdin \"${IMAGE%%/*}\"\n\n# The token is fetched fresh at deploy time and never lands in an image, a\n# workflow log, or a repository variable.\nRELAY_AUTH_TOKEN=$(aws ssm get-parameter --region \"$REGION\" \\\n  --name \"$TOKEN_PARAM\" --with-decryption --query Parameter.Value --output text)\n\n[ -n \"${RELAY_AUTH_TOKEN}\" ] || { echo \"FATAL: empty auth token; refusing to start an unauthenticated relay\" >&2; exit 1; }\n\numask 077\ncat > /opt/relay/.env <<EOF\nRELAY_IMAGE=${IMAGE}\nRELAY_AUTH_TOKEN=${RELAY_AUTH_TOKEN}\nEOF\n\n# Length only — never echo the value.\necho \"auth token loaded (${#RELAY_AUTH_TOKEN} chars)\"\n\ndocker compose pull relay\ndocker compose up -d\ndocker image prune -af\n\necho \"deployed ${IMAGE}\"\n\nRELAY_DEPLOYSH_EOF\nchmod 0755 /opt/relay/deploy.sh"
          }
        },
        "DependsOn": [
          "RelayInstanceRoleDefaultPolicy3A2ED09E",
          "RelayInstanceRoleBB4CA3BD"
        ],
        "CreationPolicy": {
          "ResourceSignal": {
            "Count": 1,
            "Timeout": "PT15M"
          }
        }
      },
      "RelayInstanceLaunchTemplateCDBD5993": {
        "Type": "AWS::EC2::LaunchTemplate",
        "Properties": {
          "LaunchTemplateData": {
            "MetadataOptions": {
              "HttpTokens": "required"
            }
          },
          "LaunchTemplateName": "RelaydevRelayInstanceLaunchTemplate154AAAB8"
        }
      },
      "RelayEip": {
        "Type": "AWS::EC2::EIP",
        "Properties": {
          "Domain": "vpc"
        }
      },
      "RelayEipAssociation": {
        "Type": "AWS::EC2::EIPAssociation",
        "Properties": {
          "AllocationId": {
            "Fn::GetAtt": [
              "RelayEip",
              "AllocationId"
            ]
          },
          "InstanceId": {
            "Ref": "RelayInstance8298494B"
          }
        }
      },
      "RelayDnsED2062AD": {
        "Type": "AWS::Route53::RecordSet",
        "Properties": {
          "HostedZoneId": {
            "Fn::ImportValue": "RelayShared:ExportsOutputRefCollabZoneC5E0737B29A41A37"
          },
          "Name": "relay-dev.collab.example.com.",
          "ResourceRecords": [
            {
              "Fn::GetAtt": [
                "RelayEip",
                "PublicIp"
              ]
            }
          ],
          "TTL": "300",
          "Type": "A"
        }
      }
    },
    "Parameters": {
      "SsmParameterValueawsserviceamiamazonlinuxlatestal2023amikernel61arm64C96584B6F00A464EAD1953AFF4B05118Parameter": {
        "Type": "AWS::SSM::Parameter::Value<AWS::EC2::Image::Id>",
        "Default": "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-6.1-arm64"
      },
      "BootstrapVersion": {
        "Type": "AWS::SSM::Parameter::Value<String>",
        "Default": "/cdk-bootstrap/hnb659fds/version",
        "Description": "Version of the CDK Bootstrap resources in this environment, automatically retrieved from SSM Parameter Store. [cdk:skip]"
      }
    },
    "Outputs": {
      "InstanceId": {
        "Value": {
          "Ref": "RelayInstance8298494B"
        }
      },
      "Hostname": {
        "Value": "relay-dev.collab.example.com"
      }
    },
    "Rules": {
      "CheckBootstrapVersion": {
        "Assertions": [
          {
            "Assert": {
              "Fn::Not": [
                {
                  "Fn::Contains": [
                    [
                      "1",
                      "2",
                      "3",
                      "4",
                      "5"
                    ],
                    {
                      "Ref": "BootstrapVersion"
                    }
                  ]
                }
              ]
            },
            "AssertDescription": "CDK bootstrap stack version 6 required. Please run 'cdk bootstrap' with a recent version of the CDK CLI."
          }
        ]
      }
    }
  },
  "Relay-staging": {
    "Description": "Relay staging: one t4g.micro instance behind Caddy",
    "Resources": {
      "RelaySg14484F35": {
        "Type": "AWS::EC2::SecurityGroup",
        "Properties": {
          "GroupDescription": "Relay staging: web ports only",
          "SecurityGroupEgress": [
            {
              "CidrIp": "0.0.0.0/0",
              "Description": "Allow all outbound traffic by default",
              "IpProtocol": "-1"
            }
          ],
          "SecurityGroupIngress": [
            {
              "CidrIp": "0.0.0.0/0",
              "Description": "ACME HTTP challenge and redirect",
              "FromPort": 80,
              "IpProtocol": "tcp",
              "ToPort": 80
            },
            {
              "CidrIp": "0.0.0.0/0",
              "Description": "TLS / WebSocket",
              "FromPort": 443,
              "IpProtocol": "tcp",
              "ToPort": 443
            }
          ],
          "VpcId": "vpc-00000000000000000"
        }
      },
      "RelayInstanceRoleBB4CA3BD": {
        "Type": "AWS::IAM::Role",
        "Properties": {
          "AssumeRolePolicyDocument": {
            "Statement": [
              {
                "Action": "sts:AssumeRole",
                "Effect": "Allow",
                "Principal": {
                  "Service": "ec2.amazonaws.com"
                }
              }
            ],
            "Version": "2012-10-17"
          },
          "Description": "Relay staging instance: SSM agent channel, registry pull, own token only"
        }
      },
      "RelayInstanceRoleDefaultPolicy3A2ED09E": {
        "Type": "AWS::IAM::Policy",
        "Properties": {
          "PolicyDocument": {
            "Statement": [
              {
                "Action": [
                  "ec2messages:AcknowledgeMessage",
                  "ec2messages:DeleteMessage",
                  "ec2messages:FailMessage",
                  "ec2messages:GetEndpoint",
                  "ec2messages:GetMessages",
                  "ec2messages:SendReply",
                  "ecr:GetAuthorizationToken",
                  "ssm:DescribeAssociation",
                  "ssm:DescribeDocument",
                  "ssm:GetDeployablePatchSnapshotForInstance",
                  "ssm:GetDocument",
                  "ssm:GetManifest",
                  "ssm:ListAssociations",
                  "ssm:ListInstanceAssociations",
                  "ssm:PutComplianceItems",
                  "ssm:PutInventory",
                  "ssm:UpdateInstanceAssociationStatus",
                  "ssm:UpdateInstanceInformation",
                  "ssmmessages:CreateControlChannel",
                  "ssmmessages:CreateDataChannel",
                  "ssmmessages:OpenControlChannel",
                  "ssmmessages:OpenDataChannel"
                ],
                "Effect": "Allow",
                "Resource": "*"
              },
              {
                "Action": [
                  "ecr:BatchCheckLayerAvailability",
                  "ecr:BatchGetImage",
                  "ecr:GetDownloadUrlForLayer"
                ],
                "Effect": "Allow",
                "Resource": {
                  "Fn::ImportValue": "RelayShared:ExportsOutputFnGetAttRelayRepo971E060DArn3C32FA8A"
                }
              },
              {
                "Action": "ssm:GetParameter",
                "Effect": "Allow",
                "Resource": "arn:aws:ssm:us-east-1:111111111111:parameter/relay/staging/auth-token"
              }
            ],
            "Version": "2012-10-17"
          },
          "PolicyName": "RelayInstanceRoleDefaultPolicy3A2ED09E",
          "Roles": [
            {
              "Ref": "RelayInstanceRoleBB4CA3BD"
            }
          ]
        }
      },
      "RelayInstanceInstanceProfileDE8E6059": {
        "Type": "AWS::IAM::InstanceProfile",
        "Properties": {
          "Roles": [
            {
              "Ref": "RelayInstanceRoleBB4CA3BD"
            }
          ]
        }
      },
      "RelayInstance8298494B": {
        "Type": "AWS::EC2::Instance",
        "Properties": {
          "AvailabilityZone": "us-east-1a",
          "BlockDeviceMappings": [
            {
              "DeviceName": "/dev/xvda",
              "Ebs": {
                "Encrypted": true,
                "VolumeSize": 8,
                "VolumeType": "gp3"
              }
            }
          ],
          "IamInstanceProfile": {
            "Ref": "RelayInstanceInstanceProfileDE8E6059"
          },
          "ImageId": {
            "Ref": "SsmParameterValueawsserviceamiamazonlinuxlatestal2023amikernel61arm64C96584B6F00A464EAD1953AFF4B05118Parameter"
          },
          "InstanceType": "t4g.micro",
          "LaunchTemplate": {
            "LaunchTemplateName": "RelaystagingRelayInstanceLaunchTemplate20D419AC",
            "Version": {
              "Fn::GetAtt": [
                "RelayInstanceLaunchTemplateCDBD5993",
                "LatestVersionNumber"
              ]
            }
          },
          "SecurityGroupIds": [
            {
              "Fn::GetAtt": [
                "RelaySg14484F35",
                "GroupId"
              ]
            }
          ],
          "SubnetId": "subnet-00000000000000000",
          "Tags": [
            {
              "Key": "Name",
              "Value": "Relay-staging/RelayInstance"
            },
            {
              "Key": "RelayEnv",
              "Value": "staging"
            }
          ],
          "UserData": {
            "Fn::Base64": "#!/bin/bash\nfunction exitTrap(){\nexitCode=$?\n/opt/aws/bin/cfn-signal --stack Relay-staging --resource RelayInstance8298494B --region us-east-1 -e $exitCode || echo 'Failed to send Cloudformation Signal'\n}\ntrap exitTrap EXIT\nset -euxo pipefail\ndnf install -y aws-cfn-bootstrap\ndnf install -y docker\nsystemctl enable --now docker\ninstall -d -m 0755 /usr/libexec/docker/cli-plugins\ncurl -fsSL https://github.com/docker/compose/releases/download/v2.29.7/docker-compose-linux-aarch64 -o /usr/libexec/docker/cli-plugins/docker-compose\necho \"6e9fbd5daa20dca5d7d89145081ae8155d68ef2928b497d9f85b54fe0f9dbb2c  /usr/libexec/docker/cli-plugins/docker-compose\" | sha256sum -c -\nchmod 0755 /usr/libexec/docker/cli-plugins/docker-compose\ninstall -d -m 0755 /opt/relay\ncat <<'RELAY_COMPOSE_EOF' > /opt/relay/docker-compose.yml\n# On-instance compose unit, written to /opt/relay/docker-compose.yml by user-data.\n#\n# healthcheck, RUST_LOG, and port 8080 mirror docker/docker-compose.yml; keep both\n# in sync on edit. The differences from local dev are deliberate: the relay\n# publishes NO host ports (Caddy is the only thing on 80/443, and the relay is\n# reachable only on the compose network), the image comes from the registry\n# rather than a local build, and stop_grace_period is widened for production.\n#\n# RELAY_IMAGE and RELAY_AUTH_TOKEN come from /opt/relay/.env, written by deploy.sh\n# at deploy time. No top-level `version:` key: current Compose Specification.\n\nservices:\n  caddy:\n    # Deliberately mutable minor tag (not digest-pinned): Caddy receives security\n    # patches on each instance replacement (AMI refresh / re-deploy). Contrast\n    # with the relay image, which IS pinned — by ECR tag immutability plus\n    # same-digest promotion across environments — because that supply chain is\n    # ours to control end to end.\n    image: caddy:2\n    restart: unless-stopped\n    ports:\n      - \"80:80\"\n      - \"443:443\"\n    volumes:\n      # Caddy's default config path — no -c flag needed.\n      - /opt/relay/Caddyfile:/etc/caddy/Caddyfile:ro\n      # caddy_data holds the Let's Encrypt certificates. Persisted across\n      # container restarts, but NOT across instance replacement (residual R3,\n      # documented-accepted: LE re-issues, mind the 5-duplicate-certs/week limit).\n      - caddy_data:/data\n      - caddy_config:/config\n    depends_on:\n      - relay\n\n  relay:\n    image: ${RELAY_IMAGE:?RELAY_IMAGE must be set}\n    restart: unless-stopped\n    # The relay handles SIGINT only (STOPSIGNAL SIGINT in docker/Dockerfile.relay);\n    # 15s gives the graceful exit room to finish inside the grace window.\n    stop_grace_period: 15s\n    environment:\n      - RUST_LOG=info\n      # Fail-closed: an unset/empty RELAY_AUTH_TOKEN aborts `compose up` instead\n      # of silently starting an open relay (SC2) — collab-relay's main.rs treats\n      # an empty token as \"auth disabled\".\n      - RELAY_AUTH_TOKEN=${RELAY_AUTH_TOKEN:?RELAY_AUTH_TOKEN must be set; refusing to start an unauthenticated relay}\n      # RELAY_SUBSCRIBE_AUTHZ must stay unset/off — see the Environment variable\n      # constraint in 01-logic-design.md; it deadlocks the MLS bootstrap handshake.\n      # Admission control lives entirely in the Identify exchange.\n    healthcheck:\n      test: [\"CMD\", \"nc\", \"-z\", \"localhost\", \"8080\"]\n      interval: 10s\n      timeout: 5s\n      retries: 5\n      start_period: 5s\n\nvolumes:\n  caddy_data:\n  caddy_config:\n\nRELAY_COMPOSE_EOF\ncat <<'RELAY_CADDYFILE_EOF' > /opt/relay/Caddyfile\nrelay-staging.collab.example.com {\n\treverse_proxy relay:8080\n}\n\nRELAY_CADDYFILE_EOF\ncat <<'RELAY_DEPLOYSH_EOF' > /opt/relay/deploy.sh\n#!/usr/bin/env bash\n# /opt/relay/deploy.sh — run as root via SSM SendCommand (AWS-RunShellScript).\n# $1 is the full image ref, e.g. <acct>.dkr.ecr.<region>.amazonaws.com/obsidian-ee/collab-relay:sha-abc123def456\n# Immutable registry tags make a tag ref equivalent to a digest pin.\nset -euo pipefail\n\nIMAGE=\"${1:?usage: deploy.sh <full-image-ref>}\"\nREGION=\"us-east-1\"\nTOKEN_PARAM=\"/relay/staging/auth-token\"\n\n# Compose auto-loads .env from the project directory only. SSM RunShellScript's\n# working directory is not /opt/relay, and without this cd the ${RELAY_IMAGE} /\n# ${RELAY_AUTH_TOKEN} substitutions resolve empty; docker-compose.yml's\n# `:?` guards then abort `compose up` rather than starting an open relay (SC2).\ncd /opt/relay\n\n# Registry login. The registry host is the image ref's first path segment, so\n# this stays correct without baking the account id into the template.\naws ecr get-login-password --region \"$REGION\" \\\n  | docker login --username AWS --password-stdin \"${IMAGE%%/*}\"\n\n# The token is fetched fresh at deploy time and never lands in an image, a\n# workflow log, or a repository variable.\nRELAY_AUTH_TOKEN=$(aws ssm get-parameter --region \"$REGION\" \\\n  --name \"$TOKEN_PARAM\" --with-decryption --query Parameter.Value --output text)\n\n[ -n \"${RELAY_AUTH_TOKEN}\" ] || { echo \"FATAL: empty auth token; refusing to start an unauthenticated relay\" >&2; exit 1; }\n\numask 077\ncat > /opt/relay/.env <<EOF\nRELAY_IMAGE=${IMAGE}\nRELAY_AUTH_TOKEN=${RELAY_AUTH_TOKEN}\nEOF\n\n# Length only — never echo the value.\necho \"auth token loaded (${#RELAY_AUTH_TOKEN} chars)\"\n\ndocker compose pull relay\ndocker compose up -d\ndocker image prune -af\n\necho \"deployed ${IMAGE}\"\n\nRELAY_DEPLOYSH_EOF\nchmod 0755 /opt/relay/deploy.sh"
          }
        },
        "DependsOn": [
          "RelayInstanceRoleDefaultPolicy3A2ED09E",
          "RelayInstanceRoleBB4CA3BD"
        ],
        "CreationPolicy": {
          "ResourceSignal": {
            "Count": 1,
            "Timeout": "PT15M"
          }
        }
      },
      "RelayInstanceLaunchTemplateCDBD5993": {
        "Type": "AWS::EC2::LaunchTemplate",
        "Properties": {
          "LaunchTemplateData": {
            "MetadataOptions": {
              "HttpTokens": "required"
            }
          },
          "LaunchTemplateName": "RelaystagingRelayInstanceLaunchTemplate20D419AC"
        }
      },
      "RelayEip": {
        "Type": "AWS::EC2::EIP",
        "Properties": {
          "Domain": "vpc"
        }
      },
      "RelayEipAssociation": {
        "Type": "AWS::EC2::EIPAssociation",
        "Properties": {
          "AllocationId": {
            "Fn::GetAtt": [
              "RelayEip",
              "AllocationId"
            ]
          },
          "InstanceId": {
            "Ref": "RelayInstance8298494B"
          }
        }
      },
      "RelayDnsED2062AD": {
        "Type": "AWS::Route53::RecordSet",
        "Properties": {
          "HostedZoneId": {
            "Fn::ImportValue": "RelayShared:ExportsOutputRefCollabZoneC5E0737B29A41A37"
          },
          "Name": "relay-staging.collab.example.com.",
          "ResourceRecords": [
            {
              "Fn::GetAtt": [
                "RelayEip",
                "PublicIp"
              ]
            }
          ],
          "TTL": "300",
          "Type": "A"
        }
      }
    },
    "Parameters": {
      "SsmParameterValueawsserviceamiamazonlinuxlatestal2023amikernel61arm64C96584B6F00A464EAD1953AFF4B05118Parameter": {
        "Type": "AWS::SSM::Parameter::Value<AWS::EC2::Image::Id>",
        "Default": "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-6.1-arm64"
      },
      "BootstrapVersion": {
        "Type": "AWS::SSM::Parameter::Value<String>",
        "Default": "/cdk-bootstrap/hnb659fds/version",
        "Description": "Version of the CDK Bootstrap resources in this environment, automatically retrieved from SSM Parameter Store. [cdk:skip]"
      }
    },
    "Outputs": {
      "InstanceId": {
        "Value": {
          "Ref": "RelayInstance8298494B"
        }
      },
      "Hostname": {
        "Value": "relay-staging.collab.example.com"
      }
    },
    "Rules": {
      "CheckBootstrapVersion": {
        "Assertions": [
          {
            "Assert": {
              "Fn::Not": [
                {
                  "Fn::Contains": [
                    [
                      "1",
                      "2",
                      "3",
                      "4",
                      "5"
                    ],
                    {
                      "Ref": "BootstrapVersion"
                    }
                  ]
                }
              ]
            },
            "AssertDescription": "CDK bootstrap stack version 6 required. Please run 'cdk bootstrap' with a recent version of the CDK CLI."
          }
        ]
      }
    }
  },
  "Relay-prod": {
    "Description": "Relay prod: one t4g.micro instance behind Caddy",
    "Resources": {
      "RelaySg14484F35": {
        "Type": "AWS::EC2::SecurityGroup",
        "Properties": {
          "GroupDescription": "Relay prod: web ports only",
          "SecurityGroupEgress": [
            {
              "CidrIp": "0.0.0.0/0",
              "Description": "Allow all outbound traffic by default",
              "IpProtocol": "-1"
            }
          ],
          "SecurityGroupIngress": [
            {
              "CidrIp": "0.0.0.0/0",
              "Description": "ACME HTTP challenge and redirect",
              "FromPort": 80,
              "IpProtocol": "tcp",
              "ToPort": 80
            },
            {
              "CidrIp": "0.0.0.0/0",
              "Description": "TLS / WebSocket",
              "FromPort": 443,
              "IpProtocol": "tcp",
              "ToPort": 443
            }
          ],
          "VpcId": "vpc-00000000000000000"
        }
      },
      "RelayInstanceRoleBB4CA3BD": {
        "Type": "AWS::IAM::Role",
        "Properties": {
          "AssumeRolePolicyDocument": {
            "Statement": [
              {
                "Action": "sts:AssumeRole",
                "Effect": "Allow",
                "Principal": {
                  "Service": "ec2.amazonaws.com"
                }
              }
            ],
            "Version": "2012-10-17"
          },
          "Description": "Relay prod instance: SSM agent channel, registry pull, own token only"
        }
      },
      "RelayInstanceRoleDefaultPolicy3A2ED09E": {
        "Type": "AWS::IAM::Policy",
        "Properties": {
          "PolicyDocument": {
            "Statement": [
              {
                "Action": [
                  "ec2messages:AcknowledgeMessage",
                  "ec2messages:DeleteMessage",
                  "ec2messages:FailMessage",
                  "ec2messages:GetEndpoint",
                  "ec2messages:GetMessages",
                  "ec2messages:SendReply",
                  "ecr:GetAuthorizationToken",
                  "ssm:DescribeAssociation",
                  "ssm:DescribeDocument",
                  "ssm:GetDeployablePatchSnapshotForInstance",
                  "ssm:GetDocument",
                  "ssm:GetManifest",
                  "ssm:ListAssociations",
                  "ssm:ListInstanceAssociations",
                  "ssm:PutComplianceItems",
                  "ssm:PutInventory",
                  "ssm:UpdateInstanceAssociationStatus",
                  "ssm:UpdateInstanceInformation",
                  "ssmmessages:CreateControlChannel",
                  "ssmmessages:CreateDataChannel",
                  "ssmmessages:OpenControlChannel",
                  "ssmmessages:OpenDataChannel"
                ],
                "Effect": "Allow",
                "Resource": "*"
              },
              {
                "Action": [
                  "ecr:BatchCheckLayerAvailability",
                  "ecr:BatchGetImage",
                  "ecr:GetDownloadUrlForLayer"
                ],
                "Effect": "Allow",
                "Resource": {
                  "Fn::ImportValue": "RelayShared:ExportsOutputFnGetAttRelayRepo971E060DArn3C32FA8A"
                }
              },
              {
                "Action": "ssm:GetParameter",
                "Effect": "Allow",
                "Resource": "arn:aws:ssm:us-east-1:111111111111:parameter/relay/prod/auth-token"
              }
            ],
            "Version": "2012-10-17"
          },
          "PolicyName": "RelayInstanceRoleDefaultPolicy3A2ED09E",
          "Roles": [
            {
              "Ref": "RelayInstanceRoleBB4CA3BD"
            }
          ]
        }
      },
      "RelayInstanceInstanceProfileDE8E6059": {
        "Type": "AWS::IAM::InstanceProfile",
        "Properties": {
          "Roles": [
            {
              "Ref": "RelayInstanceRoleBB4CA3BD"
            }
          ]
        }
      },
      "RelayInstance8298494B": {
        "Type": "AWS::EC2::Instance",
        "Properties": {
          "AvailabilityZone": "us-east-1a",
          "BlockDeviceMappings": [
            {
              "DeviceName": "/dev/xvda",
              "Ebs": {
                "Encrypted": true,
                "VolumeSize": 8,
                "VolumeType": "gp3"
              }
            }
          ],
          "IamInstanceProfile": {
            "Ref": "RelayInstanceInstanceProfileDE8E6059"
          },
          "ImageId": {
            "Ref": "SsmParameterValueawsserviceamiamazonlinuxlatestal2023amikernel61arm64C96584B6F00A464EAD1953AFF4B05118Parameter"
          },
          "InstanceType": "t4g.micro",
          "LaunchTemplate": {
            "LaunchTemplateName": "RelayprodRelayInstanceLaunchTemplateAF023CE9",
            "Version": {
              "Fn::GetAtt": [
                "RelayInstanceLaunchTemplateCDBD5993",
                "LatestVersionNumber"
              ]
            }
          },
          "SecurityGroupIds": [
            {
              "Fn::GetAtt": [
                "RelaySg14484F35",
                "GroupId"
              ]
            }
          ],
          "SubnetId": "subnet-00000000000000000",
          "Tags": [
            {
              "Key": "Name",
              "Value": "Relay-prod/RelayInstance"
            },
            {
              "Key": "RelayEnv",
              "Value": "prod"
            }
          ],
          "UserData": {
            "Fn::Base64": "#!/bin/bash\nfunction exitTrap(){\nexitCode=$?\n/opt/aws/bin/cfn-signal --stack Relay-prod --resource RelayInstance8298494B --region us-east-1 -e $exitCode || echo 'Failed to send Cloudformation Signal'\n}\ntrap exitTrap EXIT\nset -euxo pipefail\ndnf install -y aws-cfn-bootstrap\ndnf install -y docker\nsystemctl enable --now docker\ninstall -d -m 0755 /usr/libexec/docker/cli-plugins\ncurl -fsSL https://github.com/docker/compose/releases/download/v2.29.7/docker-compose-linux-aarch64 -o /usr/libexec/docker/cli-plugins/docker-compose\necho \"6e9fbd5daa20dca5d7d89145081ae8155d68ef2928b497d9f85b54fe0f9dbb2c  /usr/libexec/docker/cli-plugins/docker-compose\" | sha256sum -c -\nchmod 0755 /usr/libexec/docker/cli-plugins/docker-compose\ninstall -d -m 0755 /opt/relay\ncat <<'RELAY_COMPOSE_EOF' > /opt/relay/docker-compose.yml\n# On-instance compose unit, written to /opt/relay/docker-compose.yml by user-data.\n#\n# healthcheck, RUST_LOG, and port 8080 mirror docker/docker-compose.yml; keep both\n# in sync on edit. The differences from local dev are deliberate: the relay\n# publishes NO host ports (Caddy is the only thing on 80/443, and the relay is\n# reachable only on the compose network), the image comes from the registry\n# rather than a local build, and stop_grace_period is widened for production.\n#\n# RELAY_IMAGE and RELAY_AUTH_TOKEN come from /opt/relay/.env, written by deploy.sh\n# at deploy time. No top-level `version:` key: current Compose Specification.\n\nservices:\n  caddy:\n    # Deliberately mutable minor tag (not digest-pinned): Caddy receives security\n    # patches on each instance replacement (AMI refresh / re-deploy). Contrast\n    # with the relay image, which IS pinned — by ECR tag immutability plus\n    # same-digest promotion across environments — because that supply chain is\n    # ours to control end to end.\n    image: caddy:2\n    restart: unless-stopped\n    ports:\n      - \"80:80\"\n      - \"443:443\"\n    volumes:\n      # Caddy's default config path — no -c flag needed.\n      - /opt/relay/Caddyfile:/etc/caddy/Caddyfile:ro\n      # caddy_data holds the Let's Encrypt certificates. Persisted across\n      # container restarts, but NOT across instance replacement (residual R3,\n      # documented-accepted: LE re-issues, mind the 5-duplicate-certs/week limit).\n      - caddy_data:/data\n      - caddy_config:/config\n    depends_on:\n      - relay\n\n  relay:\n    image: ${RELAY_IMAGE:?RELAY_IMAGE must be set}\n    restart: unless-stopped\n    # The relay handles SIGINT only (STOPSIGNAL SIGINT in docker/Dockerfile.relay);\n    # 15s gives the graceful exit room to finish inside the grace window.\n    stop_grace_period: 15s\n    environment:\n      - RUST_LOG=info\n      # Fail-closed: an unset/empty RELAY_AUTH_TOKEN aborts `compose up` instead\n      # of silently starting an open relay (SC2) — collab-relay's main.rs treats\n      # an empty token as \"auth disabled\".\n      - RELAY_AUTH_TOKEN=${RELAY_AUTH_TOKEN:?RELAY_AUTH_TOKEN must be set; refusing to start an unauthenticated relay}\n      # RELAY_SUBSCRIBE_AUTHZ must stay unset/off — see the Environment variable\n      # constraint in 01-logic-design.md; it deadlocks the MLS bootstrap handshake.\n      # Admission control lives entirely in the Identify exchange.\n    healthcheck:\n      test: [\"CMD\", \"nc\", \"-z\", \"localhost\", \"8080\"]\n      interval: 10s\n      timeout: 5s\n      retries: 5\n      start_period: 5s\n\nvolumes:\n  caddy_data:\n  caddy_config:\n\nRELAY_COMPOSE_EOF\ncat <<'RELAY_CADDYFILE_EOF' > /opt/relay/Caddyfile\nrelay-prod.collab.example.com {\n\treverse_proxy relay:8080\n}\n\nRELAY_CADDYFILE_EOF\ncat <<'RELAY_DEPLOYSH_EOF' > /opt/relay/deploy.sh\n#!/usr/bin/env bash\n# /opt/relay/deploy.sh — run as root via SSM SendCommand (AWS-RunShellScript).\n# $1 is the full image ref, e.g. <acct>.dkr.ecr.<region>.amazonaws.com/obsidian-ee/collab-relay:sha-abc123def456\n# Immutable registry tags make a tag ref equivalent to a digest pin.\nset -euo pipefail\n\nIMAGE=\"${1:?usage: deploy.sh <full-image-ref>}\"\nREGION=\"us-east-1\"\nTOKEN_PARAM=\"/relay/prod/auth-token\"\n\n# Compose auto-loads .env from the project directory only. SSM RunShellScript's\n# working directory is not /opt/relay, and without this cd the ${RELAY_IMAGE} /\n# ${RELAY_AUTH_TOKEN} substitutions resolve empty; docker-compose.yml's\n# `:?` guards then abort `compose up` rather than starting an open relay (SC2).\ncd /opt/relay\n\n# Registry login. The registry host is the image ref's first path segment, so\n# this stays correct without baking the account id into the template.\naws ecr get-login-password --region \"$REGION\" \\\n  | docker login --username AWS --password-stdin \"${IMAGE%%/*}\"\n\n# The token is fetched fresh at deploy time and never lands in an image, a\n# workflow log, or a repository variable.\nRELAY_AUTH_TOKEN=$(aws ssm get-parameter --region \"$REGION\" \\\n  --name \"$TOKEN_PARAM\" --with-decryption --query Parameter.Value --output text)\n\n[ -n \"${RELAY_AUTH_TOKEN}\" ] || { echo \"FATAL: empty auth token; refusing to start an unauthenticated relay\" >&2; exit 1; }\n\numask 077\ncat > /opt/relay/.env <<EOF\nRELAY_IMAGE=${IMAGE}\nRELAY_AUTH_TOKEN=${RELAY_AUTH_TOKEN}\nEOF\n\n# Length only — never echo the value.\necho \"auth token loaded (${#RELAY_AUTH_TOKEN} chars)\"\n\ndocker compose pull relay\ndocker compose up -d\ndocker image prune -af\n\necho \"deployed ${IMAGE}\"\n\nRELAY_DEPLOYSH_EOF\nchmod 0755 /opt/relay/deploy.sh"
          }
        },
        "DependsOn": [
          "RelayInstanceRoleDefaultPolicy3A2ED09E",
          "RelayInstanceRoleBB4CA3BD"
        ],
        "CreationPolicy": {
          "ResourceSignal": {
            "Count": 1,
            "Timeout": "PT15M"
          }
        }
      },
      "RelayInstanceLaunchTemplateCDBD5993": {
        "Type": "AWS::EC2::LaunchTemplate",
        "Properties": {
          "LaunchTemplateData": {
            "MetadataOptions": {
              "HttpTokens": "required"
            }
          },
          "LaunchTemplateName": "RelayprodRelayInstanceLaunchTemplateAF023CE9"
        }
      },
      "RelayEip": {
        "Type": "AWS::EC2::EIP",
        "Properties": {
          "Domain": "vpc"
        }
      },
      "RelayEipAssociation": {
        "Type": "AWS::EC2::EIPAssociation",
        "Properties": {
          "AllocationId": {
            "Fn::GetAtt": [
              "RelayEip",
              "AllocationId"
            ]
          },
          "InstanceId": {
            "Ref": "RelayInstance8298494B"
          }
        }
      },
      "RelayDnsED2062AD": {
        "Type": "AWS::Route53::RecordSet",
        "Properties": {
          "HostedZoneId": {
            "Fn::ImportValue": "RelayShared:ExportsOutputRefCollabZoneC5E0737B29A41A37"
          },
          "Name": "relay-prod.collab.example.com.",
          "ResourceRecords": [
            {
              "Fn::GetAtt": [
                "RelayEip",
                "PublicIp"
              ]
            }
          ],
          "TTL": "300",
          "Type": "A"
        }
      }
    },
    "Parameters": {
      "SsmParameterValueawsserviceamiamazonlinuxlatestal2023amikernel61arm64C96584B6F00A464EAD1953AFF4B05118Parameter": {
        "Type": "AWS::SSM::Parameter::Value<AWS::EC2::Image::Id>",
        "Default": "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-6.1-arm64"
      },
      "BootstrapVersion": {
        "Type": "AWS::SSM::Parameter::Value<String>",
        "Default": "/cdk-bootstrap/hnb659fds/version",
        "Description": "Version of the CDK Bootstrap resources in this environment, automatically retrieved from SSM Parameter Store. [cdk:skip]"
      }
    },
    "Outputs": {
      "InstanceId": {
        "Value": {
          "Ref": "RelayInstance8298494B"
        }
      },
      "Hostname": {
        "Value": "relay-prod.collab.example.com"
      }
    },
    "Rules": {
      "CheckBootstrapVersion": {
        "Assertions": [
          {
            "Assert": {
              "Fn::Not": [
                {
                  "Fn::Contains": [
                    [
                      "1",
                      "2",
                      "3",
                      "4",
                      "5"
                    ],
                    {
                      "Ref": "BootstrapVersion"
                    }
                  ]
                }
              ]
            },
            "AssertDescription": "CDK bootstrap stack version 6 required. Please run 'cdk bootstrap' with a recent version of the CDK CLI."
          }
        ]
      }
    }
  }
};
