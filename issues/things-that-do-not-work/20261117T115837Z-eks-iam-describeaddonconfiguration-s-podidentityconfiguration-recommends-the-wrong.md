Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: eks__iam.cloudwatch-observability-recommended-policy-resolves-in-iam eks__iam.ebs-csi-driver-recommended-policy-resolves-in-iam
Issue-Key: eks__iam.cloudwatch-observability-recommended-policy-resolves-in-iam|run#5|AssertionError|assert len(arns) == 1 and arns[0].startswith("arn:aws:iam::aws:policy/"), (
Issue-Key: eks__iam.ebs-csi-driver-recommended-policy-resolves-in-iam|run#7|AssertionError|assert arn == "arn:aws:iam::aws:policy/AmazonEBSCSIDriverPolicyV2", (
Issue-Commit: 384e9c06f8b850c3e6c1129231f41c38ae5339ba

# eks__iam: DescribeAddonConfiguration's podIdentityConfiguration recommends the wrong managed policies — two policies for the CloudWatch agent and the legacy service-role EBS CSI policy

> _Note: this issue was identified against the code base at commit `384e9c06f8b850c3e6c1129231f41c38ae5339ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAddonConfiguration

## Description
Two net-new root causes in the add-on catalog's pod-identity recommendations served by DescribeAddonConfiguration.

Cause 1 of 2: the amazon-cloudwatch-observability add-on's cloudwatch-agent service account is given two recommended managed policies instead of the single one EKS recommends.
AWS: Smithy — DescribeAddonConfiguration returns podIdentityConfiguration, a list of {serviceAccount, recommendedManagedPolicies}. The model does not enumerate the values, so: recorded behavior only — for the default build of amazon-cloudwatch-observability on the account's default Kubernetes release, podIdentityConfiguration names exactly one service account, 'cloudwatch-agent', whose recommendedManagedPolicies is exactly one AWS-managed policy ARN, 'arn:aws:iam::aws:policy/CloudWatchAgentServerPolicy', and that ARN resolves in IAM as attachable with the agent's metric, log and trace actions (cloudwatch:PutMetricData, logs:PutLogEvents, logs:CreateLogGroup, xray:PutTraceSegments).
Emulator: recommendedManagedPolicies for cloudwatch-agent is ['arn:aws:iam::aws:policy/CloudWatchAgentServerPolicy', 'arn:aws:iam::aws:policy/AWSXrayWriteOnlyAccess'] — a second, extra ARN.
Why: a client that provisions the add-on's pod identity by attaching every recommended policy attaches a policy EKS does not recommend, so the IAM surface it builds (and any least-privilege review of it) diverges from AWS; a client asserting a single recommendation breaks.
Fix: the add-on catalog data behind DescribeAddonConfiguration in the eks service crate (services/service-com-amazonaws-eks-awswesleyfrontend/src/ops/describe_addon_configuration.rs and the catalog it reads) — serve the one recommended ARN AWS publishes for this build. Related but distinct open issues: 20261116T155158Z-eks-describeaddonconfiguration-ignores-addonversion-and-serves-one-synthesised.md and 20261114T010332Z-eks-read-only-catalog-listings-serve-synthesised-add.md describe the synthesised schema/version behavior generally; neither names these recommendedManagedPolicies values.
Traces: eks__iam.cloudwatch-observability-recommended-policy-resolves-in-iam.

Cause 2 of 2: the aws-ebs-csi-driver add-on recommends the legacy service-role-path policy instead of the current root-path V2 policy.
AWS: recorded behavior only (the Smithy model does not enumerate recommendedManagedPolicies) — for the default build of aws-ebs-csi-driver on the account's default Kubernetes release, podIdentityConfiguration names exactly one service account, 'ebs-csi-controller-sa', whose single recommendedManagedPolicies entry is 'arn:aws:iam::aws:policy/AmazonEBSCSIDriverPolicyV2' — at the root path '/', not the legacy service-role path — and that policy resolves in IAM as attachable at Path '/' and grants ec2:CreateVolume, ec2:DeleteVolume, ec2:AttachVolume, ec2:DetachVolume, ec2:CreateSnapshot, ec2:DeleteSnapshot.
Emulator: recommendedManagedPolicies is ['arn:aws:iam::aws:policy/service-role/AmazonEBSCSIDriverPolicy'] — the superseded policy, at the service-role path.
Why: a client wiring EBS CSI pod identity from the catalog attaches a different policy than on AWS, at a different ARN path, so attach/detach bookkeeping, path assertions and policy-document checks all diverge from the real service.
Fix: same component — the add-on catalog's podIdentityConfiguration entries behind DescribeAddonConfiguration in the eks service crate; serve the V2 root-path ARN for the default aws-ebs-csi-driver build (and ensure that ARN resolves in the IAM emulator's managed-policy catalog with the EBS volume-lifecycle actions).
Traces: eks__iam.ebs-csi-driver-recommended-policy-resolves-in-iam.

## Evidence
Cause 1 — eks__iam.cloudwatch-observability-recommended-policy-resolves-in-iam:
  AssertionError: cloudwatch-agent recommends ['arn:aws:iam::aws:policy/CloudWatchAgentServerPolicy', 'arn:aws:iam::aws:policy/AWSXrayWriteOnlyAccess'], expected exactly one AWS-managed policy ARN
  expected: recommendedManagedPolicies == ['arn:aws:iam::aws:policy/CloudWatchAgentServerPolicy']

Cause 2 — eks__iam.ebs-csi-driver-recommended-policy-resolves-in-iam:
  AssertionError: the controller's recommended policy ARN is 'arn:aws:iam::aws:policy/service-role/AmazonEBSCSIDriverPolicy'
  expected: 'arn:aws:iam::aws:policy/AmazonEBSCSIDriverPolicyV2', resolving in IAM with Path '/' and IsAttachable true

## Steps to Reproduce
1. release = eks.describe_cluster_versions(defaultOnly=True)['clusterVersions'][0]['clusterVersion'].
2. For addonName='amazon-cloudwatch-observability': find the addonVersion whose compatibilities mark defaultVersion for that release via eks.describe_addon_versions(addonName=..., kubernetesVersion=release), then eks.describe_addon_configuration(addonName=..., addonVersion=<that build>) and read podIdentityConfiguration — expect exactly {'cloudwatch-agent': ['arn:aws:iam::aws:policy/CloudWatchAgentServerPolicy']}; observed a second entry 'arn:aws:iam::aws:policy/AWSXrayWriteOnlyAccess'.
3. Repeat step 2 for addonName='aws-ebs-csi-driver' — expect {'ebs-csi-controller-sa': ['arn:aws:iam::aws:policy/AmazonEBSCSIDriverPolicyV2']}; observed 'arn:aws:iam::aws:policy/service-role/AmazonEBSCSIDriverPolicy'.
4. iam.get_policy / iam.get_policy_version on the expected ARNs must answer attachable, default-version documents granting the agent's metric/log/trace actions and the controller's EBS volume actions respectively.
