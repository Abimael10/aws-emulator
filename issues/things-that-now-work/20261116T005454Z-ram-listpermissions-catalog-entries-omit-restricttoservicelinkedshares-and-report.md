Issue-Source: trace-sweep-divergence
Issue-Trace: ram-catalog-list-permissions
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ram-catalog-list-permissions
Issue-Shape-Id: com.amazonaws.ram#AmazonResourceSharing
Issue-Key: ram|ListPermissions|2xx->2xx|$.permissions[arn="arn:aws:ram::aws:permission/AWSRAMBlankEndEntityCertificateAPICSRPassthroughIssuanceCertificateAuthority"].restrictToServiceLinkedShares
Issue-Key: ram|ListPermissions|2xx->2xx|$.permissions[arn="arn:aws:ram::aws:permission/AWSRAMBlankEndEntityCertificateAPIPassthroughIssuanceCertificateAuthority"].restrictToServiceLinkedShares
Issue-Key: ram|ListPermissions|2xx->2xx|$.permissions[arn="arn:aws:ram::aws:permission/AWSRAMDefaultPermissionCertificateAuthority"].restrictToServiceLinkedShares
Issue-Key: ram|ListPermissions|2xx->2xx|$.permissions[arn="arn:aws:ram::aws:permission/AWSRAMSubordinateCACertificatePathLen0IssuanceCertificateAuthority"].restrictToServiceLinkedShares
Issue-Key: ram|ListPermissions|2xx->2xx|$.permissions[arn="arn:aws:ram::aws:permission/AWSRAMSubordinateCACertificatePathLen1IssuanceCertificateAuthority"]
Issue-Key: ram|ListPermissions|2xx->2xx|$.permissions[arn="arn:aws:ram::aws:permission/AWSRAMSubordinateCACertificatePathLen2IssuanceCertificateAuthority"]
Issue-Key: ram|ListPermissions|2xx->2xx|$.permissions[arn="arn:aws:ram::aws:permission/AWSRAMSubordinateCACertificatePathLen3IssuanceCertificateAuthority"]
Issue-Related: 20261115T141853Z-ram-listpermissions-serves-an-incomplete-aws-managed-permission.md
Issue-Commit: f672b96071627a2f926b4576526626783c2d131c

# ram ListPermissions catalog entries omit restrictToServiceLinkedShares and report stale versions, and most AWS_MANAGED permission ARNs are absent

## Resolution

Every key this issue carries is addressed — four are fixed and three are declined with their reason
below:

- `restrictToServiceLinkedShares` is now reported on every `ResourceSharePermissionSummary` this
  service serves (ListPermissions, ListPermissionVersions, ListResourceSharePermissions and
  CreatePermission share the one projection), read off the stored permission
  (`PermissionRecord::restrict_to_service_linked_shares`); the shape's wire form is hand-written in
  `src/ops/types/permission_summary_wire.rs` because the on-disk model declares no such member.
- Each catalog entry now carries the version the permission library publishes it at
  (`AwsManagedPermission::version`) instead of one number for the whole catalog.
- The private-CA family gained the revocation permission
  (`AWSRAMRevokeCertificateCertificateAuthority`), which the library publishes for
  `acm-pca:CertificateAuthority` and the catalog did not carry.

DECLINED, with reason: the three whole-object keys for
`AWSRAMSubordinateCACertificatePathLen1/2/3IssuanceCertificateAuthority`. The permission library
publishes one subordinate-CA issuance permission per published certificate template (path lengths
zero through three) and the catalog serves all four, so what the emulator reports for those ARNs is
what AWS publishes; the recorded page of this read stops at the path-length-zero sibling and carries
none of the three, which is a property of that page rather than of the response. Removing them would
make a published, attachable permission ARN unresolvable, so they stay.

STILL OPEN, under another issue: the membership-completeness part of this report — the ~127 further
published AWS_MANAGED ARNs the catalog does not carry (AWSRAMDefaultPermissionAPIGatewayDeveloperPortalProduct,
...AgentRegistryReadOnly, the NetworkFirewall*/EventBridgeEventBusV2*/LFEnabledGlue*/SageMaker*
families and the rest). Those divergences are the key set of
`20261115T141853Z-ram-listpermissions-serves-an-incomplete-aws-managed-permission.md`, which lists
every one of them by name; this issue carries none of them. They are left to that issue rather than
guessed: no source readable from this checkout gives those permissions' resource types and policy
actions.

> _Note: this issue was identified against the code base at commit `f672b96071627a2f926b4576526626783c2d131c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListPermissions

## Description
One cause, on ram ListPermissions, rooted in the service's seeded AWS-managed permission catalog.

Cause 1 of 1: an unfiltered ListPermissions serves a catalog that is both short (most AWS_MANAGED permission ARNs are absent) and, for the entries it does serve, structurally incomplete (no restrictToServiceLinkedShares) and version-divergent (fixture ram-catalog-list-permissions, step task#1).

AWS: Smithy: ListPermissions -> ListPermissionsResponse.permissions : ResourceSharePermissionList of ResourceSharePermissionSummary, whose modeled members are arn, name, version, defaultVersion, isResourceTypeDefault, permissionType (PermissionType enum: AWS_MANAGED | CUSTOMER_MANAGED), resourceType (documented as service-code:resource-code, case-insensitive), status, creationTime, lastUpdatedTime, featureSet and tags. Input members: resourceType, permissionType (PermissionTypeFilter), maxResults @range min 1 max 500, nextToken; paginated inputToken=nextToken / outputToken=nextToken / pageSize=maxResults; http POST /listpermissions code 200; modeled errors InvalidParameterException (400), InvalidNextTokenException (400), OperationNotPermittedException (400), ServerInternalException (500), ServiceUnavailableException (503). AWS publishes one AWS_MANAGED managed permission per shareable resource type plus its named variants, and the permission catalog is account-independent: an unfiltered ListPermissions in ANY account enumerates that entire published catalog — every AWSRAMDefaultPermission*, AWSRAMPermission*, AWSRAMLFEnabled*, AWSRAM*IssuanceCertificateAuthority and AWSRAMRevokeCertificate* entry — each with permissionType=AWS_MANAGED, status=ATTACHABLE, defaultVersion=true, isResourceTypeDefault set per whether it is that resource type's default, the resourceType it governs, and the version AWS publishes for it. Recorded behavior only for the member restrictToServiceLinkedShares: it is NOT a member of ResourceSharePermissionSummary in the on-disk model, but the recording shows AWS emits it on EVERY catalog entry as a boolean (false for these AWS-managed permissions), so a wire-faithful response must carry it on every element. Status and shape only here — no error code or message is involved, since both AWS and the emulator answer 200.

Emulator: answers 200 with a permissions list that (a) omits well over a hundred published AWS_MANAGED ARNs outright, each reported `missing (expected {...}, got None)` — e.g. arn:aws:ram::aws:permission/AWSRAMDefaultPermissionAPIGatewayDeveloperPortalProduct (apigateway:Portalproducts), AWSRAMDefaultPermissionAgentRegistryReadOnly (agent-registry:Registry), AWSRAMDefaultPermissionAmazonDataZoneDomain, AWSRAMBlankEndEntityCertificateAPICSRPassthroughIssuanceCertificateAuthority and AWSRAMBlankEndEntityCertificateAPIPassthroughIssuanceCertificateAuthority (acm-pca:CertificateAuthority), AWSRAMDefaultPermissionCoipPool, AWSRAMDefaultPermissionEBSVolume, AWSRAMDefaultPermissionFsx, AWSRAMDefaultPermissionGlueCatalog, AWSRAMDefaultPermissionImageBuilderComponent/ContainerRecipe/ImageRecipe, AWSRAMDefaultPermissionIpamPool, AWSRAMDefaultPermissionLocalGateway, the whole AWSRAMDefaultPermissionNetworkFirewall* family, AWSRAMDefaultPermissionODBCloudExadataInfrastructure/ODBExascaleDbStorageVault/ODBNetwork, AWSRAMDefaultPermissionOutpost/OutpostsOutpost/OutpostsSite, AWSRAMDefaultPermissionRDSCluster, AWSRAMDefaultPermissionSSMContactsContact/SSMIncidentsResponsePlan/SSMParameterReadOnly, the AWSRAMEventBridgeEventBusV2*, AWSRAMLFEnabledGlue*, AWSRAMPermissionAccessGrants*, AWSRAMPermissionAgentRegistry*, AWSRAMPermissionSMSVoice*, AWSRAMPermissionSageMaker*, AWSRAMPermissionLFTagGlue* and AWSRAMSubordinateCACertificatePathLen1/2/3IssuanceCertificateAuthority families; (b) emits NO restrictToServiceLinkedShares on any entry it DOES serve — reported `.restrictToServiceLinkedShares: missing (expected False, got None)` on APIGatewayPrivateDomainNameManagementPolicyDefaultPermission, AWSRAMDefaultPermissionAppMesh, ...CapacityReservation, ...CertificateAuthority, ...CodeBuildProject, ...CodeBuildReportGroup, ...DedicatedHost, ...GlueDatabase, ...GlueTable, ...ImageBuilderImage, ...LicenseConfiguration, ...PrefixList, ...ResolverRule, ...ResourceGroup, ...Subnet, ...TransitGateway, AWSRAMPermissionGlueDatabaseReadWrite and AWSRAMSubordinateCACertificatePathLen0IssuanceCertificateAuthority; and (c) reports a `version` that differs from the published one for AWSRAMDefaultPermissionAppMesh, AWSRAMDefaultPermissionCapacityReservation, AWSRAMDefaultPermissionTransitGateway and AWSRAMPermissionGlueDatabaseReadWrite. creationTime/lastUpdatedTime are shaped-not-asserted and are NOT part of this finding.

Why: ListPermissions is the only enumeration API for managed permissions, so a client that discovers a permission before using it — the console share wizard, Terraform's aws_ram_permission / aws_ram_resource_share lookups, any CreateResourceShare(permissionArns=...) or AssociateResourceSharePermission flow that first resolves a resource type's default permission ARN — concludes that every absent resource type cannot be shared at all, and fails or silently skips rather than attaching a permission that really exists. A client that reads restrictToServiceLinkedShares receives nothing where AWS always sends a boolean: a strict deserializer faults on the missing field and a lenient one mis-renders service-linked-share scoping as unset. A version that disagrees with the published one makes a pinned permission version unresolvable on a later AssociateResourceSharePermission or CreatePermissionVersion.

Fix: the ram service's seeded AWS-managed permission catalog (the state the service's managed-permission entries are built from) must carry RAM's full published AWS_MANAGED catalog rather than a hand-authored subset, with each entry carrying its published version and restrictToServiceLinkedShares; the ListPermissions handler's serializer must then project restrictToServiceLinkedShares on every ResourceSharePermissionSummary element it emits.

Why this is not already covered: the replay reports this cluster with an EMPTY owned_by, i.e. no open issue carries its replay key, so a fixer re-replaying the currently filed keys does not exercise it. The closest open issue, 20261115T141853Z-ram-listpermissions-serves-an-incomplete-aws-managed-permission.md, describes the same catalog area but its recorded keys no longer bind this cluster (the catalog's divergent key set has moved on — entries such as AWSRAMDefaultPermissionAgentRegistryReadOnly, AWSRAMDefaultPermissionAmazonDataZoneDomain and the AWSRAMEventBridgeEventBusV2* family now diverge as whole-object omissions), so this finding is filed to re-gate it on the current key. The other open ram issues are about different reads: 20261113T145122Z-ram-listresourcesharepermissions-omits-a-managed-permission-attached-to.md and 20261110T181341Z-ram-listresourcesharepermissions-never-reports-a-share-attachment-status.md cover the share-scoped ListResourceSharePermissions and its ASSOCIATED status, and 20261103T042821Z-ram-tagging-operations-reject-managed-permission-arns-so.md covers TagResource/UntagResource/GetPermission on managed-permission ARNs. None of them covers the account-wide ListPermissions catalog's membership, its restrictToServiceLinkedShares member or its versions.

Traces: ram-catalog-list-permissions#task#1.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the booted emulator (emulator_up=true, /__admin/health answered 200, data listener accepting; bootstrap_signal=false; steps_total=90, steps_passed=64, score_pct=71.1).

Fixture ram-catalog-list-permissions, step task#1, service ram, operation ListPermissions, verdict mismatch, expected status 200, actual status 200, affected_count=1, stub_path empty, owned_by empty.

Expected vs actual, as reported by the replay:
  $.permissions[arn="arn:aws:ram::aws:permission/APIGatewayPrivateDomainNameManagementPolicyDefaultPermission"].restrictToServiceLinkedShares: missing (expected False, got None)
  $.permissions[arn="arn:aws:ram::aws:permission/AWSRAMBlankEndEntityCertificateAPICSRPassthroughIssuanceCertificateAuthority"].restrictToServiceLinkedShares: missing (expected False, got None)
  $.permissions[arn="arn:aws:ram::aws:permission/AWSRAMBlankEndEntityCertificateAPIPassthroughIssuanceCertificateAuthority"].restrictToServiceLinkedShares: missing (expected False, got None)
  $.permissions[arn="arn:aws:ram::aws:permission/AWSRAMDefaultPermissionAPIGatewayDeveloperPortalProduct"]: missing (expected {'arn': 'arn:aws:ram::aws:permission/AWSRAMDefaultPermissionAPIGatewayDeveloperPortalProduct', 'creationTime': '<shaped-not-asserted>', 'defaultVersion': True, 'isResourceTypeDefault': True, 'lastUpdatedTime': '<shaped-not-asserted>', 'name': 'AWSRAMDefaultPermissionAPIGatewayDeveloperPortalProduct', 'permissionType': 'AWS_MANAGED', 'resourceType': 'apigateway:Portalproducts', 'restrictToServiceLinkedShares': False, 'status': 'ATTACHABLE', 'version': '1'}, got None)
  $.permissions[arn="arn:aws:ram::aws:permission/AWSRAMDefaultPermissionAgentRegistryReadOnly"]: missing (expected {'arn': 'arn:aws:ram::aws:permission/AWSRAMDefaultPermissionAgentRegistryReadOnly', 'creationTime': '<shaped-not-asserted>', 'defaultVersion': True, 'isResourceTypeDefault': True, 'lastUpdatedTime': '<shaped-not-asserted>', 'name': 'AWSRAMDefaultPermissionAgentRegistryReadOnly', 'permissionType': 'AWS_MANAGED', 'resourceType': 'agent-registry:Registry', 'restrictToServiceLinkedShares': False, 'status': 'ATTACHABLE', 'version': '2'}, got None)
  $.permissions[arn="arn:aws:ram::aws:permission/AWSRAMDefaultPermissionAmazonDataZoneDomain"]: missing (expected a full AWS_MANAGED summary, got None)
  ... the same `missing (expected {...}, got None)` whole-object verdict for ~150 further AWS_MANAGED ARNs, including AWSRAMDefaultPermissionAppMeshPreview, ...CoipPool, ...EBSVolume, ...Fsx, ...GlueCatalog, ...ImageBuilderComponent, ...ImageBuilderContainerRecipe, ...ImageBuilderImageRecipe, ...IpamPool, ...LocalGateway, the AWSRAMDefaultPermissionNetworkFirewall* family (ContainerAssociation, FirewallPolicy, Policy, StatefulRuleGroup[WithDescribe], StatelessRuleGroup[WithDescribe]), ...ODBCloudExadataInfrastructure, ...ODBExascaleDbStorageVault, ...ODBNetwork, ...Outpost, ...OutpostsOutpost, ...OutpostsSite, ...RDSCluster, ...RefactorSpacesEnvironment, ...ResolverFirewallRuleGroup, ...ResolverQueryLogConfig, ...S3Outposts, ...SSMContactsContact, ...SSMIncidentsResponsePlan, ...SSMParameterReadOnly, ...SageMakerModelPackageGroup, ...SageMakerPipeline, ...ServerManageability, ...ServiceDiscoveryNamespace, ...Site, ...StatefulRuleGroup, ...TrafficMirror, ...TransitGatewayMulticastDomain, ...VpcLatticeServiceNetwork, ...sIpamPool, ...sPlacementGroup, ...sSageMakerLineageGroup, ...sSecurityGroup, the AWSRAMEventBridgeEventBusV2* family, AWSRAMFSxRosettaPermissionPrefixList, the AWSRAMLFEnabledGlue* family, the AWSRAMPermissionAccessGrants*, AWSRAMPermissionAgentRegistry*, AWSRAMPermissionAppSync*, AWSRAMPermissionGlue*, AWSRAMPermissionLFTagGlue*, AWSRAMPermissionSMSVoice*, AWSRAMPermissionSageMaker*, AWSRAMPermissionSagemakerHub*, AWSRAMPermissionServiceCatalogAppRegistry* and AWSRAMPermissionVpcLattice* families, AWSRAMRevokeCertificateCertificateAuthority, AWSRAMSageMakerServicePrincipalPermissionAmazonDataZoneDomain and AWSRAMSubordinateCACertificatePathLen1/2/3IssuanceCertificateAuthority.
  ... plus `.restrictToServiceLinkedShares: missing (expected False, got None)` on every entry the emulator DOES serve (AWSRAMDefaultPermissionAppMesh, ...CapacityReservation, ...CertificateAuthority, ...CodeBuildProject, ...CodeBuildReportGroup, ...DedicatedHost, ...GlueDatabase, ...GlueTable, ...ImageBuilderImage, ...LicenseConfiguration, ...PrefixList, ...ResolverRule, ...ResourceGroup, ...Subnet, ...TransitGateway, AWSRAMPermissionGlueDatabaseReadWrite, AWSRAMSubordinateCACertificatePathLen0IssuanceCertificateAuthority).
  ... plus `.version` divergence on AWSRAMDefaultPermissionAppMesh, AWSRAMDefaultPermissionCapacityReservation, AWSRAMDefaultPermissionTransitGateway and AWSRAMPermissionGlueDatabaseReadWrite.

creationTime / lastUpdatedTime are reported as '<shaped-not-asserted>' and are explicitly NOT asserted; no per-run value (request id, clock time, generated name) is part of this finding.

Model consulted: services/service-com-amazonaws-ram-amazonresourcesharing/smithy-model/com-amazonaws-ram-amazonresourcesharing.json — ListPermissions (input resourceType/permissionType/maxResults @range 1..500/nextToken; output permissions + nextToken; POST /listpermissions, code 200; errors InvalidParameterException, InvalidNextTokenException, OperationNotPermittedException, ServerInternalException, ServiceUnavailableException) and ResourceSharePermissionSummary (members arn, version, defaultVersion, name, resourceType, status, creationTime, lastUpdatedTime, isResourceTypeDefault, permissionType, featureSet, tags — there is NO restrictToServiceLinkedShares member, so that field is recorded behavior only).

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay wire fixture ram-catalog-list-permissions from trace set simparity-wire-aws-v1 and stop at step task#1 (operation ListPermissions on service ram).
3. Equivalently, by hand: POST /listpermissions to the ram endpoint with an empty body (no resourceType, no permissionType filter) — i.e. aws ram list-permissions.
4. The response is 200. Compare its permissions list against the recorded response: count the AWS_MANAGED ARNs returned and look for arn:aws:ram::aws:permission/AWSRAMDefaultPermissionImageBuilderComponent, .../AWSRAMDefaultPermissionAPIGatewayDeveloperPortalProduct, .../AWSRAMDefaultPermissionAgentRegistryReadOnly, .../AWSRAMDefaultPermissionCoipPool, .../AWSRAMDefaultPermissionOutpost and any AWSRAMDefaultPermissionNetworkFirewall* entry — all are absent from the emulator's list.
5. For any entry that IS returned (e.g. .../AWSRAMDefaultPermissionAppMesh), observe the element carries no restrictToServiceLinkedShares key at all, where the recording has restrictToServiceLinkedShares: false on every element.
6. Compare the version of .../AWSRAMDefaultPermissionAppMesh, .../AWSRAMDefaultPermissionCapacityReservation, .../AWSRAMDefaultPermissionTransitGateway and .../AWSRAMPermissionGlueDatabaseReadWrite against the recorded published versions — they differ.
