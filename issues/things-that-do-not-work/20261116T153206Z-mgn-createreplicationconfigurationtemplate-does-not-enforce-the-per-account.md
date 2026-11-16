Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: mgn.replication-configuration-template-access-denied-rejected
Issue-Shape-Id: com.amazonaws.mgn#ApplicationMigrationService
Issue-Key: mgn.replication-configuration-template-access-denied-rejected|run#18|AssertionError|assert baseline == "ServiceQuotaExceededException", (
Issue-Commit: 782d16bdd1ce5a2a9842801291a869e14f8231ec

# mgn CreateReplicationConfigurationTemplate does not enforce the per-account quota of one replication configuration template and refuses the over-quota call with the wrong error

> _Note: this issue was identified against the code base at commit `782d16bdd1ce5a2a9842801291a869e14f8231ec`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateReplicationConfigurationTemplate

## Description
One net-new root cause in mgn.

Cause 1 of 1: CreateReplicationConfigurationTemplate accepts a SECOND replication configuration template in an account that already holds one, and the quota bound it does enforce is both the wrong size and signalled with the wrong error shape. Failing check: mgn.replication-configuration-template-access-denied-rejected.

AWS: Application Migration Service admits exactly ONE replication configuration template per account. Once an account holds one, an identical CreateReplicationConfigurationTemplate call is REFUSED with error code `ServiceQuotaExceededException` and the message `Max Replication Configuration Templates per Account exceeded`. The whole message is FIXED text for this error class — it carries no request- or state-derived value at all (no id, no account number, no limit interpolated). Smithy: `com.amazonaws.mgn#ServiceQuotaExceededException` is modeled by this service with `errorKind: client`, `httpError: 402`, and optional members `message`/`code`/`resourceId`/`resourceType`/`serviceCode`/`quotaCode`/`quotaValue`; the operation's own modeled error list (AccessDeniedException 403, UninitializedAccountException 400, ValidationException 400) does NOT list it, so the quota refusal itself is recorded behavior only — the recording shows code `ServiceQuotaExceededException` with that message, and the service models the shape to carry it. The quota of one per account is recorded behavior only (the check's baseline: the account's first template is created, the identical second call meets the quota).

Emulator: the second create SUCCEEDS and returns a new `replicationConfigurationTemplateID` (replay outcome `'created'`, no error). The emulator's only bound is its own store-wide cap of 1_000 templates over all accounts (`REPLICATION_CONFIGURATION_TEMPLATE_QUOTA`, enforced via `replication_configuration_template_store_is_full`), so a per-account second create is never refused; and when that cap IS reached the refusal is the wrong one — code `ValidationException` (HTTP 400) with the emulator-invented message `Account <account> already holds the maximum of 1000 replication configuration templates.` instead of `ServiceQuotaExceededException` (HTTP 402) with `Max Replication Configuration Templates per Account exceeded`.

Why: a client that provisions a replication configuration template is over-permitted — it ends up with several templates in one account where AWS allows one, so the account's replication settings become ambiguous and state built on "the" template diverges. A client that handles the quota (catching `ServiceQuotaExceededException` to reuse the existing template, or reading `quotaCode`/`quotaValue`) never sees that error from the emulator: it sees a success, or at the store cap a `ValidationException` whose code, HTTP status (400 vs 402) and message text all differ, so the retry/reuse branch is dead code and the error branch misclassifies a quota condition as a malformed request.

Fix: in CreateReplicationConfigurationTemplate's handler, the quota check must be per-account (count the requesting account's templates, limit 1) and must be evaluated after the UninitializedAccountException gate, and the refusal it raises belongs in this service's state error constructors — a `ServiceQuotaExceededException` with the fixed AWS message, replacing the `ValidationException` the quota path currently builds. The store-wide heap bound can stay as a separate backstop, but it must not be the thing a caller observes as the account quota.

Traces: mgn.replication-configuration-template-access-denied-rejected — it creates the account's first template (or reuses an existing one), then asserts the identical second privileged call answers `ServiceQuotaExceededException` with the quota message; the emulator answered `'created'`.

## Evidence
Failing check mgn.replication-configuration-template-access-denied-rejected:

  AssertionError: expected the privileged identity's second template to meet the quota, got 'created' (None)
  at check_mgn_replication_configuration_template_access_denied_rejected.py:82
      assert baseline == "ServiceQuotaExceededException"

Expected (AWS): the second CreateReplicationConfigurationTemplate in an account that already holds one is refused —
  Error.Code    = ServiceQuotaExceededException
  Error.Message = "Max Replication Configuration Templates per Account exceeded"   (entirely fixed text)
  HTTP status   = 402 (com.amazonaws.mgn#ServiceQuotaExceededException @httpError)

Actual (emulator): HTTP 201 with a new replicationConfigurationTemplateID; no error raised. The error the emulator would eventually raise, only at its own store-wide cap of 1000 templates, is:
  Error.Code    = ValidationException
  Error.Message = "Account <account> already holds the maximum of 1000 replication configuration templates."
  HTTP status   = 400

Source of the emulator behavior (citations only):
  services/service-com-amazonaws-mgn-applicationmigrationservice/src/ops/create_replication_configuration_template.rs:260-276 — quota branch uses replication_configuration_template_store_is_full() and ValidationException
  services/service-com-amazonaws-mgn-applicationmigrationservice/src/state/replication_configuration_templates.rs:24 — REPLICATION_CONFIGURATION_TEMPLATE_QUOTA = 1_000, store-wide not per-account
  services/service-com-amazonaws-mgn-applicationmigrationservice/src/state/replication_configuration_templates.rs:205-215 — replication_configuration_template_quota_exceeded() builds the ValidationException message

Smithy (services/service-com-amazonaws-mgn-applicationmigrationservice/smithy-model/com-amazonaws-mgn-applicationmigrationservice.json):
  CreateReplicationConfigurationTemplate -> POST /CreateReplicationConfigurationTemplate, success 201, errors [AccessDeniedException 403, UninitializedAccountException 400, ValidationException 400]
  ServiceQuotaExceededException -> client error, httpError 402, members message/code/resourceId/resourceType/serviceCode/quotaCode/quotaValue

## Steps to Reproduce
1. POST /InitializeService to initialize the account.
2. POST /CreateReplicationConfigurationTemplate with a valid payload (stagingAreaSubnetId=<any subnet-xxxxxxxx>, associateDefaultSecurityGroup=true, replicationServersSecurityGroupsIDs=[], replicationServerInstanceType="t3.small", useDedicatedReplicationServer=false, defaultLargeStagingDiskType="GP3", ebsEncryption="DEFAULT", bandwidthThrottling=0, dataPlaneRouting="PRIVATE_IP", createPublicIP=false, stagingAreaTags={}). It succeeds with a new rct- id — correct, this is the account's first template.
3. POST /CreateReplicationConfigurationTemplate again with the IDENTICAL payload, same account.
   Expected: HTTP 402, Error.Code=ServiceQuotaExceededException, Error.Message="Max Replication Configuration Templates per Account exceeded".
   Actual: HTTP 201 with a second replicationConfigurationTemplateID.
4. POST /DescribeReplicationConfigurationTemplates — the account now lists two templates where AWS permits one.
5. Replay: mgn.replication-configuration-template-access-denied-rejected.
