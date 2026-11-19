Issue-Source: trace-sweep-divergence
Issue-Trace: ram-catalog-list-permissions
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ram-catalog-list-permissions ram-customer-managed-permission-lifecycle
Issue-Shape-Id: com.amazonaws.ram#AmazonResourceSharing
Issue-Key: ram|ListPermissions|2xx->2xx|$.permissions[arn="arn:aws:ram::aws:permission/AWSRAMSubordinateCACertificatePathLen1IssuanceCertificateAuthority"]
Issue-Key: ram|ListPermissions|2xx->2xx|$.permissions[arn="arn:aws:ram::aws:permission/AWSRAMSubordinateCACertificatePathLen2IssuanceCertificateAuthority"]
Issue-Key: ram|ListPermissions|2xx->2xx|$.permissions[arn="arn:aws:ram::aws:permission/AWSRAMSubordinateCACertificatePathLen3IssuanceCertificateAuthority"]
Issue-Key: ram|ListPermissionVersions|2xx->2xx|$.permissions[*]
Issue-Key: ram|ListPermissionVersions|2xx->2xx|$.permissions[*].status
Issue-Related: 20261115T141853Z-ram-listpermissions-serves-an-incomplete-aws-managed-permission.md
Issue-Commit: ae04e9c7457c944b401f43b41441b2fef7a78bf2

# ram managed-permission reads serve wrong permission state: the AWS-managed catalog is incomplete and a version's status is frozen at ATTACHABLE

> _Note: this issue was identified against the code base at commit `ae04e9c7457c944b401f43b41441b2fef7a78bf2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListPermissions

## Description
Three causes, all in the same place: what RAM's managed-permission reads report about a permission. The emulator answers HTTP 200 for every one of them, so a client sees a well-formed response carrying the wrong permission universe (cause 1) or the wrong lifecycle status (causes 2 and 3). A fixer must close ALL THREE before submitting.

Cause 1 of 3: ListPermissions omits most of RAM's published AWS-managed permission library.
AWS: ListPermissions enumerates the managed permissions visible to the caller; with permissionType set to the AWS filter it enumerates the library AWS itself publishes, one ResourceSharePermissionSummary per published permission, each carrying arn (arn:aws:ram::aws:permission/<PermissionName>, no region, 'aws' where an account id would be), name, resourceType, permissionType AWS_MANAGED, the library's published version, defaultVersion true, isResourceTypeDefault, restrictToServiceLinkedShares and status ATTACHABLE. Smithy: ListPermissions -> ListPermissionsResponse$permissions (ResourceSharePermissionList), input filter permissionType (PermissionTypeFilter), paginated nextToken/maxResults (range 1..500); the model enumerates no catalog CONTENT, so the membership of that library is recorded behavior only. The recording's 200 response contains roughly 140 published permissions that are absent from the emulator's 200 response entirely, across whole resource-type families (apigateway:Portalproducts, agent-registry:Registry, datazone:Domain, imagebuilder:Component / ContainerRecipe / ImageRecipe, every network-firewall rule-group permission, the ODB, Outposts, SSM Contacts/Incidents, SageMaker, Glue/Lake Formation, EventBridge v2 and ACM-PCA issuance families, the AWSRAMPermission* and AWSRAMLFEnabled* families, and so on).
Emulator: 200 with a shorter, hand-written catalog: for each of those permission ARNs the matcher reports 'missing ... got None' — the permission is not in the response at all. The catalog is a literal table in the service's state module rather than the published library.
Why: ListPermissions is the ONLY discovery API for managed permissions, and GetPermission's own documentation points callers at it to find a permission ARN. A client that discovers a permission there and then calls AssociateResourceSharePermission/CreateResourceShare cannot reach any of the absent resource-type families at all: the ARN it would use is undiscoverable and unresolvable, so sharing imagebuilder:Component, datazone:Domain, network-firewall rule groups etc. is impossible against the emulator although it works on AWS. Code that enumerates the library and asserts an entry per shareable resource type reads a silently truncated universe.
Fix: the AWS-managed permission catalog table the read is served from — services/service-com-amazonaws-ram-amazonresourcesharing/src/state/aws_managed_permissions.rs (the table ComAmazonawsRamAmazonresourcesharingState::default seeds) — needs the published library's full membership, each entry with its published version, isResourceTypeDefault and restrictToServiceLinkedShares; ListPermissions' handler itself only pages what that table holds. Two open issues describe neighbouring symptoms of the same table — 20261115T141853Z-ram-listpermissions-serves-an-incomplete-aws-managed-permission.md (per-entry version/restrictToServiceLinkedShares values) and 20261118T052104Z-ram-catalog-and-share-permission-reads-serve-incomplete.md (ListResourceTypes, ListResourceSharePermissions) — but the replay reports this cluster as owned by neither, so the missing-membership set above is still open.
Traces: ram-catalog-list-permissions#task#1 (ListPermissions, expected 200, got 200).

Cause 2 of 3: a non-default version of a customer-managed permission is reported ATTACHABLE where AWS reports UNATTACHABLE.
AWS: ResourceSharePermissionSummary$status targets the PermissionStatus enum, whose UNATTACHABLE value means the permission or version cannot be associated with resource shares, as against ATTACHABLE, which means it can. Smithy: ListPermissionVersions -> ResourceSharePermissionList; ResourceSharePermissionSummary$status is documented only as 'The current status of the permission', so the per-version rule is recorded behavior: once a later version of a customer-managed permission has become the default, the recording reports the superseded version with defaultVersion false and status UNATTACHABLE (only the default version is attachable to a share).
Emulator: the same version is reported with defaultVersion false but status ATTACHABLE, in a 200 response; expected status 200, actual 200.
Why: a client reading ListPermissionVersions/GetPermission to decide which version it may hand to AssociateResourceSharePermission is told every version is attachable. It will pick a superseded version that AWS would refuse, and conversely it can never observe the UNATTACHABLE state its version-rotation logic waits on.
Fix: the per-version status projection in the service's state — PermissionRecord::version_status and PermissionRecord::summary/detail in services/service-com-amazonaws-ram-amazonresourcesharing/src/state.rs and src/state/permission_deletion.rs, plus whatever writes PermissionVersionRecord.status: today that field is only ever written PERMISSION_STATUS_ATTACHABLE (state.rs, create-version path) and version_status only overrides it from a whole-permission deletion, so no version ever leaves ATTACHABLE while the permission lives. Making a new version the default must move the previously default version to UNATTACHABLE.
Traces: ram-customer-managed-permission-lifecycle#task#4 (ListPermissionVersions, expected 200, got 200).

Cause 3 of 3: a deleted permission version never reaches DELETED, so a caller polling the delete never completes.
AWS: PermissionStatus DELETED means the permission or version is deleted (DELETING while the asynchronous delete is in flight); the version record survives in that terminal status, which is what lets a caller poll the delete to completion. Recorded behavior: after the version is deleted, ListPermissionVersions reports that version with defaultVersion false and status DELETED.
Emulator: that version stays ATTACHABLE — the recorded poll for DELETED exhausted all 10 attempts with 'got ATTACHABLE', response status 200 throughout. Hypothesis: the recorded per-version delete in this fixture is answered 501 by the still-scaffolded DeletePermissionVersion (open issue 20261118T201041Z-ram-deletepermissionversion-stub.md), so nothing marks the version at all; independently of that call, the state has no way to express a per-version delete — only a whole-permission deletion record feeds version_status, so even a landed per-version delete could not be reported.
Why: any client that deletes a permission version and polls for completion (the documented way to observe an asynchronous RAM delete) hangs forever against the emulator and then acts on a version it believes is still attachable.
Fix: the DeletePermissionVersion handler (services/service-com-amazonaws-ram-amazonresourcesharing/src/ops/delete_permission_version.rs) must record the delete, and the per-version status in state (PermissionVersionRecord.status, PermissionRecord::version_status) must carry DELETING/DELETED for that one version so ListPermissionVersions/GetPermission report it. Implementing the stub alone does not close this cause unless the version's status is what the read reports.
Traces: ram-customer-managed-permission-lifecycle#task#8 (ListPermissionVersions, expected 200, got 200).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator up, /__admin/health 200):

Cause 1 — fixture ram-catalog-list-permissions, step task#1, ListPermissions: expected 200, got 200; matcher reports ~140 entries 'missing ... got None', e.g. $.permissions[arn="arn:aws:ram::aws:permission/AWSRAMDefaultPermissionAPIGatewayDeveloperPortalProduct"] (expected name AWSRAMDefaultPermissionAPIGatewayDeveloperPortalProduct, resourceType apigateway:Portalproducts, permissionType AWS_MANAGED, version '1', defaultVersion true, isResourceTypeDefault true, restrictToServiceLinkedShares false, status ATTACHABLE) and the equivalents for agent-registry:Registry, datazone:Domain, imagebuilder:Component/ContainerRecipe/ImageRecipe, the network-firewall rule-group family, ODB/Outposts/SSM/SageMaker/Glue/EventBridge-v2/ACM-PCA families — none of them present in the emulator's response.

Cause 2 — fixture ram-customer-managed-permission-lifecycle, step task#4, ListPermissionVersions: expected 200, got 200; $.permissions[0] expected {version '1', defaultVersion false, permissionType CUSTOMER_MANAGED, featureSet STANDARD, resourceType imagebuilder:Component, status 'UNATTACHABLE'}, got the same record with status 'ATTACHABLE'.

Cause 3 — fixture ram-customer-managed-permission-lifecycle, step task#8, ListPermissionVersions: expected 200, got 200; $.permissions[0] expected status 'DELETED', got 'ATTACHABLE', and '$.permissions[0].status: poll: not in ['DELETED'] after 10 attempt(s) (expected 'DELETED', got 'ATTACHABLE')'.

No divergence in this bundle is reported as blocked by a stub or owned by an open issue by the replay.

## Steps to Reproduce
1. Build and boot the emulator from this checkout and replay trace set simparity-wire-aws-v1.
2. Cause 1: replay fixture ram-catalog-list-permissions step task#1 — call ListPermissions (AWS-managed filter) and compare the returned permission ARNs against RAM's published permission library; the response is 200 but omits whole published families (apigateway:Portalproducts, agent-registry:Registry, datazone:Domain, imagebuilder:Component, network-firewall rule groups, ...).
3. Cause 2: replay fixture ram-customer-managed-permission-lifecycle through step task#4 — create a customer-managed permission, create a second version so it becomes the default, then call ListPermissionVersions on the permission ARN; version 1 comes back defaultVersion false with status ATTACHABLE instead of UNATTACHABLE.
4. Cause 3: continue the same fixture to step task#8 — delete the non-default version and poll ListPermissionVersions for that version's status; it stays ATTACHABLE for all 10 polls instead of reaching DELETED.
