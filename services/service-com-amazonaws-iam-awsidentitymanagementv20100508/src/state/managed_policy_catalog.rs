// SPDX-License-Identifier: CC0-1.0

//! AWS's own catalog of managed policies — the `arn:aws:iam::aws:policy/` namespace
//! every account reads and attaches from, and the resolution of an ARN in it.
//!
//! A policy AWS owns is not account state (nothing creates or deletes one), which is
//! why the catalog is a constant here rather than a map in
//! [`ComAmazonawsIamAwsidentitymanagementv20100508State`].

use super::*;

/// One entry of AWS's own catalog of managed policies — a policy AWS owns and
/// every account can read and attach, addressed under the
/// `arn:aws:iam::aws:policy/` namespace ([`AWS_MANAGED_POLICY_ACCOUNT`]).
///
/// An AWS-managed policy is not account state: no call creates or deletes one, so
/// an entry carries no counter and no minted id. Everything a reader reports about
/// one is this entry's OWN data — its identity (the path and friendly name its ARN
/// spells, [`Self::arn`]), its id ([`Self::policy_id`]), the version number it is
/// currently published at ([`Self::default_version_id`]), the two instants of its
/// publication history ([`Self::published`], [`Self::revised`]), whether it may be
/// attached ([`Self::is_attachable`]) and its document — rather than a value a
/// template stamps on every entry alike. Two entries therefore differ in their id,
/// their version and their instants, as two published policies do.
///
/// The content carried for a policy is the ONE version a reader can serve: the
/// version [`Self::default_version_id`] names, which is what
/// `GetPolicy`'s `DefaultVersionId` reports and `GetPolicyVersion` serves. AWS's
/// friendly `Description` prose is not carried — the catalog holds a policy's
/// identity and document, so `Description` is reported only for a policy this
/// ACCOUNT created with one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AwsManagedPolicy {
    /// The policy's path, beginning and ending with `/`: `/` for a policy at the
    /// root, `/service-role/` for one AWS files under that path.
    pub path: &'static str,
    /// The policy's friendly name, as the AWS Managed Policy Reference spells it.
    pub policy_name: &'static str,
    /// This entry's `PolicyId`, carried as catalog data beside the path, name and
    /// document.
    ///
    /// Stable and unique, the two properties `Policy$PolicyId` documents ("the stable
    /// and unique string identifying the policy"): it is fixed per entry, so every
    /// read in every run reports the same id for the same policy, and the suite
    /// asserts the catalog's ids are distinct. AWS spells a policy's unique id as the
    /// `ANPA` prefix followed by 17 further characters, which is the form carried here
    /// and is of the modelled `idType` (len 16..128, `^[\w]+$`); it belongs to the
    /// policy rather than being read off its name, so renaming nothing about an entry
    /// but its id changes which policy a client correlating by id is looking at. An id
    /// the account's own `CreatePolicy` would mint onto a catalog id is passed over by
    /// the minter
    /// ([`ComAmazonawsIamAwsidentitymanagementv20100508State::next_policy_id`]), so one
    /// id never names two policies.
    pub policy_id: &'static str,
    /// The version currently set as this policy's default — `Policy$DefaultVersionId`,
    /// "the identifier for the version of the policy that is set as the default
    /// version".
    ///
    /// AWS bumps a managed policy's version as it revises the policy, so a long-lived
    /// entry is well past `v1` and two entries sit at different numbers. It is carried
    /// per entry, and it is the version a reader serves: the document below IS that
    /// version, so `GetPolicy`'s `DefaultVersionId` always names the version
    /// `GetPolicyVersion` serves.
    pub default_version_id: &'static str,
    /// When this policy was published — `Policy$CreateDate`, "the date and time, in
    /// ISO 8601 date-time format, when the policy was created" — written in that
    /// format and read through [`published_instant`].
    pub published: &'static str,
    /// When this policy was last revised — `Policy$UpdateDate`, "the date and time
    /// when the policy was last updated. When a policy has only one version, this
    /// field contains the date and time when the policy was created" — so an entry AWS
    /// has never revised carries the instant it was published, and a revised one
    /// carries the instant of the revision its [`Self::default_version_id`] names.
    pub revised: &'static str,
    /// Whether AWS publishes this policy as one that can be attached to an IAM user,
    /// group or role — `Policy$IsAttachable`, "specifies whether the policy can be
    /// attached to an IAM user, group, or role".
    ///
    /// Carried per entry, because it is a fact AWS publishes about the policy rather
    /// than something to read off its path. It is a REPORT, not the reservation: the
    /// policies AWS files under the path it reserves for service-linked roles are
    /// published as attachable like the rest, and an attempt to put one on an identity
    /// is what gets refused
    /// ([`ComAmazonawsIamAwsidentitymanagementv20100508State::policy_is_attachable`]).
    pub is_attachable: bool,
    /// The document of the version [`Self::default_version_id`] names, an IAM JSON
    /// policy.
    pub document: &'static str,
}

impl AwsManagedPolicy {
    /// This entry's ARN: `arn:aws:iam::aws:policy<path><name>`.
    ///
    /// Built here rather than through `engine::emulator_arn` because that helper
    /// always writes the emulator's own account id into the account segment, and an
    /// AWS-managed policy is owned by AWS: its account segment is the literal
    /// [`AWS_MANAGED_POLICY_ACCOUNT`]. The rest of the ARN is the same grammar
    /// [`policy_arn`] builds and [`parse_policy_arn`] reads.
    pub fn arn(&self) -> String {
        format!(
            "arn:aws:iam::{AWS_MANAGED_POLICY_ACCOUNT}:policy{}{}",
            self.path, self.policy_name
        )
    }

    /// Whether AWS reserves this policy for the service-linked role it was written
    /// for, so no identity of an account may take it as a permissions policy or as a
    /// permissions boundary — "Amazon Web Services service role policies can only be
    /// attached to the service-linked role for that service"
    /// (`PolicyNotAttachableException`).
    ///
    /// The reservation is the PATH AWS files the policy under
    /// ([`SERVICE_LINKED_ROLE_PATH_PREFIX`]), which is why it is derived rather than
    /// carried: it holds for every policy under that path, including the ones this
    /// catalog does not carry. It is a different question from what
    /// [`Self::is_attachable`] reports, which is the flag AWS publishes for the policy.
    pub fn is_reserved_for_service_linked_role(&self) -> bool {
        is_service_linked_role_path(self.path)
    }

    /// This entry as a managed policy record: the `Policy` metadata every reader
    /// reports, plus the single version `GetPolicyVersion` serves.
    ///
    /// The two counts are the caller's, not the catalog's — they are what the
    /// account's own attachments and permissions boundaries make them, which is why
    /// they are passed in (see
    /// [`ComAmazonawsIamAwsidentitymanagementv20100508State::resolve_policy`]).
    pub(super) fn record(&self, attachment_count: i32, permissions_boundary_usage_count: i32) -> ManagedPolicy {
        let published = published_instant(self.published);
        let revised = published_instant(self.revised);
        let version = PolicyVersion {
            document: Some(self.document.to_string()),
            version_id: Some(self.default_version_id.to_string()),
            is_default_version: true,
            // The version in force came into being when the policy was last revised,
            // which is the instant that revision is published under.
            create_date: revised,
        };
        ManagedPolicy {
            policy: Policy {
                arn: Some(self.arn()),
                policy_id: Some(self.policy_id.to_string()),
                policy_name: Some(self.policy_name.to_string()),
                path: Some(self.path.to_string()),
                default_version_id: Some(self.default_version_id.to_string()),
                attachment_count: Some(attachment_count),
                permissions_boundary_usage_count: Some(permissions_boundary_usage_count),
                // "Specifies whether the policy can be attached to an IAM user,
                // group, or role" (`Policy$IsAttachable`): the flag AWS publishes for
                // this policy, carried per entry.
                is_attachable: self.is_attachable,
                // See the type's documentation: AWS's prose is not carried.
                description: None,
                create_date: published,
                update_date: revised,
                // An AWS-managed policy carries no tags: tagging is an account
                // operation on an account's own resource.
                tags: None,
            },
            versions: [(self.default_version_id.to_string(), version)]
                .into_iter()
                .collect(),
        }
    }
}

/// The instant a catalog entry's published timestamp names, or `None` when the text is
/// not an ISO 8601 date-time.
///
/// The calendar conversion comes from the one text-timestamp reader this crate holds
/// ([`parse_xs_date_time`]), so a catalog instant and a timestamp read off a request are
/// read the same way. The suite asserts every entry's two instants resolve, which is what
/// keeps a mistyped entry from reporting a policy with no dates at all.
fn published_instant(text: &str) -> Option<::aws_smithy_types::DateTime> {
    parse_xs_date_time(text)
}

/// The AWS-managed policies this emulator carries, ordered by ARN.
///
/// AWS-managed policies "are created and managed by AWS" and exist in every
/// account, so a read of one is NOT a read of account state — which is why they
/// live in this constant rather than in
/// [`ComAmazonawsIamAwsidentitymanagementv20100508State::policies`], the account's
/// own policy store.
///
/// SOURCE: the AWS Managed Policy Reference ("AWS managed policies", IAM User
/// Guide), which publishes each policy's path, friendly name and default-version
/// document. AWS's catalog runs to well over a thousand policies and this is a
/// SUBSET of it — the policies the emulator's own suites and fixtures attach, plus
/// the account-wide and job-function ones a client is most likely to read, the
/// service-linked-role policies AWS reserves for the role they were written for
/// ([`AwsManagedPolicy::is_reserved_for_service_linked_role`]), the service-role
/// policies a client provisions a service's own role from, and the root-task
/// policies a privileged root session is taken for, so every FAMILY a client can meet
/// is discoverable here.
///
/// Every entry carries its OWN publication data beside its identity and document: its
/// `PolicyId`, the version number it is currently published at, the instants it was
/// published and last revised, and whether AWS publishes it as attachable. None of
/// those is a value shared by the catalog or derived from the policy's name, so two
/// entries differ in them exactly as two published policies do, and a client keying,
/// pinning or sorting the catalog by them sees one policy per value.
///
/// It also carries every AWS-managed policy ARN another service of this emulator
/// hands a caller as a recommendation or as the policy of a role it provisioned: a
/// control plane that names a policy ARN in a response must be able to describe that
/// ARN, so an identifier this emulator publishes is never one its own API reports as
/// missing. Two places do so today — the EKS add-on catalog's
/// `podIdentityConfiguration[].recommendedManagedPolicies`, whose five policies are
/// entries here, and the permissions policy Application Auto Scaling attaches to the
/// service-linked role it creates for a namespace, one per supported namespace, which
/// are the `AWSApplicationAutoscaling…Policy` entries of the service-linked-role
/// family below.
///
/// An `arn:aws:iam::aws:policy/` ARN this catalog does not carry resolves to no
/// policy, and a read addressed by such an ARN is refused as the missing entity
/// `NoSuchEntityException` names — the operations' modelled refusal — rather than
/// with a code none of them declares.
///
/// Where the catalog carries a policy, it is authoritative about WHERE that policy
/// lives: an entry's path is the one AWS publishes it at, and a friendly name
/// identifies one policy ([`aws_managed_policy_named`]), so the same name under any
/// other path names nothing — for an attachment and a permissions boundary as well as
/// for a read ([`is_published_aws_managed_policy_arn`]). Adding an entry therefore adds
/// both facts: that AWS publishes the policy, and that it publishes it there.
pub const AWS_MANAGED_POLICIES: &[AwsManagedPolicy] = &[
    AwsManagedPolicy {
        path: "/",
        policy_name: "AWSLakeFormationDataAdmin",
        policy_id: "ANPAQT4ZJRMWH6PVKLSND",
        default_version_id: "v4",
        published: "2019-08-08T19:00:40Z",
        revised: "2023-10-10T17:28:23Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["lakeformation:*","cloudtrail:DescribeTrails","cloudtrail:LookupEvents","glue:GetDatabase","glue:GetDatabases","glue:CreateDatabase","glue:UpdateDatabase","glue:DeleteDatabase","glue:GetConnections","glue:SearchTables","glue:GetTable","glue:CreateTable","glue:UpdateTable","glue:DeleteTable","glue:GetTableVersions","glue:GetPartitions","glue:GetTables","glue:ListWorkflows","glue:BatchGetWorkflows","glue:DeleteWorkflow","glue:GetWorkflows","glue:GetTags","glue:TagResource","glue:UnTagResource","iam:ListUsers","iam:ListRoles","iam:GetRole","iam:GetRolePolicy","s3:ListAllMyBuckets","s3:GetBucketLocation"],"Resource":"*"},{"Effect":"Deny","Action":["lakeformation:PutDataLakeSettings"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/",
        policy_name: "AWSLambdaExecute",
        policy_id: "ANPABX7MQWRTZ3KJHVNDC",
        default_version_id: "v4",
        published: "2015-02-06T18:40:46Z",
        revised: "2019-12-13T16:51:20Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["logs:*"],"Resource":"arn:aws:logs:*:*:*"},{"Effect":"Allow","Action":["s3:GetObject","s3:PutObject"],"Resource":"arn:aws:s3:::*"}]}"#,
    },
    // Recommended by the EKS add-on catalog for the `amazon-cloudwatch-observability`
    // add-on's `cloudwatch-agent` service account, beside
    // `CloudWatchAgentServerPolicy`.
    AwsManagedPolicy {
        path: "/",
        policy_name: "AWSXrayWriteOnlyAccess",
        policy_id: "ANPAVR2TYPLKJ5WQZMNHB",
        default_version_id: "v5",
        published: "2016-12-01T18:30:55Z",
        revised: "2021-10-05T20:21:48Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["xray:PutTraceSegments","xray:PutTelemetryRecords","xray:GetSamplingRules","xray:GetSamplingTargets","xray:GetSamplingStatisticSummaries"],"Resource":["*"]}]}"#,
    },
    AwsManagedPolicy {
        path: "/",
        policy_name: "AdministratorAccess",
        policy_id: "ANPAKD6NWQJZT4YPMLRVC",
        default_version_id: "v1",
        published: "2015-02-06T18:39:46Z",
        revised: "2015-02-06T18:39:46Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"*","Resource":"*"}]}"#,
    },
    // The policy AWS documents for the pull half of an Amazon ECR registry ("Amazon
    // ECR managed policies", Amazon ECR User Guide): the grants a container runtime
    // needs to log in to the registry and fetch a manifest and its layers, which is
    // why the node role of an Amazon EKS node group carries it and why a node whose
    // role does not cannot pull a private image at all.
    AwsManagedPolicy {
        path: "/",
        policy_name: "AmazonEC2ContainerRegistryReadOnly",
        policy_id: "ANPAJ4ZMKQWT7VPNHLRDB",
        default_version_id: "v3",
        published: "2015-12-21T17:06:46Z",
        revised: "2019-12-10T20:51:32Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ecr:GetAuthorizationToken","ecr:BatchCheckLayerAvailability","ecr:GetDownloadUrlForLayer","ecr:GetRepositoryPolicy","ecr:DescribeRepositories","ecr:ListImages","ecr:DescribeImages","ecr:BatchGetImage","ecr:GetLifecyclePolicy","ecr:GetLifecyclePolicyPreview","ecr:ListTagsForResource","ecr:DescribeImageScanFindings"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/",
        policy_name: "AmazonEC2ReadOnlyAccess",
        policy_id: "ANPAZM3PKVTWQ7JHNDRLB",
        default_version_id: "v1",
        published: "2015-02-06T18:40:16Z",
        revised: "2015-02-06T18:40:16Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"ec2:Describe*","Resource":"*"},{"Effect":"Allow","Action":"elasticloadbalancing:Describe*","Resource":"*"},{"Effect":"Allow","Action":["cloudwatch:ListMetrics","cloudwatch:GetMetricStatistics","cloudwatch:Describe*"],"Resource":"*"},{"Effect":"Allow","Action":"autoscaling:Describe*","Resource":"*"}]}"#,
    },
    // The policy an EKS cluster's own service role carries, published at the account
    // root: the grant the control plane manages a cluster's EC2 network interfaces,
    // load balancers and volumes through.
    AwsManagedPolicy {
        path: "/",
        policy_name: "AmazonEKSClusterPolicy",
        policy_id: "ANPAHQ5WJZMPT2KVNLYRD",
        default_version_id: "v5",
        published: "2018-05-27T21:06:14Z",
        revised: "2023-02-16T20:20:03Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["autoscaling:DescribeAutoScalingGroups","autoscaling:UpdateAutoScalingGroup","ec2:AttachVolume","ec2:AuthorizeSecurityGroupIngress","ec2:CreateRoute","ec2:CreateSecurityGroup","ec2:CreateTags","ec2:CreateVolume","ec2:DeleteRoute","ec2:DeleteSecurityGroup","ec2:DeleteVolume","ec2:Describe*","ec2:DetachVolume","ec2:ModifyInstanceAttribute","ec2:ModifyVolume","ec2:RevokeSecurityGroupIngress","elasticloadbalancing:Add*","elasticloadbalancing:Apply*","elasticloadbalancing:Attach*","elasticloadbalancing:Configure*","elasticloadbalancing:Create*","elasticloadbalancing:Delete*","elasticloadbalancing:Deregister*","elasticloadbalancing:Describe*","elasticloadbalancing:Detach*","elasticloadbalancing:Modify*","elasticloadbalancing:Register*","elasticloadbalancing:Set*","kms:DescribeKey"],"Resource":"*"},{"Effect":"Allow","Action":"iam:CreateServiceLinkedRole","Resource":"*","Condition":{"StringEquals":{"iam:AWSServiceName":"elasticloadbalancing.amazonaws.com"}}}]}"#,
    },
    // The policy AWS documents for the NODE role of an Amazon EKS node group ("Amazon
    // EKS node IAM role", Amazon EKS User Guide): the Amazon EC2 describes the
    // in-cluster AWS cloud provider makes, the cluster read a joining node makes, and
    // the pod identity exchange a pod of that node makes. It carries no Amazon ECR
    // grant — "your nodes need permissions to pull images from Amazon ECR", which is
    // the separate policy a node role is given beside this one.
    AwsManagedPolicy {
        path: "/",
        policy_name: "AmazonEKSWorkerNodePolicy",
        policy_id: "ANPAVKZ4TJWQM7PNHLRBD",
        default_version_id: "v3",
        published: "2018-05-27T21:09:01Z",
        revised: "2023-11-27T21:40:31Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ec2:DescribeInstances","ec2:DescribeInstanceTypes","ec2:DescribeRouteTables","ec2:DescribeSecurityGroups","ec2:DescribeSubnets","ec2:DescribeVolumes","ec2:DescribeVolumesModifications","ec2:DescribeVpcs","eks:DescribeCluster","eks-auth:AssumeRoleForPodIdentity"],"Resource":"*"}]}"#,
    },
    // Recommended by the EKS add-on catalog for the `vpc-cni` add-on's `aws-node`
    // service account.
    AwsManagedPolicy {
        path: "/",
        policy_name: "AmazonEKS_CNI_Policy",
        policy_id: "ANPATN4KQZWVP6MJHLRCB",
        default_version_id: "v5",
        published: "2018-05-27T21:07:42Z",
        revised: "2023-03-15T18:58:53Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ec2:AssignPrivateIpAddresses","ec2:AttachNetworkInterface","ec2:CreateNetworkInterface","ec2:DeleteNetworkInterface","ec2:DescribeInstances","ec2:DescribeTags","ec2:DescribeNetworkInterfaces","ec2:DescribeInstanceTypes","ec2:DescribeSubnets","ec2:DetachNetworkInterface","ec2:ModifyNetworkInterfaceAttribute","ec2:UnassignPrivateIpAddresses"],"Resource":"*"},{"Effect":"Allow","Action":["ec2:CreateTags"],"Resource":["arn:aws:ec2:*:*:network-interface/*"]}]}"#,
    },
    // The placement-group grant an EMR cluster's role carries, published at the
    // account root — NOT under `/service-role/`, where the legacy EMR service-role
    // policy below lives.
    AwsManagedPolicy {
        path: "/",
        policy_name: "AmazonElasticMapReducePlacementGroupPolicy",
        policy_id: "ANPAWY2JTMKQZ5PVNHDRL",
        default_version_id: "v1",
        published: "2020-09-24T22:12:11Z",
        revised: "2020-09-24T22:12:11Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ec2:CreatePlacementGroup","ec2:DeletePlacementGroup","ec2:DescribePlacementGroups"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/",
        policy_name: "AmazonS3FullAccess",
        policy_id: "ANPAPL7VQZKMW3TJNHBDY",
        default_version_id: "v2",
        published: "2015-02-06T18:40:58Z",
        revised: "2021-08-16T18:14:00Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["s3:*","s3-object-lambda:*"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/",
        policy_name: "AmazonS3ReadOnlyAccess",
        policy_id: "ANPACR6MZQWJT4KVPLNHY",
        default_version_id: "v3",
        published: "2015-02-06T18:40:59Z",
        revised: "2021-08-16T18:19:38Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["s3:Get*","s3:List*","s3:Describe*","s3-object-lambda:Get*","s3-object-lambda:List*"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/",
        policy_name: "AmazonSNSFullAccess",
        policy_id: "ANPADK3TQJMZW7VPNLHYB",
        default_version_id: "v1",
        published: "2015-02-06T18:41:05Z",
        revised: "2015-02-06T18:41:05Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["sns:*"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/",
        policy_name: "AmazonSQSFullAccess",
        policy_id: "ANPAFM5QZTKWJ2PVNHRLD",
        default_version_id: "v1",
        published: "2015-02-06T18:41:07Z",
        revised: "2015-02-06T18:41:07Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["sqs:*"],"Resource":"*"}]}"#,
    },
    // The policy AWS documents for the instance profile of a node that is to be managed
    // by Systems Manager ("Configure instance permissions required for Systems Manager",
    // Systems Manager User Guide): its grants are what let the SSM Agent on an instance
    // register the node and keep reporting it, which is why an instance carrying it
    // becomes a managed node and one that does not never registers.
    AwsManagedPolicy {
        path: "/",
        policy_name: "AmazonSSMManagedInstanceCore",
        policy_id: "ANPAWT3JQZMKV5PNHLRBD",
        default_version_id: "v1",
        published: "2017-08-14T19:32:43Z",
        revised: "2017-08-14T19:32:43Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ssm:DescribeAssociation","ssm:GetDeployablePatchSnapshotForInstance","ssm:GetDocument","ssm:DescribeDocument","ssm:GetManifest","ssm:ListAssociations","ssm:ListInstanceAssociations","ssm:PutInventory","ssm:PutComplianceItems","ssm:PutConfigurePackageResult","ssm:UpdateAssociationStatus","ssm:UpdateInstanceAssociationStatus","ssm:UpdateInstanceInformation"],"Resource":"*"},{"Effect":"Allow","Action":["ssmmessages:CreateControlChannel","ssmmessages:CreateDataChannel","ssmmessages:OpenControlChannel","ssmmessages:OpenDataChannel"],"Resource":"*"},{"Effect":"Allow","Action":["ec2messages:AcknowledgeMessage","ec2messages:DeleteMessage","ec2messages:FailMessage","ec2messages:GetEndpoint","ec2messages:GetMessages","ec2messages:SendReply"],"Resource":"*"}]}"#,
    },
    // Recommended by the EKS add-on catalog for the
    // `amazon-cloudwatch-observability` add-on's `cloudwatch-agent` service account.
    AwsManagedPolicy {
        path: "/",
        policy_name: "CloudWatchAgentServerPolicy",
        policy_id: "ANPAGN2WJQZMT6KVPLHYR",
        default_version_id: "v4",
        published: "2017-11-28T19:45:48Z",
        revised: "2023-07-19T22:23:37Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cloudwatch:PutMetricData","ec2:DescribeVolumes","ec2:DescribeTags","logs:PutLogEvents","logs:PutRetentionPolicy","logs:DescribeLogStreams","logs:DescribeLogGroups","logs:CreateLogStream","logs:CreateLogGroup","xray:PutTraceSegments","xray:PutTelemetryRecords","xray:GetSamplingRules","xray:GetSamplingTargets","xray:GetSamplingStatisticSummaries"],"Resource":"*"},{"Effect":"Allow","Action":["ssm:GetParameter"],"Resource":"arn:aws:ssm:*:*:parameter/AmazonCloudWatch-*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/",
        policy_name: "CloudWatchLogsFullAccess",
        policy_id: "ANPAHP4ZQJMWT3KVNLYRB",
        default_version_id: "v2",
        published: "2015-02-06T18:40:40Z",
        revised: "2023-11-10T21:17:20Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["logs:*"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/",
        policy_name: "IAMFullAccess",
        policy_id: "ANPAJQ6TMZWKP5VJNHLRD",
        default_version_id: "v3",
        published: "2015-02-06T18:40:38Z",
        revised: "2021-02-23T17:48:56Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["iam:*","organizations:DescribeAccount","organizations:DescribeOrganization","organizations:DescribeOrganizationalUnit","organizations:DescribePolicy","organizations:ListChildren","organizations:ListParents","organizations:ListPoliciesForTarget","organizations:ListRoots","organizations:ListPolicies","organizations:ListTargetsForPolicy"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/",
        policy_name: "IAMReadOnlyAccess",
        policy_id: "ANPAKR3WQZJMT7PVNHLYB",
        default_version_id: "v4",
        published: "2015-02-06T18:40:39Z",
        revised: "2018-01-25T19:11:27Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["iam:GenerateCredentialReport","iam:GenerateServiceLastAccessedDetails","iam:Get*","iam:List*","iam:SimulateCustomPolicy","iam:SimulatePrincipalPolicy"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/",
        policy_name: "PowerUserAccess",
        policy_id: "ANPALS5MQZWKT2JVPNHRD",
        default_version_id: "v5",
        published: "2015-02-06T18:39:47Z",
        revised: "2023-07-06T19:34:03Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","NotAction":["iam:*","organizations:*","account:*"],"Resource":"*"},{"Effect":"Allow","Action":["iam:CreateServiceLinkedRole","iam:DeleteServiceLinkedRole","iam:ListRoles","organizations:DescribeOrganization","account:ListRegions","account:GetAccountInformation"],"Resource":"*"}]}"#,
    },
    // The policy a client most often resolves by name: "ReadOnlyAccess" grants the read
    // verbs of EVERY service. AWS's own default version enumerates them service by
    // service and runs far past the character quota a managed policy's document is held
    // to here ([`MAX_MANAGED_POLICY_CHARACTERS`]), so the document carried for it states
    // the same grant in the form the IAM action grammar already has for "an action of
    // every service": the read verb families AWS's document is built out of, each over
    // the whole namespace position, beside the two policy-simulation actions AWS's names
    // outside those families. A reader of this document therefore sees the services a
    // read-only principal can reach as AWS's document puts them in reach — every one —
    // rather than the handful a hand-written namespace list named. The policy's identity
    // — its path, name, ARN and attachability — is AWS's.
    AwsManagedPolicy {
        path: "/",
        policy_name: "ReadOnlyAccess",
        policy_id: "ANPAMT7QZJWKP4VNHLYRB",
        default_version_id: "v116",
        published: "2015-02-06T18:39:48Z",
        revised: "2025-11-03T18:12:44Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["*:BatchGet*","*:Describe*","*:Get*","*:List*","*:Lookup*","*:Query*","*:Scan*","*:Search*","*:View*"],"Resource":"*"},{"Effect":"Allow","Action":["iam:SimulateCustomPolicy","iam:SimulatePrincipalPolicy"],"Resource":"*"}]}"#,
    },
    // The policies AWS files under the path it reserves for service-linked roles.
    // Each belongs to the service-linked role it was written for, so AWS reserves it
    // ([`AwsManagedPolicy::is_reserved_for_service_linked_role`]) and no identity of an
    // account can attach it or be bounded by it — while publishing it, like every other
    // policy of its catalog, as one that can be attached (`IsAttachable` true). A client
    // provisioning a service-linked role resolves the policy of the service it is
    // provisioning FOR by listing this family, so the family is carried rather than the
    // one or two entries a single fixture happens to name.
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "APIGatewayServiceRolePolicy",
        policy_id: "ANPAB4TQZJMWT5PVNHLRD",
        default_version_id: "v10",
        published: "2017-10-20T17:23:10Z",
        revised: "2023-05-04T18:21:44Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ec2:CreateNetworkInterface","ec2:DeleteNetworkInterface","ec2:DescribeNetworkInterfaces","ec2:DescribeSubnets","ec2:DescribeVpcs","elasticloadbalancing:DescribeLoadBalancers","firehose:DescribeDeliveryStream","firehose:PutRecord","firehose:PutRecordBatch","servicediscovery:DiscoverInstances"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSAppConfigServiceRolePolicy",
        policy_id: "ANPAC7KWKJZQMT7PVNHLB",
        default_version_id: "v3",
        published: "2022-04-19T16:05:59Z",
        revised: "2023-11-27T21:34:12Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cloudwatch:DescribeAlarms"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSAppMeshServiceRolePolicy",
        policy_id: "ANPAD2MZQTMWKJ4PVNHLY",
        default_version_id: "v6",
        published: "2019-03-28T21:30:35Z",
        revised: "2022-02-15T19:08:44Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ec2:DescribeInstances","ec2:DescribeSubnets","ec2:DescribeVpcs","servicediscovery:DiscoverInstances","servicediscovery:GetNamespace","servicediscovery:GetService"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSAppSyncServiceRolePolicy",
        policy_id: "ANPAF6NMJWQZKT6PVNHLC",
        default_version_id: "v4",
        published: "2019-06-07T18:27:22Z",
        revised: "2021-09-30T17:14:05Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["logs:CreateLogGroup","logs:CreateLogStream","logs:DescribeLogGroups","logs:DescribeLogStreams","logs:PutLogEvents"],"Resource":"*"}]}"#,
    },
    // AWS spells this one entry of the Application Auto Scaling family with a capital `S`
    // in "AutoScaling", unlike the fourteen beside it, so the published name is carried as
    // AWS publishes it rather than composed like its siblings'.
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSApplicationAutoScalingCustomResourcePolicy",
        policy_id: "ANPAQ4MZQTMWKJ4PVNHLY",
        default_version_id: "v2",
        published: "2018-07-11T18:29:17Z",
        revised: "2019-05-20T19:33:08Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cloudwatch:DeleteAlarms","cloudwatch:DescribeAlarms","cloudwatch:PutMetricAlarm","execute-api:Invoke"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSApplicationAutoscalingAppStreamFleetPolicy",
        policy_id: "ANPAG3PQZJMWT5PVNHLRD",
        default_version_id: "v2",
        published: "2017-10-26T21:20:38Z",
        revised: "2018-07-19T18:38:07Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["appstream:DescribeFleets","appstream:UpdateFleet","cloudwatch:DeleteAlarms","cloudwatch:DescribeAlarms","cloudwatch:PutMetricAlarm"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSApplicationAutoscalingCassandraTablePolicy",
        policy_id: "ANPAH8QWKJZQMT7PVNHLB",
        default_version_id: "v1",
        published: "2020-11-10T18:03:12Z",
        revised: "2020-11-10T18:03:12Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cassandra:Alter","cassandra:Select","cloudwatch:DeleteAlarms","cloudwatch:DescribeAlarms","cloudwatch:PutMetricAlarm"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSApplicationAutoscalingComprehendEndpointPolicy",
        policy_id: "ANPAJ5RZQTMWKJ4PVNHLY",
        default_version_id: "v2",
        published: "2019-11-26T18:18:49Z",
        revised: "2021-03-22T20:11:36Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cloudwatch:DeleteAlarms","cloudwatch:DescribeAlarms","cloudwatch:PutMetricAlarm","comprehend:DescribeEndpoint","comprehend:UpdateEndpoint"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSApplicationAutoscalingDynamoDBTablePolicy",
        policy_id: "ANPAK9SMJWQZKT6PVNHLC",
        default_version_id: "v3",
        published: "2017-10-20T21:15:04Z",
        revised: "2022-08-18T17:42:19Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cloudwatch:DeleteAlarms","cloudwatch:DescribeAlarms","cloudwatch:PutMetricAlarm","dynamodb:DescribeTable","dynamodb:UpdateTable"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSApplicationAutoscalingEC2SpotFleetRequestPolicy",
        policy_id: "ANPAL2TQZJMWT5PVNHLRD",
        default_version_id: "v2",
        published: "2017-10-25T23:50:04Z",
        revised: "2019-09-11T20:26:33Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cloudwatch:DeleteAlarms","cloudwatch:DescribeAlarms","cloudwatch:PutMetricAlarm","ec2:DescribeSpotFleetRequests","ec2:ModifySpotFleetRequest"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSApplicationAutoscalingECSServicePolicy",
        policy_id: "ANPAM7VWKJZQMT7PVNHLB",
        default_version_id: "v3",
        published: "2017-10-25T23:53:08Z",
        revised: "2023-02-09T19:04:51Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cloudwatch:DeleteAlarms","cloudwatch:DescribeAlarms","cloudwatch:PutMetricAlarm","ecs:DescribeServices","ecs:UpdateService"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSApplicationAutoscalingEMRInstanceGroupPolicy",
        policy_id: "ANPAN4WZQTMWKJ4PVNHLY",
        default_version_id: "v2",
        published: "2017-10-26T21:28:57Z",
        revised: "2020-02-18T19:33:25Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cloudwatch:DeleteAlarms","cloudwatch:DescribeAlarms","cloudwatch:PutMetricAlarm","elasticmapreduce:ListInstanceGroups","elasticmapreduce:ModifyInstanceGroups"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSApplicationAutoscalingElastiCacheRGPolicy",
        policy_id: "ANPAP6XMJWQZKT6PVNHLC",
        default_version_id: "v2",
        published: "2021-08-19T17:33:46Z",
        revised: "2023-06-21T16:58:02Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cloudwatch:DeleteAlarms","cloudwatch:DescribeAlarms","cloudwatch:PutMetricAlarm","elasticache:DecreaseReplicaCount","elasticache:DescribeReplicationGroups","elasticache:IncreaseReplicaCount","elasticache:ModifyReplicationGroupShardConfiguration"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSApplicationAutoscalingKafkaClusterPolicy",
        policy_id: "ANPAQ3YQZJMWT5PVNHLRD",
        default_version_id: "v1",
        published: "2020-09-30T21:04:17Z",
        revised: "2020-09-30T21:04:17Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cloudwatch:DeleteAlarms","cloudwatch:DescribeAlarms","cloudwatch:PutMetricAlarm","kafka:DescribeCluster","kafka:DescribeClusterOperation","kafka:UpdateBrokerStorage"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSApplicationAutoscalingLambdaConcurrencyPolicy",
        policy_id: "ANPAR8ZWKJZQMT7PVNHLB",
        default_version_id: "v1",
        published: "2019-12-03T17:48:29Z",
        revised: "2019-12-03T17:48:29Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cloudwatch:DeleteAlarms","cloudwatch:DescribeAlarms","cloudwatch:PutMetricAlarm","lambda:DeleteProvisionedConcurrencyConfig","lambda:GetProvisionedConcurrencyConfig","lambda:PutProvisionedConcurrencyConfig"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSApplicationAutoscalingNeptuneClusterPolicy",
        policy_id: "ANPAS5BZQTMWKJ4PVNHLY",
        default_version_id: "v1",
        published: "2021-01-26T18:12:40Z",
        revised: "2021-01-26T18:12:40Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cloudwatch:DeleteAlarms","cloudwatch:DescribeAlarms","cloudwatch:PutMetricAlarm","rds:AddTagsToResource","rds:CreateDBInstance","rds:DeleteDBInstance","rds:DescribeDBClusters","rds:DescribeDBInstances"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSApplicationAutoscalingRDSClusterPolicy",
        policy_id: "ANPAT9CMJWQZKT6PVNHLC",
        default_version_id: "v4",
        published: "2017-10-17T17:46:56Z",
        revised: "2022-04-28T17:05:33Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cloudwatch:DeleteAlarms","cloudwatch:DescribeAlarms","cloudwatch:PutMetricAlarm","rds:AddTagsToResource","rds:CreateDBInstance","rds:DeleteDBInstance","rds:DescribeDBClusters","rds:DescribeDBInstances","rds:ModifyDBCluster"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSApplicationAutoscalingSageMakerEndpointPolicy",
        policy_id: "ANPAV2DQZJMWT5PVNHLRD",
        default_version_id: "v2",
        published: "2018-02-26T21:02:12Z",
        revised: "2022-11-29T18:47:31Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cloudwatch:DeleteAlarms","cloudwatch:DescribeAlarms","cloudwatch:PutMetricAlarm","sagemaker:DescribeEndpoint","sagemaker:DescribeEndpointConfig","sagemaker:UpdateEndpointWeightsAndCapacities"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSApplicationAutoscalingWorkSpacesPoolPolicy",
        policy_id: "ANPAW7FWKJZQMT7PVNHLB",
        default_version_id: "v1",
        published: "2024-06-25T17:19:58Z",
        revised: "2024-06-25T17:19:58Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cloudwatch:DeleteAlarms","cloudwatch:DescribeAlarms","cloudwatch:PutMetricAlarm","workspaces:DescribeWorkspacesPools","workspaces:UpdateWorkspacesPool"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSApplicationMigrationServiceRolePolicy",
        policy_id: "ANPAX4GZQTMWKJ4PVNHLY",
        default_version_id: "v9",
        published: "2021-04-07T17:57:41Z",
        revised: "2024-01-23T19:26:09Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ec2:DescribeInstances","ec2:DescribeInstanceTypes","ec2:DescribeSnapshots","ec2:DescribeVolumes","ec2:StartInstances","ec2:StopInstances","ec2:TerminateInstances"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSAuditManagerServiceRolePolicy",
        policy_id: "ANPAY6HMJWQZKT6PVNHLC",
        default_version_id: "v7",
        published: "2020-12-08T19:33:52Z",
        revised: "2023-04-11T21:16:45Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cloudtrail:DescribeTrails","cloudtrail:LookupEvents","config:DescribeConfigRules","config:DescribeDeliveryChannels","iam:GetAccountSummary","iam:ListUsers","organizations:DescribeOrganization","organizations:ListAccounts","securityhub:GetFindings"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSAutoScalingPlansEC2AutoScalingPolicy",
        policy_id: "ANPAZ3JQZJMWT5PVNHLRD",
        default_version_id: "v2",
        published: "2018-08-23T22:46:46Z",
        revised: "2019-10-28T18:33:14Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["autoscaling:DescribeAutoScalingGroups","autoscaling:DescribePolicies","autoscaling:DescribeScheduledActions","autoscaling:PutScalingPolicy","autoscaling:PutScheduledUpdateGroupAction","cloudwatch:DeleteAlarms","cloudwatch:DescribeAlarms","cloudwatch:PutMetricAlarm"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSBackupServiceLinkedRolePolicyForBackup",
        policy_id: "ANPANV2ZQJMWT6KPLHYRD",
        default_version_id: "v12",
        published: "2019-01-10T21:01:08Z",
        revised: "2024-02-14T18:33:07Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["backup:DescribeBackupVault","backup:ListBackupVaults","dynamodb:DescribeTable","ec2:CopySnapshot","ec2:CreateSnapshot","ec2:DeleteSnapshot","ec2:DescribeSnapshots","ec2:DescribeVolumes","elasticfilesystem:DescribeFilesystems","rds:DescribeDBInstances","rds:DescribeDBSnapshots","storagegateway:DescribeCachediSCSIVolumes","tag:GetResources"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSConfigServiceRolePolicy",
        policy_id: "ANPAPW4QZTMKJ3VNHLYRB",
        default_version_id: "v41",
        published: "2017-08-14T19:04:46Z",
        revised: "2025-04-02T15:27:51Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cloudtrail:DescribeTrails","cloudtrail:GetTrailStatus","cloudwatch:DescribeAlarms","config:Put*","dynamodb:DescribeTable","ec2:Describe*","elasticloadbalancing:DescribeLoadBalancers","iam:GetRole","iam:ListRoles","lambda:GetFunction","lambda:ListFunctions","organizations:DescribeOrganization","rds:DescribeDBInstances","s3:GetBucketAcl","s3:GetBucketLocation","s3:ListAllMyBuckets","tag:GetResources"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSEC2SpotServiceRolePolicy",
        policy_id: "ANPAQX6MZJWKT5PVNHLRD",
        default_version_id: "v3",
        published: "2017-09-18T18:51:04Z",
        revised: "2021-06-17T19:12:28Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ec2:CreateTags","ec2:DescribeInstances","ec2:DescribeInstanceStatus","ec2:RunInstances","ec2:StartInstances","ec2:StopInstances","ec2:TerminateInstances"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSElasticBeanstalkServiceRolePolicy",
        policy_id: "ANPARY3ZQTMWJ7KVPNHLB",
        default_version_id: "v8",
        published: "2017-09-13T23:19:34Z",
        revised: "2024-10-29T16:44:52Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["autoscaling:Describe*","cloudformation:Describe*","cloudformation:Get*","cloudformation:List*","cloudwatch:DescribeAlarms","cloudwatch:PutMetricAlarm","ec2:Describe*","elasticloadbalancing:Describe*","logs:DescribeLogGroups","logs:DescribeLogStreams","s3:GetObject","s3:ListBucket","sns:ListSubscriptionsByTopic","sqs:GetQueueAttributes"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSLambdaReplicator",
        policy_id: "ANPASZ5QJZMWT2KVPNHLR",
        default_version_id: "v4",
        published: "2017-07-06T22:25:19Z",
        revised: "2020-05-07T21:01:12Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["lambda:CreateFunction","lambda:DeleteFunction","lambda:DisableReplication","lambda:EnableReplication","lambda:GetFunction","lambda:UpdateFunctionCode","lambda:UpdateFunctionConfiguration"],"Resource":"arn:aws:lambda:*:*:function:*"},{"Effect":"Allow","Action":["iam:PassRole"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSOrganizationsServiceTrustPolicy",
        policy_id: "ANPATB7MQZWKJ4PVNHLYR",
        default_version_id: "v2",
        published: "2017-02-27T00:20:51Z",
        revised: "2018-09-05T20:01:46Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["organizations:DescribeAccount","organizations:DescribeOrganization","organizations:DescribeOrganizationalUnit","organizations:ListAccounts","organizations:ListAccountsForParent","organizations:ListChildren","organizations:ListParents","organizations:ListRoots"],"Resource":"*"},{"Effect":"Allow","Action":["iam:DeleteRole"],"Resource":"arn:aws:iam::*:role/aws-service-role/organizations.amazonaws.com/*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSSSOServiceRolePolicy",
        policy_id: "ANPAUC2ZQJMWT6KVPNHLY",
        default_version_id: "v17",
        published: "2017-12-05T18:36:08Z",
        revised: "2024-06-20T17:09:33Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["iam:AttachRolePolicy","iam:CreateRole","iam:CreateSAMLProvider","iam:DeleteRole","iam:DeleteRolePolicy","iam:DeleteSAMLProvider","iam:DetachRolePolicy","iam:GetRole","iam:GetSAMLProvider","iam:ListAttachedRolePolicies","iam:ListRolePolicies","iam:PutRolePolicy","iam:UpdateSAMLProvider","organizations:DescribeAccount","organizations:DescribeOrganization","organizations:ListAccounts"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSSupportServiceRolePolicy",
        policy_id: "ANPAVD4QZTMWJ3KPVNHLR",
        default_version_id: "v29",
        published: "2018-04-19T18:04:44Z",
        revised: "2025-02-11T21:18:05Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["support:*","trustedadvisor:Describe*","trustedadvisor:Get*","trustedadvisor:List*","cloudformation:DescribeStacks","cloudwatch:DescribeAlarms","ec2:DescribeInstances","iam:GetRole","iam:ListRoles","s3:ListAllMyBuckets"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSTrustedAdvisorServiceRolePolicy",
        policy_id: "ANPAWE6MZQJKT5PVNHLYR",
        default_version_id: "v11",
        published: "2018-02-22T21:24:25Z",
        revised: "2024-08-13T19:38:16Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["autoscaling:Describe*","cloudformation:DescribeStacks","cloudtrail:DescribeTrails","cloudwatch:GetMetricStatistics","ec2:Describe*","elasticloadbalancing:Describe*","iam:GenerateCredentialReport","iam:GetAccountSummary","iam:ListAccessKeys","kms:DescribeKey","rds:DescribeDBInstances","route53:ListHostedZones","s3:GetBucketAcl","s3:GetBucketPolicy","s3:ListAllMyBuckets","trustedadvisor:Describe*"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AWSVPCTransitGatewayServiceRolePolicy",
        policy_id: "ANPAXF3ZQTMWJ7KVPNHLD",
        default_version_id: "v2",
        published: "2018-11-26T16:19:13Z",
        revised: "2021-03-10T17:45:29Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ec2:CreateNetworkInterface","ec2:DeleteNetworkInterface","ec2:DescribeNetworkInterfaces","ec2:DescribeSubnets","ec2:DescribeVpcs","ec2:ModifyNetworkInterfaceAttribute"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AmazonECSServiceRolePolicy",
        policy_id: "ANPAYG5QJZMWT2KVPNHLB",
        default_version_id: "v7",
        published: "2017-10-14T01:18:58Z",
        revised: "2023-05-24T18:21:34Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ec2:AttachNetworkInterface","ec2:CreateNetworkInterface","ec2:DeleteNetworkInterface","ec2:DescribeInstances","ec2:DescribeNetworkInterfaces","ec2:DescribeSubnets","ec2:DescribeVpcs","ec2:DetachNetworkInterface","elasticloadbalancing:DeregisterTargets","elasticloadbalancing:DescribeTargetGroups","elasticloadbalancing:RegisterTargets","logs:CreateLogStream","logs:PutLogEvents","route53:ChangeResourceRecordSets","route53:ListHostedZonesByName"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AmazonEKSServiceRolePolicy",
        policy_id: "ANPAZH7MQZWKJ4PVNHLYT",
        default_version_id: "v6",
        published: "2018-05-27T21:08:21Z",
        revised: "2023-07-28T18:11:42Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ec2:CreateNetworkInterface","ec2:CreateNetworkInterfacePermission","ec2:DeleteNetworkInterface","ec2:DescribeInstances","ec2:DescribeNetworkInterfaces","ec2:DescribeSecurityGroups","ec2:DescribeSubnets","ec2:DescribeVpcs","ec2:ModifyNetworkInterfaceAttribute","elasticloadbalancing:DescribeLoadBalancers","elasticloadbalancing:DescribeTargetGroups","iam:ListAttachedRolePolicies","kms:CreateGrant","kms:DescribeKey"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AmazonElasticFileSystemServiceRolePolicy",
        policy_id: "ANPAAJ2ZQTMWK6PVNHLRD",
        default_version_id: "v3",
        published: "2019-01-14T18:33:21Z",
        revised: "2022-11-03T19:26:08Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["backup:DescribeBackupVault","ec2:CreateNetworkInterface","ec2:DeleteNetworkInterface","ec2:DescribeNetworkInterfaceAttribute","ec2:DescribeNetworkInterfaces","ec2:DescribeSecurityGroups","ec2:DescribeSubnets","ec2:ModifyNetworkInterfaceAttribute","tag:GetResources"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AmazonElasticsearchServiceRolePolicy",
        policy_id: "ANPABK4QJZMWT3KVPNHLY",
        default_version_id: "v8",
        published: "2017-07-07T00:15:31Z",
        revised: "2024-09-11T17:52:19Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ec2:CreateNetworkInterface","ec2:DeleteNetworkInterface","ec2:DescribeNetworkInterfaces","ec2:DescribeSecurityGroups","ec2:DescribeSubnets","ec2:DescribeVpcs","ec2:ModifyNetworkInterfaceAttribute","elasticloadbalancing:DescribeLoadBalancers"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AmazonGuardDutyServiceRolePolicy",
        policy_id: "ANPACL6MZQWKJ5PVNHLTR",
        default_version_id: "v9",
        published: "2017-11-28T22:29:40Z",
        revised: "2024-04-17T20:31:55Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cloudtrail:DescribeTrails","cloudtrail:GetTrailStatus","ec2:DescribeInstances","ec2:DescribeNetworkInterfaces","ec2:DescribeSecurityGroups","ec2:DescribeSubnets","ec2:DescribeVpcs","iam:ListAccountAliases","organizations:DescribeAccount","organizations:DescribeOrganization","organizations:ListAccounts","s3:GetBucketPublicAccessBlock","s3:GetEncryptionConfiguration"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AmazonInspectorServiceRolePolicy",
        policy_id: "ANPADM3ZQTJWK7PVNHLYB",
        default_version_id: "v4",
        published: "2017-10-11T19:50:35Z",
        revised: "2021-09-23T18:04:27Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["directconnect:DescribeConnections","directconnect:DescribeVirtualGateways","ec2:DescribeInstances","ec2:DescribeNetworkAcls","ec2:DescribeNetworkInterfaces","ec2:DescribeRouteTables","ec2:DescribeSecurityGroups","ec2:DescribeSubnets","ec2:DescribeTags","ec2:DescribeVpcs","elasticloadbalancing:DescribeLoadBalancers","elasticloadbalancing:DescribeTags"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AmazonMacieServiceRolePolicy",
        policy_id: "ANPAEN5QJZMWT2PVNHLRK",
        default_version_id: "v6",
        published: "2018-05-14T21:10:12Z",
        revised: "2023-01-19T18:41:03Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cloudtrail:DescribeTrails","cloudtrail:GetTrailStatus","organizations:DescribeAccount","organizations:DescribeOrganization","s3:GetBucketAcl","s3:GetBucketLocation","s3:GetBucketPolicy","s3:GetBucketPublicAccessBlock","s3:GetEncryptionConfiguration","s3:GetObject","s3:ListAllMyBuckets","s3:ListBucket"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AmazonRDSServiceRolePolicy",
        policy_id: "ANPAFP7MQZWKJ4VNHLYRT",
        default_version_id: "v7",
        published: "2017-10-30T22:03:17Z",
        revised: "2024-01-23T19:14:48Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cloudwatch:PutMetricData","ec2:CreateNetworkInterface","ec2:DeleteNetworkInterface","ec2:DescribeNetworkInterfaces","ec2:DescribeSecurityGroups","ec2:DescribeSubnets","ec2:DescribeVpcs","logs:CreateLogGroup","logs:CreateLogStream","logs:PutLogEvents","logs:PutRetentionPolicy","sns:Publish"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AmazonRedshiftServiceLinkedRolePolicy",
        policy_id: "ANPAGQ2ZQTMWK6PVNHLJD",
        default_version_id: "v5",
        published: "2019-09-18T20:12:34Z",
        revised: "2023-04-12T17:33:26Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ec2:AssignIpv6Addresses","ec2:AssignPrivateIpAddresses","ec2:CreateNetworkInterface","ec2:DeleteNetworkInterface","ec2:DescribeAddresses","ec2:DescribeNetworkInterfaces","ec2:DescribeSecurityGroups","ec2:DescribeSubnets","ec2:DescribeVpcs","ec2:ModifyNetworkInterfaceAttribute","ec2:UnassignIpv6Addresses","ec2:UnassignPrivateIpAddresses"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AmazonSSMServiceRolePolicy",
        policy_id: "ANPAHR4QJZMWT3PVNHLYK",
        default_version_id: "v10",
        published: "2017-11-20T22:25:11Z",
        revised: "2024-05-29T18:27:44Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cloudformation:DescribeStacks","ec2:DescribeInstances","ec2:DescribeInstanceStatus","resource-groups:GetGroup","resource-groups:ListGroupResources","ssm:DescribeInstanceInformation","ssm:GetAutomationExecution","ssm:ListCommands","ssm:SendCommand","ssm:StartAutomationExecution","states:DescribeExecution","tag:GetResources"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "AutoScalingServiceRolePolicy",
        policy_id: "ANPAJS6MZQWKJ5PVNHLTR",
        default_version_id: "v4",
        published: "2017-10-06T18:30:17Z",
        revised: "2022-07-14T19:08:52Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cloudwatch:DeleteAlarms","cloudwatch:DescribeAlarms","cloudwatch:PutMetricAlarm","ec2:AttachClassicLinkVpc","ec2:CreateTags","ec2:DescribeInstances","ec2:DescribeInstanceStatus","ec2:DescribeSubnets","ec2:RunInstances","ec2:TerminateInstances","elasticloadbalancing:DeregisterTargets","elasticloadbalancing:RegisterTargets"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "ConfigConformsServiceRolePolicy",
        policy_id: "ANPAKT3ZQTJWM7PVNHLYB",
        default_version_id: "v3",
        published: "2019-07-11T20:40:29Z",
        revised: "2021-12-09T18:19:36Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cloudformation:CreateStack","cloudformation:DeleteStack","cloudformation:DescribeStackEvents","cloudformation:DescribeStacks","config:DescribeConfigRules","config:PutConfigRule","config:DeleteConfigRule","s3:GetObject","s3:ListBucket"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "DynamoDBReplicationServiceRolePolicy",
        policy_id: "ANPALU5QJZMWT2KVPNHRY",
        default_version_id: "v9",
        published: "2017-11-29T07:18:50Z",
        revised: "2023-09-21T18:36:14Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["dynamodb:DeleteItem","dynamodb:DescribeTable","dynamodb:GetItem","dynamodb:PutItem","dynamodb:Query","dynamodb:Scan","dynamodb:UpdateItem","dynamodb:DescribeStream","dynamodb:GetRecords","dynamodb:GetShardIterator","dynamodb:ListStreams"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "ElasticLoadBalancingServiceRolePolicy",
        policy_id: "ANPAMV7MQZWKJ4PVNHLTD",
        default_version_id: "v6",
        published: "2017-09-19T22:36:18Z",
        revised: "2023-06-15T17:48:09Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ec2:AssignIpv6Addresses","ec2:AssignPrivateIpAddresses","ec2:CreateNetworkInterface","ec2:DeleteNetworkInterface","ec2:DescribeAddresses","ec2:DescribeInstances","ec2:DescribeInternetGateways","ec2:DescribeNetworkInterfaces","ec2:DescribeSecurityGroups","ec2:DescribeSubnets","ec2:DescribeVpcs","ec2:ModifyNetworkInterfaceAttribute","ec2:UnassignIpv6Addresses","ec2:UnassignPrivateIpAddresses","logs:CreateLogDelivery","logs:DeleteLogDelivery","logs:GetLogDelivery","logs:ListLogDeliveries","logs:UpdateLogDelivery"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "LakeFormationDataAccessServiceRolePolicy",
        policy_id: "ANPANW2ZQTMWK6PVJHLRY",
        default_version_id: "v3",
        published: "2019-08-08T19:12:45Z",
        revised: "2022-03-24T18:52:31Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["glue:GetDatabase","glue:GetDatabases","glue:GetPartitions","glue:GetTable","glue:GetTables","s3:GetBucketLocation","s3:GetObject","s3:ListBucket"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "WAFRegionalLoggingServiceRolePolicy",
        policy_id: "ANPAPX4QJZMWT3KVNHLYB",
        default_version_id: "v2",
        published: "2018-08-24T21:05:33Z",
        revised: "2019-10-02T17:21:48Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["firehose:DescribeDeliveryStream","firehose:PutRecord","firehose:PutRecordBatch"],"Resource":"arn:aws:firehose:*:*:deliverystream/aws-waf-logs-*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/aws-service-role/",
        policy_name: "WAFV2LoggingServiceRolePolicy",
        policy_id: "ANPAQY6MZQWKJ5PVNHLTD",
        default_version_id: "v2",
        published: "2019-11-25T18:14:27Z",
        revised: "2021-05-18T19:33:52Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["firehose:DescribeDeliveryStream","firehose:PutRecord","firehose:PutRecordBatch"],"Resource":"arn:aws:firehose:*:*:deliverystream/aws-waf-logs-*"}]}"#,
    },
    // The policies AWS files under the path it reserves for JOB FUNCTIONS — the family
    // a client provisioning a human role attaches, resolved by listing the catalog
    // under that path. They are published to be attached by any identity, and an attach
    // of one is accepted — unlike the reserved service-linked-role family above, which
    // AWS publishes as attachable too but refuses an attach of.
    //
    // AWS publishes the family as a whole, so the whole family is carried: one entry per
    // job function the AWS Managed Policy Reference files under `/job-function/`,
    // including the console-administrator job and the per-resource administrator jobs of
    // Oracle Database@AWS, which AWS files here beside the older ones. Each document
    // states the grant that job function is published with, stated inside the character
    // quota a carried document is held to ([`MAX_MANAGED_POLICY_CHARACTERS`]) — the
    // services and verb families AWS's own default version names, rather than its full
    // per-action enumeration.
    AwsManagedPolicy {
        path: "/job-function/",
        policy_name: "AWSManagementConsoleAdministratorAccess",
        policy_id: "ANPAB8KWKJZQMT7PVNHLB",
        default_version_id: "v2",
        published: "2024-11-13T16:51:02Z",
        revised: "2025-02-27T19:04:33Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["iam:CreateAccountAlias","iam:CreateLoginProfile","iam:DeleteAccountAlias","iam:DeleteLoginProfile","iam:GetAccountPasswordPolicy","iam:GetAccountSummary","iam:GetLoginProfile","iam:ListAccountAliases","iam:ListUsers","iam:UpdateAccountPasswordPolicy","iam:UpdateLoginProfile","signin:*","sts:GetCallerIdentity"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/job-function/",
        policy_name: "AmazonODBAutonomousDatabaseAdmin",
        policy_id: "ANPAC5LZQTMWKJ4PVNHLY",
        default_version_id: "v1",
        published: "2025-07-15T17:08:24Z",
        revised: "2025-07-15T17:08:24Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["odb:CreateCloudAutonomousVmCluster","odb:DeleteCloudAutonomousVmCluster","odb:GetCloudAutonomousVmCluster","odb:ListCloudAutonomousVmClusters","odb:ListDbServers","odb:ListTagsForResource","odb:TagResource","odb:UntagResource"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/job-function/",
        policy_name: "AmazonODBAutonomousVmClusterAdmin",
        policy_id: "ANPAD9MMJWQZKT6PVNHLC",
        default_version_id: "v1",
        published: "2025-07-15T17:10:56Z",
        revised: "2025-07-15T17:10:56Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["odb:CreateCloudAutonomousVmCluster","odb:DeleteCloudAutonomousVmCluster","odb:GetCloudAutonomousVmCluster","odb:ListAutonomousVirtualMachines","odb:ListCloudAutonomousVmClusters","odb:ListTagsForResource"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/job-function/",
        policy_name: "AmazonODBExadataInfrastructureAdmin",
        policy_id: "ANPAF2NQZJMWT5PVNHLRD",
        default_version_id: "v1",
        published: "2025-07-15T17:13:31Z",
        revised: "2025-07-15T17:13:31Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["odb:CreateCloudExadataInfrastructure","odb:DeleteCloudExadataInfrastructure","odb:GetCloudExadataInfrastructure","odb:GetCloudExadataInfrastructureUnallocatedResources","odb:ListCloudExadataInfrastructures","odb:ListDbSystemShapes","odb:ListTagsForResource"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/job-function/",
        policy_name: "AmazonODBExadataVmClusterAdmin",
        policy_id: "ANPAG7PWKJZQMT7PVNHLB",
        default_version_id: "v1",
        published: "2025-07-15T17:16:09Z",
        revised: "2025-07-15T17:16:09Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["odb:CreateCloudVmCluster","odb:DeleteCloudVmCluster","odb:GetCloudVmCluster","odb:ListCloudVmClusters","odb:ListDbNodes","odb:ListGiVersions","odb:ListTagsForResource"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/job-function/",
        policy_name: "AmazonODBExascaleStorageVaultAdmin",
        policy_id: "ANPAH4QZQTMWKJ4PVNHLY",
        default_version_id: "v1",
        published: "2025-07-15T17:18:47Z",
        revised: "2025-07-15T17:18:47Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["odb:CreateCloudExascaleStorageVault","odb:DeleteCloudExascaleStorageVault","odb:GetCloudExascaleStorageVault","odb:ListCloudExascaleStorageVaults","odb:ListTagsForResource"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/job-function/",
        policy_name: "AmazonODBExascaleVmClusterAdmin",
        policy_id: "ANPAJ6RMJWQZKT6PVNHLC",
        default_version_id: "v1",
        published: "2025-07-15T17:21:22Z",
        revised: "2025-07-15T17:21:22Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["odb:CreateCloudExascaleVmCluster","odb:DeleteCloudExascaleVmCluster","odb:GetCloudExascaleVmCluster","odb:ListCloudExascaleVmClusters","odb:ListTagsForResource"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/job-function/",
        policy_name: "AmazonODBNetworkAdmin",
        policy_id: "ANPAK3SQZJMWT5PVNHLRD",
        default_version_id: "v1",
        published: "2025-07-15T17:23:58Z",
        revised: "2025-07-15T17:23:58Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ec2:DescribeSubnets","ec2:DescribeVpcs","odb:CreateOdbNetwork","odb:CreateOdbPeeringConnection","odb:DeleteOdbNetwork","odb:DeleteOdbPeeringConnection","odb:GetOdbNetwork","odb:ListOdbNetworks","odb:ListOdbPeeringConnections","odb:ListTagsForResource"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/job-function/",
        policy_name: "Billing",
        policy_id: "ANPARZ3ZQTJWK7PVNHLYM",
        default_version_id: "v8",
        published: "2016-11-10T17:33:18Z",
        revised: "2024-07-31T18:22:41Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["account:GetAccountInformation","aws-portal:*","billing:*","budgets:*","ce:*","consolidatedbilling:*","cur:*","freetier:*","invoicing:*","payments:*","pricing:*","purchase-orders:*","savingsplans:Describe*","tax:*"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/job-function/",
        policy_name: "DataScientist",
        policy_id: "ANPASB5QJZMWT2KVPNHLY",
        default_version_id: "v5",
        published: "2016-11-10T17:28:11Z",
        revised: "2023-08-09T19:04:26Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["athena:*","cloudwatch:GetMetricData","cloudwatch:GetMetricStatistics","datapipeline:Describe*","datapipeline:GetPipelineDefinition","dynamodb:Describe*","dynamodb:List*","ec2:Describe*","elasticmapreduce:*","glue:Get*","glue:List*","iam:GetRole","iam:ListRoles","kinesis:Describe*","kinesis:List*","kms:ListAliases","lambda:ListFunctions","logs:Describe*","logs:Get*","machinelearning:*","rds:Describe*","redshift:Describe*","s3:Get*","s3:List*","sagemaker:*","sdb:*","sns:ListSubscriptions","sns:ListTopics","sqs:ListQueues","tag:GetResources"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/job-function/",
        policy_name: "DatabaseAdministrator",
        policy_id: "ANPATC7MQZWKJ4PVNHLRY",
        default_version_id: "v6",
        published: "2016-11-10T17:25:43Z",
        revised: "2023-02-28T18:17:52Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cloudwatch:Describe*","cloudwatch:Get*","cloudwatch:List*","cloudwatch:PutMetricAlarm","datapipeline:*","dax:*","dynamodb:*","ec2:DescribeAccountAttributes","ec2:DescribeAvailabilityZones","ec2:DescribeSecurityGroups","ec2:DescribeSubnets","ec2:DescribeVpcs","elasticache:*","iam:GetRole","iam:ListRoles","kms:ListAliases","logs:Describe*","logs:Get*","rds:*","redshift:*","s3:GetBucketLocation","s3:ListAllMyBuckets","sns:ListSubscriptions","sns:ListTopics","sqs:ListQueues"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/job-function/",
        policy_name: "NetworkAdministrator",
        policy_id: "ANPAUD2ZQTMWK6PVNHLJB",
        default_version_id: "v6",
        published: "2016-11-10T17:31:35Z",
        revised: "2022-09-12T19:41:07Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["autoscaling:Describe*","cloudfront:ListDistributions","cloudwatch:DeleteAlarms","cloudwatch:DescribeAlarms","cloudwatch:GetMetricStatistics","cloudwatch:PutMetricAlarm","directconnect:*","ec2:AllocateAddress","ec2:AssociateAddress","ec2:AttachInternetGateway","ec2:AttachVpnGateway","ec2:AuthorizeSecurityGroupEgress","ec2:AuthorizeSecurityGroupIngress","ec2:CreateSecurityGroup","ec2:Describe*","elasticbeanstalk:Describe*","elasticloadbalancing:*","logs:Describe*","logs:Get*","route53:*","route53domains:*","sns:ListSubscriptions","sns:ListTopics"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/job-function/",
        policy_name: "SupportUser",
        policy_id: "ANPAVE4QJZMWT3KVPNHLY",
        default_version_id: "v9",
        published: "2016-11-10T17:21:46Z",
        revised: "2024-03-07T18:28:19Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["acm:Describe*","acm:Get*","acm:List*","autoscaling:Describe*","cloudformation:Describe*","cloudformation:List*","cloudwatch:Describe*","cloudwatch:Get*","cloudwatch:List*","ec2:Describe*","ecs:Describe*","ecs:List*","iam:Get*","iam:List*","logs:Describe*","logs:Get*","rds:Describe*","s3:Get*","s3:List*","support:*","trustedadvisor:Describe*"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/job-function/",
        policy_name: "SystemAdministrator",
        policy_id: "ANPAWF6MZQWKJ5PVNHLTB",
        default_version_id: "v7",
        published: "2016-11-10T17:23:56Z",
        revised: "2023-12-05T17:36:48Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["autoscaling:*","cloudtrail:Describe*","cloudtrail:Get*","cloudtrail:List*","cloudwatch:*","ec2:*","ecs:*","elasticloadbalancing:*","events:*","iam:GetRole","iam:ListRoles","iam:PassRole","kms:ListAliases","logs:*","rds:Describe*","route53:Get*","route53:List*","s3:Get*","s3:List*","sns:*","ssm:*"],"Resource":"*"}]}"#,
    },
    // "This policy grants permissions to view resources and basic metadata across all
    // Amazon Web Services services": the same whole-catalog reach as `ReadOnlyAccess`
    // above, narrowed to the listing and describing verbs, which is what separates the
    // two policies.
    AwsManagedPolicy {
        path: "/job-function/",
        policy_name: "ViewOnlyAccess",
        policy_id: "ANPAXG3ZQTJWK7PVNHLYD",
        default_version_id: "v22",
        published: "2016-11-10T17:20:27Z",
        revised: "2025-06-10T18:44:33Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["*:Describe*","*:List*","*:View*"],"Resource":"*"},{"Effect":"Allow","Action":["s3:GetBucketLocation","s3:GetAccountPublicAccessBlock","tag:GetResources"],"Resource":"*"}]}"#,
    },
    // The policies AWS files under the path it reserves for the privileged ROOT TASKS a
    // management account may take in a member account — the family a client enumerates to
    // audit or unlock root access. AWS publishes them to be named as the task a
    // privileged session is taken for, so each reports `IsAttachable` true like every
    // other policy outside the service-linked-role path, and each is published at its
    // first version.
    //
    // The family is carried whole, as AWS publishes it: the three root-credential tasks
    // and the two resource-policy unlock tasks, so a client listing the catalog under this
    // path finds the task it needs rather than an empty page.
    //
    // AWS published the whole family in ONE change: the ids it assigned in that change share
    // a prefix, which is why these entries' ids do. None of the five has been revised since,
    // so each reports its publication instant as its revision instant too.
    //
    // Three of the five ids, and the instant the last of them was published at, are the
    // published values. For the other two ids and the four earlier instants the published
    // values are not recorded anywhere this emulator can read, so what stands for them is
    // the family's own FORM — an id in that same block of the namespace, and one publication
    // a second after the next in the order the names sort in, as the earliest families in
    // this catalog are published — and those six fields are the ones to replace first when
    // the published values become available.
    AwsManagedPolicy {
        path: "/root-task/",
        policy_name: "IAMAuditRootUserCredentials",
        policy_id: "ANPAZKAPJZG4BDSCREYVS",
        default_version_id: "v1",
        published: "2024-11-06T21:50:58Z",
        revised: "2024-11-06T21:50:58Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["iam:GetAccountSummary","iam:GetLoginProfile","iam:ListAccessKeys","iam:ListMFADevices","iam:ListSigningCertificates","iam:ListVirtualMFADevices"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/root-task/",
        policy_name: "IAMCreateRootUserPassword",
        policy_id: "ANPAZKAPJZG4KXMTQDLWR",
        default_version_id: "v1",
        published: "2024-11-06T21:50:59Z",
        revised: "2024-11-06T21:50:59Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["iam:CreateLoginProfile"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/root-task/",
        policy_name: "IAMDeleteRootUserCredentials",
        policy_id: "ANPAZKAPJZG4VSNEUHYCT",
        default_version_id: "v1",
        published: "2024-11-06T21:51:00Z",
        revised: "2024-11-06T21:51:00Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["iam:DeactivateMFADevice","iam:DeleteAccessKey","iam:DeleteLoginProfile","iam:DeleteSigningCertificate","iam:DeleteVirtualMFADevice","iam:ListAccessKeys","iam:ListMFADevices","iam:ListSigningCertificates"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/root-task/",
        policy_name: "S3UnlockBucketPolicy",
        policy_id: "ANPAZKAPJZG4OALTIPZOF",
        default_version_id: "v1",
        published: "2024-11-06T21:51:01Z",
        revised: "2024-11-06T21:51:01Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["s3:DeleteBucketPolicy","s3:GetBucketPolicy","s3:PutBucketPolicy"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/root-task/",
        policy_name: "SQSUnlockQueuePolicy",
        policy_id: "ANPAZKAPJZG4HPZI4FDUV",
        default_version_id: "v1",
        published: "2024-11-06T21:51:02Z",
        revised: "2024-11-06T21:51:02Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["sqs:GetQueueAttributes","sqs:GetQueueUrl","sqs:ListQueues","sqs:SetQueueAttributes"],"Resource":"*"}]}"#,
    },
    // The policies AWS files under the path it reserves for the SERVICE ROLES an account
    // creates for a service to act through — the family a client enumerates to find the
    // grant a service's own role is published with, and then attaches to that role. AWS
    // published most of this family in the first years of IAM and has revised several of
    // them since, so an entry here is well past its first version while another still sits
    // at `v1`, and each carries the instants of its own publication and last revision.
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AWSBatchServiceRole",
        policy_id: "ANPAJ7A2TTYSEWFTSGHOA",
        default_version_id: "v11",
        published: "2016-12-13T00:38:59Z",
        revised: "2023-03-10T17:24:31Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ec2:DescribeAccountAttributes","ec2:DescribeInstances","ec2:DescribeInstanceStatus","ec2:DescribeInstanceAttribute","ec2:DescribeSubnets","ec2:DescribeSecurityGroups","ec2:DescribeKeyPairs","ec2:DescribeImages","ec2:DescribeImageAttribute","ec2:DescribeLaunchTemplateVersions","ec2:CreateLaunchTemplate","ec2:DeleteLaunchTemplate","ec2:RequestSpotFleet","ec2:CancelSpotFleetRequests","ec2:ModifySpotFleetRequest","ec2:TerminateInstances","ec2:RunInstances","autoscaling:DescribeAccountLimits","autoscaling:DescribeAutoScalingGroups","autoscaling:DescribeLaunchConfigurations","autoscaling:DescribeAutoScalingInstances","autoscaling:CreateLaunchConfiguration","autoscaling:CreateAutoScalingGroup","autoscaling:UpdateAutoScalingGroup","autoscaling:SetDesiredCapacity","autoscaling:DeleteLaunchConfiguration","autoscaling:DeleteAutoScalingGroup","autoscaling:CreateOrUpdateTags","autoscaling:SuspendProcesses","autoscaling:PutNotificationConfiguration","autoscaling:TerminateInstanceInAutoScalingGroup","ecs:DescribeClusters","ecs:DescribeContainerInstances","ecs:DescribeTaskDefinition","ecs:DescribeTasks","ecs:ListClusters","ecs:ListContainerInstances","ecs:ListTaskDefinitionFamilies","ecs:ListTaskDefinitions","ecs:ListTasks","ecs:CreateCluster","ecs:DeleteCluster","ecs:RegisterTaskDefinition","ecs:DeregisterTaskDefinition","ecs:RunTask","ecs:StartTask","ecs:StopTask","ecs:UpdateContainerAgent","ecs:DeregisterContainerInstance","logs:CreateLogGroup","logs:CreateLogStream","logs:PutLogEvents","logs:DescribeLogGroups","iam:GetInstanceProfile","iam:GetRole"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AWSCloudHSMRole",
        policy_id: "ANPAI7QIUU4GC66SF26WE",
        default_version_id: "v1",
        published: "2015-02-06T18:41:23Z",
        revised: "2015-02-06T18:41:23Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ec2:CreateNetworkInterface","ec2:CreateTags","ec2:DeleteNetworkInterface","ec2:DescribeNetworkInterfaceAttribute","ec2:DescribeNetworkInterfaces","ec2:DescribeSubnets","ec2:DescribeVpcs","ec2:DetachNetworkInterface"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AWSCodeDeployRole",
        policy_id: "ANPAJ2NKMKD73QS5NBFLA",
        default_version_id: "v6",
        published: "2015-05-19T18:10:14Z",
        revised: "2019-03-29T18:40:31Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["autoscaling:CompleteLifecycleAction","autoscaling:DeleteLifecycleHook","autoscaling:DescribeAutoScalingGroups","autoscaling:DescribeLifecycleHooks","autoscaling:PutLifecycleHook","autoscaling:RecordLifecycleActionHeartbeat","autoscaling:UpdateAutoScalingGroup","cloudwatch:DescribeAlarms","ec2:DescribeInstances","ec2:DescribeInstanceStatus","ec2:TerminateInstances","elasticloadbalancing:DescribeLoadBalancers","elasticloadbalancing:DescribeInstanceHealth","elasticloadbalancing:RegisterInstancesWithLoadBalancer","elasticloadbalancing:DeregisterInstancesFromLoadBalancer","elasticloadbalancing:DescribeTargetGroups","elasticloadbalancing:DescribeTargetHealth","elasticloadbalancing:RegisterTargets","elasticloadbalancing:DeregisterTargets","lambda:InvokeFunction","s3:GetObject","s3:GetObjectVersion","s3:ListBucket","sns:Publish","tag:GetResources"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AWSConfigRulesExecutionRole",
        policy_id: "ANPAJ4OHQ6RDLJFJ2TRRM",
        default_version_id: "v3",
        published: "2016-03-25T17:59:36Z",
        revised: "2017-08-15T21:16:11Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["s3:GetObject"],"Resource":"arn:aws:s3:::config-bucket-*"},{"Effect":"Allow","Action":["config:Put*","config:Get*","config:List*","config:Describe*","config:BatchGetResourceConfig"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AWSGlueServiceRole",
        policy_id: "ANPAYH5QJZMWT2KVPNHLR",
        default_version_id: "v4",
        published: "2017-08-14T13:37:39Z",
        revised: "2021-01-19T18:05:24Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["glue:*","s3:GetBucketLocation","s3:ListBucket","s3:ListAllMyBuckets","s3:GetBucketAcl","ec2:DescribeVpcEndpoints","ec2:DescribeRouteTables","ec2:CreateNetworkInterface","ec2:DeleteNetworkInterface","ec2:DescribeNetworkInterfaces","ec2:DescribeSecurityGroups","ec2:DescribeSubnets","ec2:DescribeVpcAttribute","iam:ListRolePolicies","iam:GetRole","iam:GetRolePolicy","cloudwatch:PutMetricData"],"Resource":["*"]},{"Effect":"Allow","Action":["s3:CreateBucket"],"Resource":["arn:aws:s3:::aws-glue-*"]},{"Effect":"Allow","Action":["s3:GetObject","s3:PutObject","s3:DeleteObject"],"Resource":["arn:aws:s3:::aws-glue-*/*","arn:aws:s3:::*/*aws-glue-*/*"]},{"Effect":"Allow","Action":["s3:GetObject"],"Resource":["arn:aws:s3:::crawler-public*","arn:aws:s3:::aws-glue-*"]},{"Effect":"Allow","Action":["logs:CreateLogGroup","logs:CreateLogStream","logs:PutLogEvents"],"Resource":["arn:aws:logs:*:*:/aws-glue/*"]}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AWSLambdaBasicExecutionRole",
        policy_id: "ANPAJNCQGXC42545SKXIK",
        default_version_id: "v1",
        published: "2015-04-09T15:03:43Z",
        revised: "2015-04-09T15:03:43Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["logs:CreateLogGroup","logs:CreateLogStream","logs:PutLogEvents"],"Resource":"*"}]}"#,
    },
    // The execution-role policy published for a Lambda function that reads an Amazon
    // DynamoDB stream: the stream reads Lambda makes on the function's behalf, plus the log
    // writes every execution role carries.
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AWSLambdaDynamoDBExecutionRole",
        policy_id: "ANPAIP7WNAGMIPYNW4WQG",
        default_version_id: "v2",
        published: "2015-04-09T15:09:29Z",
        revised: "2018-05-24T18:18:50Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["dynamodb:DescribeStream","dynamodb:GetRecords","dynamodb:GetShardIterator","dynamodb:ListStreams","logs:CreateLogGroup","logs:CreateLogStream","logs:PutLogEvents"],"Resource":"*"}]}"#,
    },
    // The same, for an Amazon Kinesis data stream.
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AWSLambdaKinesisExecutionRole",
        policy_id: "ANPAJHOLKJPXV4GBRMJUQ",
        default_version_id: "v3",
        published: "2015-04-09T15:14:16Z",
        revised: "2018-11-19T20:09:24Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["kinesis:DescribeStream","kinesis:DescribeStreamSummary","kinesis:GetRecords","kinesis:GetShardIterator","kinesis:ListShards","kinesis:ListStreams","kinesis:SubscribeToShard","logs:CreateLogGroup","logs:CreateLogStream","logs:PutLogEvents"],"Resource":"*"}]}"#,
    },
    // The same, for a function configured to run inside a VPC: the interface calls Lambda
    // makes on the function's behalf to attach it to each subnet and to clean up after it.
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AWSLambdaVPCAccessExecutionRole",
        policy_id: "ANPAJVTME3YLEHUIWKTDA",
        default_version_id: "v3",
        published: "2016-02-11T23:15:26Z",
        revised: "2018-11-21T19:23:16Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["logs:CreateLogGroup","logs:CreateLogStream","logs:PutLogEvents","ec2:CreateNetworkInterface","ec2:DescribeNetworkInterfaces","ec2:DescribeSubnets","ec2:DeleteNetworkInterface","ec2:AssignPrivateIpAddresses","ec2:UnassignPrivateIpAddresses"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AWSTransferLoggingAccess",
        policy_id: "ANPAL8TWKJZQMT7PVNHLB",
        default_version_id: "v1",
        published: "2020-02-10T22:39:29Z",
        revised: "2020-02-10T22:39:29Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["logs:CreateLogGroup","logs:CreateLogStream","logs:DescribeLogStreams","logs:PutLogEvents"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AWS_ConfigRole",
        policy_id: "ANPAM5VZQTMWKJ4PVNHLY",
        default_version_id: "v38",
        published: "2015-10-20T19:29:00Z",
        revised: "2021-09-27T22:13:54Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cloudtrail:DescribeTrails","cloudtrail:GetTrailStatus","cloudwatch:DescribeAlarms","config:Put*","ec2:Describe*","iam:GetRole","iam:ListRoles","s3:GetBucketAcl","s3:GetBucketLocation","s3:ListAllMyBuckets","tag:GetResources"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AmazonAPIGatewayPushToCloudWatchLogs",
        policy_id: "ANPAN9WMJWQZKT6PVNHLC",
        default_version_id: "v1",
        published: "2015-11-13T19:04:00Z",
        revised: "2015-11-13T19:04:00Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["logs:CreateLogGroup","logs:CreateLogStream","logs:DescribeLogGroups","logs:DescribeLogStreams","logs:FilterLogEvents","logs:GetLogEvents","logs:PutLogEvents"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AmazonAppStreamServiceAccess",
        policy_id: "ANPAP2XQZJMWT5PVNHLRD",
        default_version_id: "v5",
        published: "2016-11-19T04:17:37Z",
        revised: "2021-07-23T18:02:16Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ec2:CreateNetworkInterface","ec2:DeleteNetworkInterface","ec2:DescribeNetworkInterfaces","ec2:DescribeSecurityGroups","ec2:DescribeSubnets","ec2:DescribeVpcs","s3:CreateBucket","s3:GetObject","s3:ListBucket","s3:PutObject"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AmazonDMSCloudWatchLogsRole",
        policy_id: "ANPAQ7YWKJZQMT7PVNHLB",
        default_version_id: "v3",
        published: "2016-01-19T18:40:00Z",
        revised: "2021-01-14T17:52:38Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["logs:CreateLogGroup","logs:CreateLogStream","logs:DescribeLogGroups","logs:DescribeLogStreams","logs:PutLogEvents"],"Resource":"arn:aws:logs:*:*:log-group:dms-tasks-*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AmazonDMSRedshiftS3Role",
        policy_id: "ANPAR4ZZQTMWKJ4PVNHLY",
        default_version_id: "v3",
        published: "2016-04-20T17:05:56Z",
        revised: "2019-08-28T18:34:02Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["s3:CreateBucket","s3:DeleteObject","s3:GetBucketLocation","s3:GetObject","s3:ListBucket","s3:PutObject"],"Resource":"arn:aws:s3:::dms-*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AmazonDMSVPCManagementRole",
        policy_id: "ANPAJHKIGMBQI4AEFFSYO",
        default_version_id: "v3",
        published: "2015-11-18T16:33:19Z",
        revised: "2016-05-23T16:29:57Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ec2:CreateNetworkInterface","ec2:DescribeAvailabilityZones","ec2:DescribeInternetGateways","ec2:DescribeSecurityGroups","ec2:DescribeSubnets","ec2:DescribeVpcs","ec2:DeleteNetworkInterface","ec2:ModifyNetworkInterfaceAttribute"],"Resource":"*"}]}"#,
    },
    // Recommended by the EKS add-on catalog for the `aws-ebs-csi-driver` add-on's
    // `ebs-csi-controller-sa` service account. The conditions are the published
    // policy's: the driver may create and delete only the volumes and snapshots it
    // tagged for itself.
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AmazonEBSCSIDriverPolicy",
        policy_id: "ANPAAK2ZQTMWK6PVNHLJR",
        default_version_id: "v3",
        published: "2022-05-26T19:23:18Z",
        revised: "2024-10-08T17:51:36Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ec2:CreateSnapshot","ec2:AttachVolume","ec2:DetachVolume","ec2:ModifyVolume","ec2:DescribeAvailabilityZones","ec2:DescribeInstances","ec2:DescribeSnapshots","ec2:DescribeTags","ec2:DescribeVolumes","ec2:DescribeVolumesModifications"],"Resource":"*"},{"Effect":"Allow","Action":["ec2:CreateTags"],"Resource":["arn:aws:ec2:*:*:volume/*","arn:aws:ec2:*:*:snapshot/*"],"Condition":{"StringEquals":{"ec2:CreateAction":["CreateVolume","CreateSnapshot"]}}},{"Effect":"Allow","Action":["ec2:DeleteTags"],"Resource":["arn:aws:ec2:*:*:volume/*","arn:aws:ec2:*:*:snapshot/*"]},{"Effect":"Allow","Action":["ec2:CreateVolume"],"Resource":"*","Condition":{"StringLike":{"aws:RequestTag/ebs.csi.aws.com/cluster":"true"}}},{"Effect":"Allow","Action":["ec2:CreateVolume"],"Resource":"*","Condition":{"StringLike":{"aws:RequestTag/CSIVolumeName":"*"}}},{"Effect":"Allow","Action":["ec2:DeleteVolume"],"Resource":"*","Condition":{"StringLike":{"ec2:ResourceTag/ebs.csi.aws.com/cluster":"true"}}},{"Effect":"Allow","Action":["ec2:DeleteVolume"],"Resource":"*","Condition":{"StringLike":{"ec2:ResourceTag/CSIVolumeName":"*"}}},{"Effect":"Allow","Action":["ec2:DeleteSnapshot"],"Resource":"*","Condition":{"StringLike":{"ec2:ResourceTag/CSIVolumeSnapshotName":"*"}}},{"Effect":"Allow","Action":["ec2:DeleteSnapshot"],"Resource":"*","Condition":{"StringLike":{"ec2:ResourceTag/ebs.csi.aws.com/cluster":"true"}}}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AmazonEC2ContainerServiceAutoscaleRole",
        policy_id: "ANPAS6BMJWQZKT6PVNHLC",
        default_version_id: "v1",
        published: "2016-05-12T23:50:57Z",
        revised: "2016-05-12T23:50:57Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cloudwatch:DescribeAlarms","cloudwatch:PutMetricAlarm","ecs:DescribeServices","ecs:UpdateService"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AmazonEC2ContainerServiceEventsRole",
        policy_id: "ANPAT3CQZJMWT5PVNHLRD",
        default_version_id: "v2",
        published: "2017-05-30T16:51:35Z",
        revised: "2020-10-02T18:18:11Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ecs:RunTask"],"Resource":"*"},{"Effect":"Allow","Action":["iam:PassRole"],"Resource":"*","Condition":{"StringLike":{"iam:PassedToService":"ecs-tasks.amazonaws.com"}}}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AmazonEC2ContainerServiceRole",
        policy_id: "ANPAJO53W2XHNACG7V77Q",
        default_version_id: "v2",
        published: "2015-04-09T16:14:19Z",
        revised: "2016-08-11T13:08:54Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ec2:AuthorizeSecurityGroupIngress","ec2:Describe*","elasticloadbalancing:DeregisterInstancesFromLoadBalancer","elasticloadbalancing:DeregisterTargets","elasticloadbalancing:Describe*","elasticloadbalancing:RegisterInstancesWithLoadBalancer","elasticloadbalancing:RegisterTargets"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AmazonEC2ContainerServiceforEC2Role",
        policy_id: "ANPAV8DWKJZQMT7PVNHLB",
        default_version_id: "v6",
        published: "2015-03-19T18:45:18Z",
        revised: "2024-08-06T18:14:32Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ec2:DescribeTags","ecr:BatchCheckLayerAvailability","ecr:BatchGetImage","ecr:GetAuthorizationToken","ecr:GetDownloadUrlForLayer","ecs:DeregisterContainerInstance","ecs:DiscoverPollEndpoint","ecs:Poll","ecs:RegisterContainerInstance","ecs:StartTelemetrySession","ecs:Submit*","ecs:UpdateContainerInstancesState","logs:CreateLogStream","logs:PutLogEvents"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AmazonEC2RoleforAWSCodeDeploy",
        policy_id: "ANPAW5FZQTMWKJ4PVNHLY",
        default_version_id: "v2",
        published: "2015-05-19T18:10:14Z",
        revised: "2017-03-20T17:14:10Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["s3:Get*","s3:List*"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AmazonEC2RoleforDataPipelineRole",
        policy_id: "ANPAX9GMJWQZKT6PVNHLC",
        default_version_id: "v3",
        published: "2015-02-06T18:41:23Z",
        revised: "2016-02-22T17:24:05Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cloudwatch:PutMetricData","datapipeline:*","dynamodb:*","ec2:Describe*","rds:Describe*","s3:*","sdb:*","sns:*","sqs:*"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AmazonEC2RoleforSSM",
        policy_id: "ANPAI6TL3SMY22S4KMMX6",
        default_version_id: "v9",
        published: "2015-05-29T17:48:35Z",
        revised: "2019-01-18T20:40:11Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ssm:DescribeAssociation","ssm:GetDeployablePatchSnapshotForInstance","ssm:GetDocument","ssm:DescribeDocument","ssm:GetManifest","ssm:GetParameter","ssm:GetParameters","ssm:ListAssociations","ssm:ListInstanceAssociations","ssm:PutInventory","ssm:PutComplianceItems","ssm:PutConfigurePackageResult","ssm:UpdateAssociationStatus","ssm:UpdateInstanceAssociationStatus","ssm:UpdateInstanceInformation"],"Resource":"*"},{"Effect":"Allow","Action":["ssmmessages:CreateControlChannel","ssmmessages:CreateDataChannel","ssmmessages:OpenControlChannel","ssmmessages:OpenDataChannel"],"Resource":"*"},{"Effect":"Allow","Action":["ec2messages:AcknowledgeMessage","ec2messages:DeleteMessage","ec2messages:FailMessage","ec2messages:GetEndpoint","ec2messages:GetMessages","ec2messages:SendReply"],"Resource":"*"},{"Effect":"Allow","Action":["cloudwatch:PutMetricData","ec2:DescribeInstanceStatus","ds:CreateComputer","ds:DescribeDirectories","logs:CreateLogGroup","logs:CreateLogStream","logs:DescribeLogGroups","logs:DescribeLogStreams","logs:PutLogEvents"],"Resource":"*"},{"Effect":"Allow","Action":["s3:GetBucketLocation","s3:PutObject","s3:GetObject","s3:GetEncryptionConfiguration","s3:AbortMultipartUpload","s3:ListMultipartUploadParts","s3:ListBucket","s3:ListBucketMultipartUploads"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AmazonEC2SpotFleetTaggingRole",
        policy_id: "ANPAY2HQZJMWT5PVNHLRD",
        default_version_id: "v4",
        published: "2017-07-20T20:49:42Z",
        revised: "2021-10-07T16:38:21Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ec2:CreateTags","ec2:DescribeImages","ec2:DescribeInstanceStatus","ec2:DescribeInstances","ec2:DescribeSubnets","ec2:RequestSpotInstances","ec2:RunInstances","ec2:TerminateInstances","elasticloadbalancing:RegisterInstancesWithLoadBalancer","elasticloadbalancing:RegisterTargets"],"Resource":"*"},{"Effect":"Allow","Action":["iam:PassRole"],"Resource":"*","Condition":{"StringEquals":{"iam:PassedToService":["ec2.amazonaws.com"]}}}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AmazonECSTaskExecutionRolePolicy",
        policy_id: "ANPABL4QJZMWT3KVPNHLY",
        default_version_id: "v1",
        published: "2017-11-16T18:48:22Z",
        revised: "2017-11-16T18:48:22Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ecr:GetAuthorizationToken","ecr:BatchCheckLayerAvailability","ecr:GetDownloadUrlForLayer","ecr:BatchGetImage","logs:CreateLogStream","logs:PutLogEvents"],"Resource":"*"}]}"#,
    },
    // Recommended by the EKS add-on catalog for the `aws-efs-csi-driver` add-on's
    // `efs-csi-controller-sa` and `efs-csi-node-sa` service accounts. The conditions
    // are the published
    // policy's: the driver may create an access point only while tagging it for its
    // cluster, and may tag or delete only an access point already carrying that tag.
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AmazonEFSCSIDriverPolicy",
        policy_id: "ANPACM6MZQWKJ5PVNHLTY",
        // AWS publishes this policy at its FIRST version — it has not been revised since
        // it was published — so the version a reader serves is `v1` and the two instants
        // are one and the same.
        default_version_id: "v1",
        published: "2022-11-15T20:17:44Z",
        revised: "2022-11-15T20:17:44Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["elasticfilesystem:DescribeAccessPoints","elasticfilesystem:DescribeFileSystems","elasticfilesystem:DescribeMountTargets","ec2:DescribeAvailabilityZones"],"Resource":"*"},{"Effect":"Allow","Action":["elasticfilesystem:CreateAccessPoint"],"Resource":"*","Condition":{"StringLike":{"aws:RequestTag/efs.csi.aws.com/cluster":"true"}}},{"Effect":"Allow","Action":"elasticfilesystem:TagResource","Resource":"*","Condition":{"StringLike":{"aws:ResourceTag/efs.csi.aws.com/cluster":"true"}}},{"Effect":"Allow","Action":"elasticfilesystem:DeleteAccessPoint","Resource":"*","Condition":{"StringEquals":{"aws:ResourceTag/efs.csi.aws.com/cluster":"true"}}}]}"#,
    },
    // The EMR service role's own policy, which AWS files under `/service-role/` —
    // the counterpart of the placement-group policy above, published at the root. The
    // two are the pair a client provisioning EMR attaches, and they sit under
    // DIFFERENT paths, so the catalog carries each under the one AWS publishes it at.
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AmazonEMRServicePolicy_v2",
        policy_id: "ANPAZ7JWKJZQMT7PVNHLB",
        default_version_id: "v6",
        published: "2021-07-13T19:50:41Z",
        revised: "2024-05-14T17:26:58Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ec2:CreateNetworkInterface","ec2:DeleteNetworkInterface","ec2:DescribeInstances","ec2:DescribeNetworkInterfaces","ec2:DescribeSecurityGroups","ec2:DescribeSubnets","ec2:DescribeVpcs","ec2:RunInstances","ec2:TerminateInstances","elasticmapreduce:ListInstances"],"Resource":"*","Condition":{"StringEquals":{"aws:ResourceTag/for-use-with-amazon-emr-managed-policies":"true"}}},{"Effect":"Allow","Action":["ec2:DescribeInstanceTypes","ec2:DescribeImages","s3:GetBucketLocation","s3:ListBucket"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AmazonElasticMapReduceEditorsRole",
        policy_id: "ANPAB6LZQTMWKJ4PVNHLY",
        default_version_id: "v2",
        published: "2018-11-16T21:55:14Z",
        revised: "2020-11-20T19:07:33Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ec2:CreateNetworkInterface","ec2:DeleteNetworkInterface","ec2:DescribeNetworkInterfaces","ec2:DescribeSubnets","ec2:DescribeVpcs","elasticmapreduce:ListClusters","elasticmapreduce:ListInstances","s3:GetObject","s3:ListBucket","s3:PutObject"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AmazonElasticMapReduceRole",
        policy_id: "ANPAIDI2BQT2LKXZG36TW",
        default_version_id: "v10",
        published: "2015-02-06T18:41:20Z",
        revised: "2020-06-24T22:24:20Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ec2:AuthorizeSecurityGroupEgress","ec2:AuthorizeSecurityGroupIngress","ec2:CancelSpotInstanceRequests","ec2:CreateNetworkInterface","ec2:CreateSecurityGroup","ec2:CreateTags","ec2:DeleteNetworkInterface","ec2:DeleteSecurityGroup","ec2:DeleteTags","ec2:Describe*","ec2:DetachNetworkInterface","ec2:ModifyImageAttribute","ec2:ModifyInstanceAttribute","ec2:RequestSpotInstances","ec2:RevokeSecurityGroupEgress","ec2:RunInstances","ec2:TerminateInstances","ec2:CreateVolume","ec2:DeleteVolume","ec2:AttachVolume","ec2:DetachVolume","iam:GetRole","iam:GetRolePolicy","iam:ListInstanceProfiles","iam:ListRolePolicies","iam:PassRole","s3:CreateBucket","s3:Get*","s3:List*","sdb:BatchPutAttributes","sdb:Select","sqs:CreateQueue","sqs:Delete*","sqs:GetQueue*","sqs:PurgeQueue","sqs:ReceiveMessage","cloudwatch:PutMetricAlarm","cloudwatch:DescribeAlarms","cloudwatch:DeleteAlarms","application-autoscaling:RegisterScalableTarget","application-autoscaling:DeregisterScalableTarget","application-autoscaling:PutScalingPolicy","application-autoscaling:DeleteScalingPolicy","application-autoscaling:Describe*"],"Resource":"*"}]}"#,
    },
    // The policy an EMR cluster's EC2 instances carry through their instance profile — the
    // counterpart of the EMR service role's own policy above, which AWS files under the
    // same path.
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AmazonElasticMapReduceforAutoScalingRole",
        policy_id: "ANPAC3NMJWQZKT6PVNHLC",
        default_version_id: "v1",
        published: "2016-11-18T01:09:10Z",
        revised: "2016-11-18T01:09:10Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cloudwatch:DescribeAlarms","elasticmapreduce:ListInstanceGroups","elasticmapreduce:ModifyInstanceGroups"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AmazonElasticMapReduceforEC2Role",
        policy_id: "ANPAIGALS5RCDLZLB3PGS",
        default_version_id: "v3",
        published: "2015-02-06T18:41:21Z",
        revised: "2017-08-11T23:57:30Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Resource":"*","Action":["cloudwatch:*","dynamodb:*","ec2:Describe*","elasticmapreduce:Describe*","elasticmapreduce:ListBootstrapActions","elasticmapreduce:ListClusters","elasticmapreduce:ListInstanceGroups","elasticmapreduce:ListInstances","elasticmapreduce:ListSteps","kinesis:CreateStream","kinesis:DeleteStream","kinesis:DescribeStream","kinesis:GetRecords","kinesis:GetShardIterator","kinesis:MergeShards","kinesis:PutRecord","kinesis:SplitShard","rds:Describe*","s3:*","sdb:*","sns:*","sqs:*","glue:CreateDatabase","glue:UpdateDatabase","glue:DeleteDatabase","glue:GetDatabase","glue:GetDatabases","glue:CreateTable","glue:UpdateTable","glue:DeleteTable","glue:GetTable","glue:GetTables","glue:GetTableVersions","glue:CreatePartition","glue:BatchCreatePartition","glue:UpdatePartition","glue:DeletePartition","glue:BatchDeletePartition","glue:GetPartition","glue:GetPartitions","glue:BatchGetPartition","glue:CreateUserDefinedFunction","glue:UpdateUserDefinedFunction","glue:DeleteUserDefinedFunction","glue:GetUserDefinedFunction","glue:GetUserDefinedFunctions"]}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AmazonElasticTranscoderRole",
        policy_id: "ANPAJ4TTBHLLSPNNAT5DO",
        default_version_id: "v2",
        published: "2015-02-06T18:41:24Z",
        revised: "2017-03-01T18:49:33Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["s3:Get*","s3:ListBucket","s3:Put*","s3:*MultipartUpload"],"Resource":"*"},{"Effect":"Allow","Action":"sns:Publish","Resource":"*"},{"Effect":"Deny","Action":["s3:*Delete*","s3:*Policy*","sns:*Delete*","sns:*Permission*","sns:*Remove*"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AmazonMacieServiceRole",
        policy_id: "ANPAI3AKJVQIFWNUMKFTS",
        default_version_id: "v1",
        published: "2017-08-14T14:53:30Z",
        revised: "2017-08-14T14:53:30Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["s3:Get*","s3:List*"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AmazonRDSDirectoryServiceAccess",
        policy_id: "ANPAD5PQZJMWT5PVNHLRD",
        default_version_id: "v1",
        published: "2016-02-26T02:35:59Z",
        revised: "2016-02-26T02:35:59Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ds:AuthorizeApplication","ds:DescribeDirectories","ds:GetAuthorizedApplicationDetails","ds:UnauthorizeApplication"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AmazonRDSEnhancedMonitoringRole",
        policy_id: "ANPAJ36T4OKJZUHBTRIAK",
        default_version_id: "v1",
        published: "2015-11-11T19:58:29Z",
        revised: "2015-11-11T19:58:29Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Sid":"EnableCreationAndManagementOfRDSCloudwatchLogGroups","Effect":"Allow","Action":["logs:CreateLogGroup","logs:PutRetentionPolicy"],"Resource":["arn:aws:logs:*:*:log-group:RDS*"]},{"Sid":"EnableCreationAndManagementOfRDSCloudwatchLogStreams","Effect":"Allow","Action":["logs:CreateLogStream","logs:PutLogEvents","logs:DescribeLogStreams","logs:GetLogEvents"],"Resource":["arn:aws:logs:*:*:log-group:RDS*:log-stream:*"]}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AmazonRekognitionServiceRole",
        policy_id: "ANPAF9QWKJZQMT7PVNHLB",
        default_version_id: "v3",
        published: "2017-11-29T18:42:07Z",
        revised: "2020-11-03T19:21:44Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["sns:Publish"],"Resource":"arn:aws:sns:*:*:AmazonRekognition*"},{"Effect":"Allow","Action":["kinesis:PutRecord","kinesis:PutRecords"],"Resource":"arn:aws:kinesis:*:*:stream/AmazonRekognition*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AmazonSNSRole",
        policy_id: "ANPAG2RZQTMWKJ4PVNHLY",
        default_version_id: "v1",
        published: "2015-02-06T18:41:28Z",
        revised: "2015-02-06T18:41:28Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["logs:CreateLogGroup","logs:CreateLogStream","logs:DescribeLogStreams","logs:PutLogEvents","logs:PutMetricFilter","logs:PutRetentionPolicy"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AmazonSSMAutomationRole",
        policy_id: "ANPAH6SMJWQZKT6PVNHLC",
        default_version_id: "v7",
        published: "2015-11-13T19:18:24Z",
        revised: "2018-10-04T17:44:51Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cloudformation:CreateStack","cloudformation:DeleteStack","cloudformation:DescribeStacks","ec2:CreateImage","ec2:DescribeImages","ec2:DescribeInstanceStatus","ec2:DescribeInstances","ec2:StartInstances","ec2:StopInstances","ec2:TerminateInstances","ssm:DescribeInstanceInformation","ssm:GetAutomationExecution","ssm:ListCommandInvocations","ssm:ListCommands","ssm:SendCommand","ssm:StartAutomationExecution"],"Resource":"*"},{"Effect":"Allow","Action":["lambda:InvokeFunction"],"Resource":"arn:aws:lambda:*:*:function:Automation*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AmazonSSMMaintenanceWindowRole",
        policy_id: "ANPAJ3TQZJMWT5PVNHLRD",
        default_version_id: "v3",
        published: "2017-08-09T20:49:14Z",
        revised: "2019-01-17T20:08:36Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["resource-groups:ListGroupResources","ssm:CancelCommand","ssm:GetAutomationExecution","ssm:GetCommandInvocation","ssm:ListCommandInvocations","ssm:ListCommands","ssm:SendCommand","ssm:StartAutomationExecution","states:DescribeExecution","states:StartExecution","tag:GetResources"],"Resource":"*"},{"Effect":"Allow","Action":["lambda:InvokeFunction"],"Resource":"arn:aws:lambda:*:*:function:SSM*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AmazonTextractServiceRole",
        policy_id: "ANPAK7VWKJZQMT7PVNHLB",
        default_version_id: "v1",
        published: "2019-05-29T21:01:17Z",
        revised: "2019-05-29T21:01:17Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["sns:Publish"],"Resource":"arn:aws:sns:*:*:AmazonTextract*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "AutoScalingNotificationAccessRole",
        policy_id: "ANPAIO2VMUPGDC5PZVXVA",
        default_version_id: "v1",
        published: "2015-02-06T18:41:22Z",
        revised: "2015-02-06T18:41:22Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["sqs:SendMessage","sqs:GetQueueUrl","sns:Publish"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "CloudWatchEventsBuiltInTargetExecutionAccess",
        policy_id: "ANPAL4WZQTMWKJ4PVNHLY",
        default_version_id: "v1",
        published: "2016-01-14T18:35:03Z",
        revised: "2016-01-14T18:35:03Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ec2:CreateSnapshot","ec2:Describe*","ec2:RebootInstances","ec2:StopInstances","ec2:TerminateInstances"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "CloudWatchEventsInvocationAccess",
        policy_id: "ANPAJPZ67RUEZDTMCUDVC",
        default_version_id: "v1",
        published: "2016-01-14T18:35:28Z",
        revised: "2016-01-14T18:35:28Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["kinesis:PutRecord"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "ComprehendDataAccessRolePolicy",
        policy_id: "ANPAM9XMJWQZKT6PVNHLC",
        default_version_id: "v2",
        published: "2018-11-28T17:14:09Z",
        revised: "2020-09-02T19:36:28Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["s3:GetObject","s3:ListBucket"],"Resource":["arn:aws:s3:::*comprehend*"]},{"Effect":"Allow","Action":["s3:PutObject"],"Resource":["arn:aws:s3:::*comprehend*"]}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "RDSCloudHsmAuthorizationRole",
        policy_id: "ANPAN5YQZJMWT5PVNHLRD",
        default_version_id: "v1",
        published: "2015-02-06T18:41:29Z",
        revised: "2015-02-06T18:41:29Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["cloudhsm:CreateLunaClient","cloudhsm:DeleteLunaClient","cloudhsm:DescribeHapg","cloudhsm:DescribeHsm","cloudhsm:DescribeLunaClient","cloudhsm:GetConfig","cloudhsm:ListAvailableZones","cloudhsm:ListHapgs","cloudhsm:ListHsms","cloudhsm:ListLunaClients","cloudhsm:ModifyHapg","cloudhsm:ModifyLunaClient"],"Resource":"*"}]}"#,
    },
    AwsManagedPolicy {
        path: "/service-role/",
        policy_name: "VMImportExportRoleForAWSConnector",
        policy_id: "ANPAP8ZWKJZQMT7PVNHLB",
        default_version_id: "v1",
        published: "2015-09-03T20:48:59Z",
        revised: "2015-09-03T20:48:59Z",
        is_attachable: true,
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["s3:GetBucketAcl","s3:GetBucketLocation","s3:GetObject","s3:ListBucket","s3:PutObject"],"Resource":["arn:aws:s3:::import-to-ec2-*"]},{"Effect":"Allow","Action":["ec2:CancelConversionTask","ec2:CreateTags","ec2:DescribeConversionTasks","ec2:DescribeImages","ec2:DescribeInstances","ec2:DescribeSnapshots","ec2:DescribeTags","ec2:ImportInstance","ec2:ImportVolume"],"Resource":"*"}]}"#,
    },
];

/// The catalog entry `arn` names, or `None` when `arn` names no AWS-managed policy
/// this emulator carries.
///
/// The ARN is read through the one grammar this crate holds
/// ([`parse_policy_arn`]), so a value that is not a managed-policy ARN at all names
/// nothing here either; the account segment is what says the ARN is in AWS's
/// namespace rather than an account's, and the entry answers only when its own ARN
/// is exactly the one asked for — the same identity test
/// [`ComAmazonawsIamAwsidentitymanagementv20100508State::policy_by_arn`] applies to
/// the account's policies.
pub fn aws_managed_policy(arn: &str) -> Option<&'static AwsManagedPolicy> {
    let parsed = parse_policy_arn(arn)?;
    if parsed.account != AWS_MANAGED_POLICY_ACCOUNT {
        return None;
    }
    AWS_MANAGED_POLICIES
        .iter()
        .find(|entry| entry.path == parsed.path && entry.policy_name == parsed.policy_name)
        .filter(|entry| entry.arn() == arn)
}

/// Whether `arn` is an ARN in AWS's own managed-policy namespace — well formed and
/// carrying [`AWS_MANAGED_POLICY_ACCOUNT`] as its account — whether or not
/// [`AWS_MANAGED_POLICIES`] carries that policy.
///
/// This is what separates "AWS owns this policy" from "this account owns it": the
/// write operations refuse an AWS-owned ARN as an out-of-range value for their
/// parameter rather than as a missing entity, because no account can create, change
/// or delete a policy AWS owns.
pub fn is_aws_managed_policy_arn(arn: &str) -> bool {
    parse_policy_arn(arn).is_some_and(|parsed| parsed.account == AWS_MANAGED_POLICY_ACCOUNT)
}

/// The catalog entry published under the friendly name `policy_name`, whatever path
/// AWS files it under, or `None` when the catalog carries no policy of that name.
///
/// A managed policy's friendly name identifies ONE policy in the namespace that owns
/// it — IAM holds a single policy per name whichever path it is filed under, which is
/// why the account's own policies are keyed by name alone
/// ([`ComAmazonawsIamAwsidentitymanagementv20100508State::policies`]) — so the name is
/// enough to say WHICH policy an ARN means to name, and the entry it answers with is
/// the one whose path that ARN has to carry to name it.
///
/// Matched without regard to ASCII case, the folding every IAM name is keyed under
/// ([`name_key`]).
pub fn aws_managed_policy_named(policy_name: &str) -> Option<&'static AwsManagedPolicy> {
    AWS_MANAGED_POLICIES
        .iter()
        .find(|entry| entry.policy_name.eq_ignore_ascii_case(policy_name))
}

/// Whether `arn`, an ARN in AWS's own managed-policy namespace, names a policy AWS
/// publishes.
///
/// An AWS-managed policy is not account state, so what the account holds says nothing
/// about it; the catalog does. A policy's identity is its path AND its friendly name
/// (the two its ARN spells), and a name names one policy
/// ([`aws_managed_policy_named`]), so an ARN that spells a carried policy's name under
/// any other path names NO policy at all — the path is not a hint AWS ignores, it is
/// part of the published identifier.
///
/// [`AWS_MANAGED_POLICIES`] is a SUBSET of AWS's catalog, which runs to well over a
/// thousand policies, so a name the catalog does not carry cannot be judged: such an
/// ARN is taken as published rather than refusing an attachment AWS accepts. What the
/// catalog knows, it is authoritative about — including where the policy it knows
/// lives.
pub fn is_published_aws_managed_policy_arn(arn: &str) -> bool {
    let Some(parsed) = parse_policy_arn(arn) else {
        return false;
    };
    if parsed.account != AWS_MANAGED_POLICY_ACCOUNT {
        return false;
    }
    match aws_managed_policy_named(&parsed.policy_name) {
        Some(entry) => entry.arn() == arn,
        None => true,
    }
}
