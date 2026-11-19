// SPDX-License-Identifier: CC0-1.0

//! The catalog of AWS managed permissions this service ships.
//!
//! RAM maintains managed permissions of its own for the resource types it shares, and
//! those permissions belong to Amazon Web Services rather than to an account: their
//! ARNs name no Region and carry `aws` where an account id would be
//! (`arn:aws:ram::aws:permission/<PermissionName>`), their `permissionType` is
//! `AWS_MANAGED` — "Amazon Web Services created and manages this managed permission.
//! You can associate it with your resource shares, but you can't modify it"
//! (`ResourceSharePermissionDetail$permissionType`) — and they exist before any caller
//! creates anything. So the store holds them from the moment it is first touched
//! (`ComAmazonawsRamAmazonresourcesharingState::default`), beside the customer managed
//! permissions `CreatePermission` mints, and every read that resolves a permission ARN
//! resolves these too.
//!
//! Source of the table below: the managed permissions AWS Resource Access Manager
//! publishes for its shareable resource types — the RAM Permissions library, which
//! `ListPermissions` with `permissionType` `AWS` reports and which
//! `GetPermission$permissionArn` points a caller at to discover them ("To find the ARN
//! for a permission, use either the ListPermissions operation or go to the Permissions
//! library page in the RAM console") — one entry per published permission, with the
//! resource type it applies to, the actions its policy allows and whether it is that
//! resource type's default. The model enumerates none of them, so the catalog is
//! written out here, exactly as the resource-type catalog in [`super::RESOURCE_TYPES`]
//! is; every entry's resource type is one of that catalog's.
//!
//! MEMBERSHIP is part of that data. `ListPermissions` is the only enumeration API for
//! managed permissions, so the catalog carries an entry per permission the library
//! publishes — a resource type whose owning service maintains a family of certificate
//! templates gets one entry per published permission of that family, the revocation
//! permission beside the issuance ones — and a name no permission is published under
//! resolves nowhere, so an ARN a caller discovers here can be attached.
//!
//! So is each entry's published VERSION ([`AwsManagedPermission::version`]). The library
//! revises a managed permission in place and publishes the revision as the permission's
//! version, so an entry carries the version the library publishes it at rather than one
//! number for the whole catalog: that is the version every read of it reports and the
//! default version a share attaches.

use std::collections::BTreeMap;

use super::{
    PermissionRecord, PermissionVersionRecord, ARN_SERVICE, FIRST_PERMISSION_VERSION,
    PERMISSION_ARN_RESOURCE_PREFIX, PERMISSION_FEATURE_SET_STANDARD, PERMISSION_STATUS_ATTACHABLE,
    POLICY_EFFECT_ALLOW, POLICY_TEMPLATE_ACTION_ELEMENT, POLICY_TEMPLATE_EFFECT_ELEMENT,
};

/// The `PermissionType` of a permission Amazon Web Services created and manages, the
/// other value of the modelled `PermissionType` enum beside
/// [`super::PERMISSION_TYPE_CUSTOMER_MANAGED`].
pub const PERMISSION_TYPE_AWS_MANAGED: &str = "AWS_MANAGED";

/// The partition field of every ARN this emulator reports.
pub const ARN_PARTITION: &str = "aws";

/// What an AWS managed permission ARN carries where an account id would be: the
/// permission is Amazon Web Services' own rather than any account's.
pub const AWS_MANAGED_PERMISSION_ARN_ACCOUNT: &str = "aws";

/// One entry of the AWS managed permission catalog.
#[derive(Debug, Clone, Copy)]
pub struct AwsManagedPermission {
    /// The permission's name, which is also the resource id of its ARN.
    pub name: &'static str,
    /// The resource type it applies to, `<service-code>:<resource-code>`; one of
    /// [`super::RESOURCE_TYPES`].
    pub resource_type: &'static str,
    /// The actions its policy allows, which is what the `permission` document reports.
    pub actions: &'static [&'static str],
    /// Whether this is "the default managed permission for all resources of this
    /// resource type" (`ResourceSharePermissionDetail$isResourceTypeDefault`) — the
    /// one RAM attaches when a share names no permission for the type.
    pub is_resource_type_default: bool,
    /// Whether Amazon Web Services publishes this permission for service-linked
    /// resource shares only — a share a service created on the account's behalf — so a
    /// caller cannot attach it to a share of their own. A published property of each
    /// permission, reported beside the modelled members of every summary it appears in.
    pub restrict_to_service_linked_shares: bool,
    /// The version Amazon Web Services publishes this permission at, which is therefore
    /// its default version: a permission the library has revised since it was first
    /// published is published at the revision, and a read of it reports that number
    /// (`ResourceSharePermissionSummary$version`). Counting from
    /// [`FIRST_PERMISSION_VERSION`], the version every permission starts at.
    pub version: i32,
}

/// Whether a permission the catalog publishes for any resource share, which is what
/// every entry below is, is confined to service-linked shares: it is not.
const ATTACHABLE_TO_ANY_SHARE: bool = false;

/// The resource type a private certificate authority's ARN names, spelled as
/// [`super::RESOURCE_TYPES`] publishes it. Named once because the permissions Amazon Web
/// Services publishes for it are a family rather than a single entry.
const CERTIFICATE_AUTHORITY_RESOURCE_TYPE: &str = "acm-pca:CertificateAuthority";

/// The actions every managed permission for ISSUING a certificate of a private
/// certificate authority allows: the reads a receiving account makes about the shared CA,
/// plus the issuance the share exists to grant.
///
/// The issuance permissions of that family differ from one another in the certificate
/// TEMPLATE a condition on the issuance admits — which is the `Condition` element of each
/// published policy, not its `Action` element — so every issuance entry of the family
/// allows the same actions. A catalog record carries a permission's effect and its actions
/// (`ResourceSharePermissionDetail$permission`, "The permission's effect and actions in JSON
/// format"), so the template condition is not a field of one.
const CERTIFICATE_AUTHORITY_ISSUANCE_ACTIONS: &[&str] = &[
    "acm-pca:DescribeCertificateAuthority",
    "acm-pca:GetCertificate",
    "acm-pca:GetCertificateAuthorityCertificate",
    "acm-pca:IssueCertificate",
    "acm-pca:ListPermissions",
    "acm-pca:ListTags",
];

/// The actions the managed permission for REVOKING a certificate of a shared private
/// certificate authority allows: the same reads the issuance permissions of the family
/// allow, with the revocation the permission exists to grant in place of the issuance.
const CERTIFICATE_AUTHORITY_REVOCATION_ACTIONS: &[&str] = &[
    "acm-pca:DescribeCertificateAuthority",
    "acm-pca:GetCertificate",
    "acm-pca:GetCertificateAuthorityCertificate",
    "acm-pca:ListPermissions",
    "acm-pca:ListTags",
    "acm-pca:RevokeCertificate",
];

/// The AWS managed permissions this service serves, one entry per published
/// permission. See the module documentation for the source.
pub const AWS_MANAGED_PERMISSIONS: &[AwsManagedPermission] = &[
    entry(
        "AWSRAMDefaultPermissionCertificateAuthority",
        CERTIFICATE_AUTHORITY_RESOURCE_TYPE,
        CERTIFICATE_AUTHORITY_ISSUANCE_ACTIONS,
        true,
    ),
    entry(
        "AWSRAMBlankEndEntityCertificateAPIPassthroughIssuanceCertificateAuthority",
        CERTIFICATE_AUTHORITY_RESOURCE_TYPE,
        CERTIFICATE_AUTHORITY_ISSUANCE_ACTIONS,
        false,
    ),
    entry(
        "AWSRAMBlankEndEntityCertificateAPICSRPassthroughIssuanceCertificateAuthority",
        CERTIFICATE_AUTHORITY_RESOURCE_TYPE,
        CERTIFICATE_AUTHORITY_ISSUANCE_ACTIONS,
        false,
    ),
    entry(
        "AWSRAMSubordinateCACertificatePathLen0IssuanceCertificateAuthority",
        CERTIFICATE_AUTHORITY_RESOURCE_TYPE,
        CERTIFICATE_AUTHORITY_ISSUANCE_ACTIONS,
        false,
    ),
    entry(
        "AWSRAMSubordinateCACertificatePathLen1IssuanceCertificateAuthority",
        CERTIFICATE_AUTHORITY_RESOURCE_TYPE,
        CERTIFICATE_AUTHORITY_ISSUANCE_ACTIONS,
        false,
    ),
    entry(
        "AWSRAMSubordinateCACertificatePathLen2IssuanceCertificateAuthority",
        CERTIFICATE_AUTHORITY_RESOURCE_TYPE,
        CERTIFICATE_AUTHORITY_ISSUANCE_ACTIONS,
        false,
    ),
    entry(
        "AWSRAMSubordinateCACertificatePathLen3IssuanceCertificateAuthority",
        CERTIFICATE_AUTHORITY_RESOURCE_TYPE,
        CERTIFICATE_AUTHORITY_ISSUANCE_ACTIONS,
        false,
    ),
    entry(
        "AWSRAMRevokeCertificateCertificateAuthority",
        CERTIFICATE_AUTHORITY_RESOURCE_TYPE,
        CERTIFICATE_AUTHORITY_REVOCATION_ACTIONS,
        false,
    ),
    entry(
        "APIGatewayPrivateDomainNameManagementPolicyDefaultPermission",
        "apigateway:Domainnames",
        &["apigateway:CreateAccessAssociation"],
        true,
    ),
    entry_at_version(
        "AWSRAMDefaultPermissionAppMesh",
        "appmesh:Mesh",
        &[
            "appmesh:CreateVirtualNode",
            "appmesh:DescribeMesh",
            "appmesh:DescribeVirtualNode",
            "appmesh:ListVirtualNodes",
        ],
        true,
        3,
    ),
    entry(
        "AWSRAMDefaultPermissionCodeBuildProject",
        "codebuild:Project",
        &[
            "codebuild:BatchGetProjects",
            "codebuild:ListBuildsForProject",
            "codebuild:StartBuild",
        ],
        true,
    ),
    entry(
        "AWSRAMDefaultPermissionCodeBuildReportGroup",
        "codebuild:ReportGroup",
        &[
            "codebuild:BatchGetReportGroups",
            "codebuild:BatchGetReports",
            "codebuild:ListReportsForReportGroup",
        ],
        true,
    ),
    entry_at_version(
        "AWSRAMDefaultPermissionCapacityReservation",
        "ec2:CapacityReservation",
        &["ec2:DescribeCapacityReservations", "ec2:RunInstances"],
        true,
        2,
    ),
    entry(
        "AWSRAMDefaultPermissionDedicatedHost",
        "ec2:DedicatedHost",
        &["ec2:DescribeHosts", "ec2:RunInstances"],
        true,
    ),
    entry(
        "AWSRAMDefaultPermissionIpamPool",
        "ec2:IpamPool",
        &[
            "ec2:AllocateIpamPoolCidr",
            "ec2:DescribeIpamPools",
            "ec2:GetIpamPoolAllocations",
            "ec2:GetIpamPoolCidrs",
        ],
        true,
    ),
    entry(
        "AWSRAMDefaultPermissionPrefixList",
        "ec2:PrefixList",
        &[
            "ec2:DescribeManagedPrefixLists",
            "ec2:GetManagedPrefixListEntries",
        ],
        true,
    ),
    entry(
        "AWSRAMDefaultPermissionSubnet",
        "ec2:Subnet",
        &[
            "ec2:CreateNetworkInterface",
            "ec2:DescribeSubnets",
            "ec2:RunInstances",
        ],
        true,
    ),
    entry_at_version(
        "AWSRAMDefaultPermissionTransitGateway",
        "ec2:TransitGateway",
        &[
            "ec2:CreateTransitGatewayVpcAttachment",
            "ec2:DeleteTransitGatewayVpcAttachment",
            "ec2:DescribeTransitGatewayVpcAttachments",
            "ec2:DescribeTransitGateways",
        ],
        true,
        3,
    ),
    entry(
        "AWSRAMDefaultPermissionGlueDatabase",
        "glue:Database",
        &[
            "glue:GetDatabase",
            "glue:GetDatabases",
            "glue:GetTable",
            "glue:GetTables",
        ],
        true,
    ),
    entry_at_version(
        "AWSRAMPermissionGlueDatabaseReadWrite",
        "glue:Database",
        &[
            "glue:CreateTable",
            "glue:DeleteTable",
            "glue:GetDatabase",
            "glue:GetTable",
            "glue:UpdateTable",
        ],
        false,
        3,
    ),
    entry(
        "AWSRAMDefaultPermissionGlueTable",
        "glue:Table",
        &[
            "glue:GetPartition",
            "glue:GetPartitions",
            "glue:GetTable",
            "glue:GetTables",
        ],
        true,
    ),
    entry(
        "AWSRAMDefaultPermissionImageBuilderImage",
        "imagebuilder:Image",
        &[
            "imagebuilder:GetImage",
            "imagebuilder:ListImageBuildVersions",
            "imagebuilder:ListImages",
        ],
        true,
    ),
    entry(
        "AWSRAMDefaultPermissionLicenseConfiguration",
        "license-manager:LicenseConfiguration",
        &[
            "license-manager:GetLicenseConfiguration",
            "license-manager:ListLicenseSpecificationsForResource",
            "license-manager:ListUsageForLicenseConfiguration",
            "license-manager:UpdateLicenseSpecificationsForResource",
        ],
        true,
    ),
    entry(
        "AWSRAMDefaultPermissionNetworkFirewallFirewallPolicy",
        "network-firewall:FirewallPolicy",
        &[
            "network-firewall:AssociateFirewallPolicy",
            "network-firewall:DescribeFirewallPolicy",
            "network-firewall:ListFirewallPolicies",
        ],
        true,
    ),
    entry(
        "AWSRAMDefaultPermissionStatefulRuleGroup",
        "network-firewall:StatefulRuleGroup",
        &[
            "network-firewall:DescribeRuleGroup",
            "network-firewall:ListRuleGroups",
        ],
        true,
    ),
    entry(
        "AWSRAMDefaultPermissionOutpost",
        "outposts:Outpost",
        &[
            "outposts:GetOutpost",
            "outposts:GetOutpostInstanceTypes",
            "outposts:ListOutposts",
        ],
        true,
    ),
    entry(
        "AWSRAMDefaultPermissionSite",
        "outposts:Site",
        &["outposts:GetSite", "outposts:ListSites"],
        true,
    ),
    entry(
        "AWSRAMDefaultPermissionResourceGroup",
        "resource-groups:Group",
        &[
            "resource-groups:GetGroup",
            "resource-groups:GetGroupConfiguration",
            "resource-groups:ListGroupResources",
        ],
        true,
    ),
    entry(
        "AWSRAMDefaultPermissionResolverRule",
        "route53resolver:ResolverRule",
        &[
            "route53resolver:AssociateResolverRule",
            "route53resolver:DisassociateResolverRule",
            "route53resolver:GetResolverRule",
            "route53resolver:ListResolverRuleAssociations",
        ],
        true,
    ),
    entry(
        "AWSRAMDefaultPermissionVpcLatticeServiceNetwork",
        "vpc-lattice:ServiceNetwork",
        &[
            "vpc-lattice:CreateServiceNetworkVpcAssociation",
            "vpc-lattice:GetServiceNetwork",
            "vpc-lattice:ListServiceNetworks",
        ],
        true,
    ),
];

/// One catalog entry, so [`AWS_MANAGED_PERMISSIONS`] reads as the table it is.
///
/// Every entry the catalog publishes is one a caller may attach to a share of their own
/// ([`ATTACHABLE_TO_ANY_SHARE`]); the restriction is a per-permission property of the
/// record ([`super::PermissionRecord::restrict_to_service_linked_shares`]) rather than
/// of the catalog, so a permission that carries it reports it without the table having
/// to.
const fn entry(
    name: &'static str,
    resource_type: &'static str,
    actions: &'static [&'static str],
    is_resource_type_default: bool,
) -> AwsManagedPermission {
    entry_at_version(
        name,
        resource_type,
        actions,
        is_resource_type_default,
        FIRST_PERMISSION_VERSION,
    )
}

/// One catalog entry whose permission the library has REVISED since it was first
/// published, so the version it publishes — and the one every read of it reports — is
/// `version` rather than the first.
const fn entry_at_version(
    name: &'static str,
    resource_type: &'static str,
    actions: &'static [&'static str],
    is_resource_type_default: bool,
    version: i32,
) -> AwsManagedPermission {
    AwsManagedPermission {
        name,
        resource_type,
        actions,
        is_resource_type_default,
        restrict_to_service_linked_shares: ATTACHABLE_TO_ANY_SHARE,
        version,
    }
}

/// The ARN of the AWS managed permission `name`.
///
/// An AWS managed permission is Amazon Web Services' own and exists in every Region,
/// so — unlike [`super::permission_arn`], which names a customer managed permission in
/// one account and Region — its ARN carries no Region and
/// [`AWS_MANAGED_PERMISSION_ARN_ACCOUNT`] in place of an account id.
pub fn aws_managed_permission_arn(name: &str) -> String {
    format!(
        "arn:{ARN_PARTITION}:{ARN_SERVICE}::{AWS_MANAGED_PERMISSION_ARN_ACCOUNT}:\
         {PERMISSION_ARN_RESOURCE_PREFIX}{name}"
    )
}

impl AwsManagedPermission {
    /// This entry's ARN.
    pub fn arn(&self) -> String {
        aws_managed_permission_arn(self.name)
    }

    /// The policy document this entry's version reports: the effect and the actions,
    /// the two elements `ResourceSharePermissionDetail$permission` describes ("The
    /// permission's effect and actions in JSON format").
    pub fn policy_document(&self) -> String {
        ::serde_json::json!({
            POLICY_TEMPLATE_EFFECT_ELEMENT: POLICY_EFFECT_ALLOW,
            POLICY_TEMPLATE_ACTION_ELEMENT: self.actions,
        })
        .to_string()
    }

    /// This entry as the record the store holds, created at `now`.
    ///
    /// Amazon Web Services publishes each of these permissions at one version
    /// ([`AwsManagedPermission::version`]), which is therefore its default version, and
    /// that version is `ATTACHABLE`: it "can be associated with resource shares"
    /// (`ResourceSharePermissionDetail$status`), which is the whole point of the catalog.
    /// A permission nobody can modify carries no tags.
    fn record(&self, now: i64) -> PermissionRecord {
        let version = self.version;
        PermissionRecord {
            arn: self.arn(),
            name: self.name.to_string(),
            resource_type: self.resource_type.to_string(),
            permission_type: PERMISSION_TYPE_AWS_MANAGED.to_string(),
            feature_set: PERMISSION_FEATURE_SET_STANDARD.to_string(),
            tags: BTreeMap::new(),
            creation_time: now,
            last_updated_time: now,
            default_version: version,
            is_resource_type_default: self.is_resource_type_default,
            restrict_to_service_linked_shares: self.restrict_to_service_linked_shares,
            versions: [(
                version,
                PermissionVersionRecord {
                    version,
                    policy_template: self.policy_document(),
                    status: PERMISSION_STATUS_ATTACHABLE.to_string(),
                    creation_time: now,
                    last_updated_time: now,
                },
            )]
            .into_iter()
            .collect(),
            // A permission Amazon Web Services manages is one no caller can delete, so
            // no catalog entry is ever on the delete path.
            deletion: None,
        }
    }
}

/// The whole catalog as the records the store starts from, keyed by ARN exactly as the
/// customer managed permissions beside them are, so one lookup resolves either kind.
pub fn catalog_records(now: i64) -> BTreeMap<String, PermissionRecord> {
    AWS_MANAGED_PERMISSIONS
        .iter()
        .map(|entry| (entry.arn(), entry.record(now)))
        .collect()
}

/// The AWS managed permission RAM attaches to a share that shares a resource of
/// `resource_type` and names no permission for that type: the catalog entry that is
/// "the default managed permission for all resources of this resource type"
/// ([`AwsManagedPermission::is_resource_type_default`]), or `None` when the catalog
/// publishes no permission for the type at all.
///
/// The type is matched the way [`super::resource_type_matches`] matches one, so the
/// spelling read off a shared resource's ARN finds the entry the catalog publishes
/// under the display spelling of the same type.
pub fn default_aws_managed_permission(
    resource_type: &str,
) -> Option<&'static AwsManagedPermission> {
    AWS_MANAGED_PERMISSIONS.iter().find(|entry| {
        entry.is_resource_type_default
            && super::resource_type_matches(entry.resource_type, resource_type)
    })
}

