Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: mgn.replication-configuration-template-access-denied-rejected
Issue-Shape-Id: com.amazonaws.mgn#ApplicationMigrationService
Issue-Key: mgn.replication-configuration-template-access-denied-rejected|run#32|AssertionError|assert seen == "AccessDeniedException", (
Issue-Commit: e7231530d31edae4c390de9d5190837f7429b405

# mgn CreateReplicationConfigurationTemplate ignores the caller's IAM identity: an identity with no mgn grant is admitted to the account's business logic instead of AccessDeniedException (403)

> _Note: this issue was identified against the code base at commit `e7231530d31edae4c390de9d5190837f7429b405`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateReplicationConfigurationTemplate

## Description
Cause 1 of 1: mgn request handling performs no IAM identity-based authorization, so a SigV4 caller holding no mgn permission is treated exactly like the account's privileged caller. Failing check: mgn.replication-configuration-template-access-denied-rejected.

AWS: CreateReplicationConfigurationTemplate models AccessDeniedException as a client error with HTTP 403 (Smithy: com.amazonaws.mgn#CreateReplicationConfigurationTemplate errors = AccessDeniedException (403) | UninitializedAccountException (400) | ValidationException (400); com.amazonaws.mgn#AccessDeniedException httpError 403, members message/code). AWS evaluates IAM authorization for the signing identity BEFORE any account-level business state (account-initialization gate, per-account template quota), so an identity with no identity-based policy allowing mgn:CreateReplicationConfigurationTemplate is refused with AccessDeniedException and creates nothing — regardless of whether the account is initialized or already at its template quota. The message for this error class is AWS's standard implicit-deny sentence, whose FIXED text is: "User: " + "<identity ARN>" + " is not authorized to perform: " + "<action>" + " on resource: " + "<resource ARN>" + " because no identity-based policy allows the " + "<action>" + " action". Its VARIABLE parts all come from the request and state, never from a recording: the identity ARN is the ARN of the IAM principal whose credentials signed the request; the action is the wire service namespace plus operation, here mgn:CreateReplicationConfigurationTemplate; the resource ARN is arn:<partition>:mgn:<region>:<account>:replication-configuration-template/* built from the request's partition, signing region and the account the request acts on. (The behavioral check derives exactly these from the user it created, the run's region and account — those literal values are per-run and are not part of the requirement.)

Emulator: the call from an IAM user with no attached policy or inline policy is not refused at all on authorization grounds. It falls straight through to the account's business state and answers ServiceQuotaExceededException (the per-account template quota, message "Max Replication Configuration Templates per Account exceeded") — i.e. the same answer the privileged caller gets, so the identity is indistinguishable from an authorized one. services/service-com-amazonaws-mgn-applicationmigrationservice/src/ops/create_replication_configuration_template.rs::handle consults no caller identity whatsoever: it resolves the target account with crate::state::account_key(None) ("the emulator's own canonical account", line 251) and then evaluates only is_initialized (line 258) and replication_configuration_template_account_quota_reached (line 280). AccessDeniedException is declared in the operation's error enum (line 186) but is never constructed. Consequently, when the account holds fewer templates than the quota, the same unauthorized identity would be allowed to CREATE a template and mutate account state.

Why: a client that relies on AWS's authorization ordering cannot test it here. Negative-path tests that assert least-privilege (an unprivileged role/user must be denied) pass vacuously or see the wrong error code and HTTP status (402/400-class business errors instead of 403 AccessDeniedException), and error-handling branches keyed on AccessDeniedException are never exercised. Worse, a principal that AWS would deny can create and delete replication configuration templates against the emulator, so a policy regression that real AWS would catch is invisible, and privilege-separation behaviour verified against the emulator does not hold in production.

Fix: the request-authorization step of the mgn service — resolve the SigV4 credential on the request to the IAM identity the emulator's IAM state holds, evaluate its identity-based policies for the mgn action being invoked, and refuse with the already-declared AccessDeniedException (403) and the implicit-deny message template above BEFORE the account-initialization gate and the per-account quota in CreateReplicationConfigurationTemplate's handler. The handler's existing ordering (account gate, then quota) stays; authorization goes ahead of both. Note this is distinct from the open issues 20261023T003710Z-mgn-tag-api-and-createlaunchconfigurationtemplate-answer-501-notimplemented.md (tag-API 501s and the initialization gate) and 20261104T132908Z-mgn-initializeservice-always-answers-204-success-and-never.md — neither addresses caller authorization. The sibling authorization check on CreateLaunchConfigurationTemplate is currently owned by the first of those issues because that operation answers 501, so it is not duplicated here.

Traces: check mgn.replication-configuration-template-access-denied-rejected — the unprivileged identity's call answered 'ServiceQuotaExceededException' where AccessDeniedException was required; handler evidence at services/service-com-amazonaws-mgn-applicationmigrationservice/src/ops/create_replication_configuration_template.rs lines 251, 258, 280.

## Evidence
Failing check mgn.replication-configuration-template-access-denied-rejected:

  File "check_mgn_replication_configuration_template_access_denied_rejected.py", line 117, in run
    assert seen == "AccessDeniedException", (
  AssertionError: expected AccessDeniedException for an unauthorized identity, got 'ServiceQuotaExceededException'

Expected (AWS): HTTP 403, Error.Code = "AccessDeniedException", Error.Message = "User: <signing identity ARN> is not authorized to perform: mgn:CreateReplicationConfigurationTemplate on resource: arn:<partition>:mgn:<region>:<account>:replication-configuration-template/* because no identity-based policy allows the mgn:CreateReplicationConfigurationTemplate action"; no template created; the account's template set unchanged. After the identity is granted Action "mgn:CreateReplicationConfigurationTemplate" the identical call collapses onto the privileged caller's outcome.

Actual (emulator): the unprivileged identity's call is not authorized at all; it is answered from account business state with Error.Code = "ServiceQuotaExceededException" (HTTP 402) and Message = "Max Replication Configuration Templates per Account exceeded" — byte-for-byte the privileged caller's baseline answer. The check's preceding assertions (privileged create succeeds, privileged second create meets the quota with that message) passed, so the quota path is implemented and the authorization path is simply absent.

Code evidence (services/service-com-amazonaws-mgn-applicationmigrationservice/src/ops/create_replication_configuration_template.rs):
  251: let account = crate::state::account_key(None);   // caller identity never consulted
  258: if !store.is_initialized(&account) { ... UninitializedAccountException }
  280: if store.replication_configuration_template_account_quota_reached(&account) { ... ServiceQuotaExceededException }
  186: AccessDeniedException(AccessDeniedException),     // declared, never constructed

Smithy: com.amazonaws.mgn#CreateReplicationConfigurationTemplate errors include AccessDeniedException; com.amazonaws.mgn#AccessDeniedException -> errorKind client, httpError 403.

## Steps to Reproduce
1. Boot the emulator and point botocore at it.
2. mgn.initialize_service() so the canonical account is initialized, then create replication configuration templates until the account quota is met (the emulator admits one per account), so the privileged baseline answer is ServiceQuotaExceededException "Max Replication Configuration Templates per Account exceeded".
3. With the emulator's IAM: iam.create_user(UserName=<new user>) and iam.create_access_key(UserName=<new user>). Attach NO policy of any kind to that user.
4. Build a second mgn client signed with that user's access key id / secret access key.
5. Call create_replication_configuration_template(...) on that unprivileged client with any valid body (stagingAreaSubnetId=<a subnet id>, associateDefaultSecurityGroup=True, replicationServersSecurityGroupsIDs=[], replicationServerInstanceType="t3.small", useDedicatedReplicationServer=False, defaultLargeStagingDiskType="GP3", ebsEncryption="DEFAULT", bandwidthThrottling=0, dataPlaneRouting="PRIVATE_IP", createPublicIP=False, stagingAreaTags={}).
6. Observe Error.Code == "ServiceQuotaExceededException" (HTTP 402) instead of "AccessDeniedException" (HTTP 403) with the implicit-deny message.
7. Variant showing the mutation side: delete the account's templates first so the quota is free, then repeat step 5 with the unprivileged client — the call SUCCEEDS and mints a template (replicationConfigurationTemplateID visible via describe_replication_configuration_templates), which AWS would have denied.
