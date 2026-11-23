Issue-Source: trace-sweep-divergence
Issue-Trace: iam-catalog-list-policies
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iam-catalog-list-policies iam-service-access-analysis-and-role-template-reports
Issue-Shape-Id: com.amazonaws.iam#AWSIdentityManagementV20100508
Issue-Key: iam|ListPolicies|2xx->2xx|$.ListPoliciesResponse.ListPoliciesResult.Policies.member[Arn="arn:aws:iam::aws:policy/service-role/AWSLambdaMSKExecutionRole"].CreateDate
Issue-Key: iam|ListPolicies|2xx->2xx|$.ListPoliciesResponse.ListPoliciesResult.Policies.member[Arn="arn:aws:iam::aws:policy/service-role/AWSLambdaMSKExecutionRole"].DefaultVersionId
Issue-Key: iam|ListPolicies|2xx->2xx|$.ListPoliciesResponse.ListPoliciesResult.Policies.member[Arn="arn:aws:iam::aws:policy/service-role/AWSLambdaMSKExecutionRole"].PolicyId
Issue-Key: iam|ListPolicies|2xx->2xx|$.ListPoliciesResponse.ListPoliciesResult.Policies.member[Arn="arn:aws:iam::aws:policy/service-role/AWSLambdaMSKExecutionRole"].UpdateDate
Issue-Key: iam|ListPolicies|2xx->2xx|$.ListPoliciesResponse.ListPoliciesResult.Policies.member[Arn="arn:aws:iam::aws:policy/service-role/AWSLambdaSQSQueueExecutionRole"].CreateDate
Issue-Key: iam|ListPolicies|2xx->2xx|$.ListPoliciesResponse.ListPoliciesResult.Policies.member[Arn="arn:aws:iam::aws:policy/service-role/AWSLambdaSQSQueueExecutionRole"].PolicyId
Issue-Key: iam|ListPolicies|2xx->2xx|$.ListPoliciesResponse.ListPoliciesResult.Policies.member[Arn="arn:aws:iam::aws:policy/service-role/AWSLambdaSQSQueueExecutionRole"].UpdateDate
Issue-Key: iam|GetServiceLastAccessedDetails|2xx->2xx|$.GetServiceLastAccessedDetailsResponse.GetServiceLastAccessedDetailsResult.ServicesLastAccessed.member[*].ServiceName
Issue-Key: iam|GetServiceLastAccessedDetails|2xx->2xx|$.GetServiceLastAccessedDetailsResponse.GetServiceLastAccessedDetailsResult.ServicesLastAccessed.member[*].ServiceNamespace
Issue-Related: 20261115T063919Z-iam-listpolicies-aws-managed-policy-catalog-diverges-from.md
Issue-Commit: f76d85d47be0a2dd652a7035a8c2c788dca4515b

# iam: built-in AWS reference catalogs diverge from AWS (managed policies, Access Advisor service list)

> _Note: this issue was identified against the code base at commit `f76d85d47be0a2dd652a7035a8c2c788dca4515b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListPolicies

## Description
TWO net-new causes, both in the static AWS reference data the IAM emulator serves from built-in catalogs rather than from per-run state. Both must be closed before this issue is submitted.

=== Cause 1 of 2: ListPolicies' AWS-managed policy catalog is incomplete and its per-policy metadata is invented ===
AWS: Smithy: ListPolicies -> ListPoliciesResponse.Policies (policyListType), filtered by Scope / PathPrefix / PolicyUsageFilter; each member carries PolicyName, PolicyId, Arn, Path, DefaultVersionId, CreateDate, UpdateDate (modelled errors: ServiceFailure only — this is a read of a fixed catalog, so a 200 body is the whole contract). AWS's Scope=AWS catalog is a published, partition-wide set: every AWS-managed policy that exists is listed, each with the PolicyId AWS minted for it (ANPA...), its own CreateDate, and the UpdateDate/DefaultVersionId of its current default version. Recorded behavior for the /service-role/ page of this corpus: it contains arn:aws:iam::aws:policy/service-role/AWSLambdaRole (PolicyName AWSLambdaRole, Path /service-role/, DefaultVersionId v1, CreateDate = UpdateDate), and the other members carry the catalog's own ids and dates.
Emulator: status is 200 in both, so only the body diverges. (a) The member for arn:aws:iam::aws:policy/service-role/AWSLambdaRole is ABSENT from Policies entirely (reported 'missing ... got None'). (b) For members that are present, PolicyId / CreateDate / UpdateDate / DefaultVersionId are not the catalog's: e.g. service-role/AmazonEC2RoleforDataPipelineRole.PolicyId is served as 'ANPAX9GMJWQZKT6PVNHLC' where the catalog's id is 'ANPAJ3Z5I2WAJE5DN2J36'; service-role/AmazonElasticTranscoderRole.UpdateDate is served as 2017-03-01T18:49:33Z where the catalog's is 2019-06-13T22:48:22Z (and its PolicyId and CreateDate likewise differ); service-role/RDSCloudHsmAuthorizationRole.UpdateDate and DefaultVersionId are stale in the same way. The served ids follow a generated shape (ANPA + random body), i.e. they are minted at seed time instead of being the policy's fixed identity.
Why: a client relying on AWS's behavior enumerates this catalog to resolve a managed policy before attaching it. A policy that AWS lists and the emulator omits makes the subsequent AttachRolePolicy/GetPolicy on arn:aws:iam::aws:policy/service-role/AWSLambdaRole unresolvable, so a role that AWS can build cannot be built here. A PolicyId that changes per seed breaks every consumer that keys on PolicyId (drift detection, inventories, policy-pinning), and stale UpdateDate/DefaultVersionId make a consumer believe it already has the policy's current version.
Fix: the IAM crate's built-in AWS-managed-policy catalog seed (the data ListPolicies, GetPolicy and ListAttachedRolePolicies all project), not the ListPolicies handler or its pagination: the catalog must carry every AWS-managed policy in the recorded set, and PolicyId / CreateDate / UpdateDate / DefaultVersionId must be fixed per-policy constants rather than values minted at boot.
Traces: iam-catalog-list-policies#task#4. NOTE on overlap: the open issue 20261115T063919Z-iam-listpolicies-aws-managed-policy-catalog-diverges-from.md carries the replay keys for task#1, task#2 and task#3 of this same fixture (the aws-service-role, job-function and root-task pages); the /service-role/ page cluster filed here is unowned by it and includes a cause that issue does not state at all — a catalog member missing outright.

=== Cause 2 of 2: GetServiceLastAccessedDetails serves a service catalog with an extra namespace, non-AWS display names, and a resulting one-off shift ===
AWS: Smithy: GetServiceLastAccessedDetails -> ServicesLastAccessed (required, list of ServiceLastAccessed{ServiceName required, ServiceNamespace required (pattern ^[\w-]*$), LastAuthenticated, ...}). For a service-granularity job AWS reports one entry per service namespace in its Access Advisor catalog, ordered by namespace, each with AWS's own display name for that service.
Emulator: status 200, body diverges. (a) It reports a namespace AWS does not carry in this list — ServiceNamespace 'arsenal' / ServiceName 'Application Discovery Arsenal' — inserted mid-list. (b) Its display names for the ARC services are not AWS's: it serves 'AWS ARC - Region switch' where AWS reports 'Amazon ARC Region switch', and 'AWS ARC - Zonal Shift' where AWS reports 'Amazon Application Recovery Controller - Zonal Shift'. (c) Because the extra entry is inserted rather than appended, every later member is shifted by one and its ServiceNamespace/ServiceName pair is the preceding service's: member[29].ServiceNamespace is served as 'artifact' where 'athena' is expected, member[30] as 'athena' where 'auditmanager' is expected, and so on across the remaining ~170 members of the list.
Why: Access Advisor consumers index into ServicesLastAccessed or join it by position against a prior page to decide which services a principal has used, then narrow the principal's policy from that. A one-off shift makes every such decision name the wrong service, a non-AWS display name breaks any consumer matching on service name, and a namespace AWS never reports can be acted on as if it were a real service.
Fix: the IAM crate's service-namespace catalog seed behind the Access Advisor projection (GetServiceLastAccessedDetails), not its job lifecycle or pagination: the namespace set must be AWS's, each entry's ServiceName must be AWS's display name for that namespace, and the list must be ordered as AWS orders it.
Traces: iam-service-access-analysis-and-role-template-reports#task#4.

## Evidence
Cause 1 (iam-catalog-list-policies#task#4, ListPolicies, expected 200 / got 200):
- $...Policies.member[Arn="arn:aws:iam::aws:policy/service-role/AWSLambdaRole"]: missing — expected {'Path': '/service-role/', 'UpdateDate': '2015-02-06T18:41:28Z', 'DefaultVersionId': 'v1', 'PolicyId': 'ANPAJX4DPCRGTC4NFDUXI', 'IsAttachable': 'true', 'PolicyName': 'AWSLambdaRole', 'Arn': 'arn:aws:iam::aws:policy/service-role/AWSLambdaRole', 'CreateDate': '2015-02-06T18:41:28Z'}, got None
- $...member[.../service-role/AmazonEC2RoleforDataPipelineRole].PolicyId: expected 'ANPAJ3Z5I2WAJE5DN2J36', got 'ANPAX9GMJWQZKT6PVNHLC'
- $...member[.../service-role/AmazonEC2RoleforDataPipelineRole].CreateDate: expected '2015-02-06T18:41:25Z', got '2015-02-06T18:41:23Z'
- $...member[.../service-role/AmazonElasticTranscoderRole].UpdateDate: expected '2019-06-13T22:48:22Z', got '2017-03-01T18:49:33Z'
- $...member[.../service-role/AmazonElasticTranscoderRole].PolicyId: expected 'ANPAJNW3WMKVXFJ2KPIQ2', got 'ANPAJ4TTBHLLSPNNAT5DO'
- $...member[.../service-role/RDSCloudHsmAuthorizationRole].UpdateDate: expected '2019-09-26T22:14:29Z', got '2015-02-06T18:41:29Z'
(the cluster's divergence key lists the same four members — PolicyId, CreateDate, UpdateDate, DefaultVersionId — across several hundred catalog entries on this page)

Cause 2 (iam-service-access-analysis-and-role-template-reports#task#4, GetServiceLastAccessedDetails, expected 200 / got 200):
- $...ServicesLastAccessed.member[26].ServiceName: expected 'Amazon ARC Region switch', got 'AWS ARC - Region switch'
- $...member[27].ServiceName: expected 'Amazon Application Recovery Controller - Zonal Shift', got 'AWS ARC - Zonal Shift'
- $...member[28].ServiceNamespace: expected 'artifact', got 'arsenal'; member[28].ServiceName: expected 'AWS Artifact', got 'Application Discovery Arsenal'
- $...member[29].ServiceNamespace: expected 'athena', got 'artifact'; member[30].ServiceNamespace: expected 'auditmanager', got 'athena'; member[31].ServiceNamespace: expected 'autoscaling', got 'auditmanager' (shift continues through member[199])

## Steps to Reproduce
1. Clone the repo and boot the emulator built from that checkout.
2. Replay the simparity-wire-aws-v1 wire corpus (replay_wire_corpus).
3. Cause 1: step task#4 of fixture iam-catalog-list-policies — a ListPolicies call covering the /service-role/ page of the AWS-managed catalog. Compare the Policies members against the recorded response: the AWSLambdaRole member is absent and PolicyId/CreateDate/UpdateDate/DefaultVersionId differ on the members that are present.
4. Cause 2: step task#4 of fixture iam-service-access-analysis-and-role-template-reports — GenerateServiceLastAccessedDetails followed by GetServiceLastAccessedDetails. Walk ServicesLastAccessed from member[26]: the ARC display names differ, member[28] is the extra 'arsenal' entry, and every later member's namespace/name pair is shifted by one.
