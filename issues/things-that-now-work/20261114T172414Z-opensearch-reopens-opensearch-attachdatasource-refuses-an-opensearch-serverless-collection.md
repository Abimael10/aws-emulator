Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: opensearch.list-migrations-most-recent-first-ordering opensearch.migration-listing-recency-ordering opensearch.migration-progress-visibility-poll-window opensearch.representation-migration-timestamp-precision-preserved opensearch.start-migration-invalid-workspace-type-rejected
Issue-Key: opensearch.list-migrations-most-recent-first-ordering|_attach_data_source#4|Timeout|time.sleep(interval_s)
Issue-Key: opensearch.migration-listing-recency-ordering|_attach_data_source#7|Timeout|time.sleep(interval_s)
Issue-Key: opensearch.migration-progress-visibility-poll-window|_attach_data_source#4|Timeout|time.sleep(interval_s)
Issue-Key: opensearch.representation-migration-timestamp-precision-preserved|_attach_data_source#4|Timeout|time.sleep(interval_s)
Issue-Key: opensearch|AttachDataSource|ValidationException
Issue-Commit: 249ba79e4912d920a234429df8a39f85e3c7aa16

# REOPENS: opensearch AttachDataSource refuses an OpenSearch Serverless collection ARN with 400 ValidationException "does not name a supported data source", so no application data source can be attached and the whole migration surface is unreachable

> _Note: this issue was identified against the code base at commit `249ba79e4912d920a234429df8a39f85e3c7aa16`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AttachDataSource

## Description
REOPENS 20261027T214206Z-opensearch-2-net-new-causes-attachdatasource-performs-no.md: its fix left the call refused.

This issue carries 1 root cause, in the opensearch service. It must be closed before submitting.

Cause 1 of 1: AttachDataSource refuses a dataSourceArn naming an OpenSearch Serverless (aoss) collection of the caller's own account with 400 ValidationException instead of accepting the attach.
Failing checks: opensearch.start-migration-invalid-workspace-type-rejected (surfaces the refusal directly), opensearch.list-migrations-most-recent-first-ordering, opensearch.migration-listing-recency-ordering, opensearch.migration-progress-visibility-poll-window, opensearch.representation-migration-timestamp-precision-preserved (the same refusal, swallowed by each check's bounded attach/settle loop, which treats ValidationException as a transient "not ready yet" and retries until the harness cap).

AWS: Smithy: AttachDataSource(id @required @httpLabel pattern ^[a-z0-9]{3,30}$, dataSourceArn @required ARN @length(20..2048) pattern .*, workspaceId, workspaceConfiguration, clientToken) POST /2021-01-01/opensearch/application/{id}/attachDataSource -> 200 {attachmentId, id, arn, dataSourceArn, status: DataSourceAttachmentStatus = PENDING|ATTACHED|FAILED}; modeled errors AccessDeniedException(403), ConflictException(409), DisabledOperationException(409), ResourceNotFoundException(409), ValidationException(400), InternalException(500). The operation's own modeled documentation gives the lifecycle: "If both the application and the data source are active, the attachment completes immediately with a status of ATTACHED. Otherwise, the operation returns PENDING and completes the attachment automatically once both become active. If the attachment cannot be completed, its status becomes FAILED. This operation is idempotent...". Recorded behavior: an `arn:aws:aoss:<region>:<account>:collection/<collection-id>` returned by opensearchserverless CreateCollection IS a data source an OpenSearch application attaches — the attach is ACCEPTED (200, status PENDING even while the application and/or the collection are still provisioning) and settles to ATTACHED, after which DescribeDataSourceAttachment reports ATTACHED and StartMigration accepts that same ARN as migrationOptions.source.datasourceArn. StartMigration resolves source.datasourceArn against the application's OWN attached-data-source registry, so an AOSS collection that cannot be attached can never be migrated at all. The model's prose sentence "The data source must be an Amazon OpenSearch Service domain" does not describe the migration data-source surface the recording exercises; it is not a basis for refusing an aoss collection ARN.

Emulator: the call is refused 400 ValidationException with message `dataSourceArn arn:aws:aoss:<region>:<account>:collection/<collection-id> does not name a supported data source.` (fixed text: `<member> `, ` does not name a supported data source.`; variable parts: the member name `dataSourceArn` and the ARN taken verbatim from the request — the region, account and collection id in any one run are per-run values, not requirements). The refusal is read off the VALUE before anything is resolved, so no amount of waiting for the collection to become ACTIVE changes it. Mechanism: services/service-com-amazonaws-opensearch-amazonopensearchservice/src/ops/attach_data_source.rs:218-223 calls crate::state::data_source_arns::unsupported_data_source_refusal, and services/service-com-amazonaws-opensearch-amazonopensearchservice/src/state/data_source_arns.rs:39 admits only DATA_SOURCE_RESOURCE_TYPES = ["domain", "datasource"] under this service's ARN namespace plus the account's Glue Data Catalog (lines 51-62). The `aoss` namespace is in neither set, so every OpenSearch Serverless collection ARN takes the refusal branch.

Why: a client following AWS's documented flow — create an AOSS collection, attach it to an OpenSearch application, then StartMigration/GetMigration/ListMigrations against it — is told its request is malformed for an ARN AWS accepts. Because the refusal is a ValidationException, retry wrappers that (correctly) treat ValidationException as the transient "the collection has not registered yet" condition AWS emits during provisioning spin until they give up, so the caller sees a hang rather than a verdict. Nothing downstream of the attachment is reachable: migration listing order, migration progress visibility, migration createdAt representation and StartMigration's own workspace-type validation can none of them be exercised, and AttachDataSource's 200/PENDING->ATTACHED path is dead for the only data-source kind these flows use.

Fix: the data-source kind rule in services/service-com-amazonaws-opensearch-amazonopensearchservice/src/state/data_source_arns.rs (names_a_data_source / unsupported_data_source_refusal) and the activation resolution it is paired with (ComAmazonawsOpensearchAmazonopensearchserviceState::data_source_activation, consumed at attach_data_source.rs:312-322) must admit an `aoss` collection ARN of the caller's account as a data source kind and resolve a collection that exists to Active (or ActiveAt its provisioning due time) so the attachment converges PENDING -> ATTACHED, with ValidationException reserved for a value that is no ARN, names another account, or names a resource kind AWS genuinely does not attach.

Neighbouring filed work (named, not relied on): open issue 20261029T182036Z-opensearch-index-reads-writes-and-data-source-attachment.md carries these same five check ids under its Cause 4, but for a DIFFERENT emulator behavior — an attachment that was ACCEPTED and then settled FAILED. The emulator no longer accepts the attach at all, so the replay keys differ and the report does not list these checks as owned by it. Open issue 20261103T141919Z-opensearch-attachdatasource-is-not-idempotent-after-its-attachment.md covers attachmentId identity across a repeat attach, which these checks never reach.

Traces: opensearch.start-migration-invalid-workspace-type-rejected, opensearch.list-migrations-most-recent-first-ordering, opensearch.migration-listing-recency-ordering, opensearch.migration-progress-visibility-poll-window, opensearch.representation-migration-timestamp-precision-preserved.

## Evidence
Cause 1 — direct refusal, failing check opensearch.start-migration-invalid-workspace-type-rejected [error]:
  botocore.errorfactory.ValidationException: An error occurred (ValidationException) when calling the AttachDataSource operation: dataSourceArn arn:aws:aoss:us-west-2:000000000000:collection/0eezulb6b9pl8aw4t58g does not name a supported data source.
(The region/account/collection id shown are this run's generated values; the requirement is that this ARN SHAPE is accepted, not that any particular id appears.)

Expected: HTTP 200 with {attachmentId, id, arn, dataSourceArn = the submitted aoss collection ARN, status in (PENDING, ATTACHED)}, converging to DescribeDataSourceAttachment status ATTACHED.
Actual: HTTP 400 ValidationException, message as above. No attachment is minted, so DescribeDataSourceAttachment answers ResourceNotFoundException and StartMigration can never resolve the ARN.

Cause 1 — same refusal, swallowed into a hang. opensearch.list-migrations-most-recent-first-ordering, opensearch.migration-listing-recency-ordering, opensearch.migration-progress-visibility-poll-window and opensearch.representation-migration-timestamp-precision-preserved all reach the attach through a helper (_attach_data_source) whose loop retries the attach on ResourceNotFoundException/ValidationException — the codes AWS emits transiently while the collection is still registering — and then polls DescribeDataSourceAttachment for ATTACHED. Because the emulator's ValidationException is a permanent verdict on the ARN's value, the loop never leaves its retry leg and each check reports:
  Error: Check timed out after 60.0s

Handler evidence:
- services/service-com-amazonaws-opensearch-amazonopensearchservice/src/ops/attach_data_source.rs:218-223 — unsupported_data_source_refusal is applied to input.data_source_arn and returned as ValidationException before any lookup.
- services/service-com-amazonaws-opensearch-amazonopensearchservice/src/state/data_source_arns.rs:39 — const DATA_SOURCE_RESOURCE_TYPES: [&str; 2] = ["domain", "datasource"].
- services/service-com-amazonaws-opensearch-amazonopensearchservice/src/state/data_source_arns.rs:51-62 — names_a_data_source admits only this service's namespace with those two resource types, or a Glue data catalog; the `aoss` vendor is not admitted.
- services/service-com-amazonaws-opensearch-amazonopensearchservice/src/state/data_source_arns.rs:70-79 — the message template `{member} {arn} does not name a supported data source.`

Prior filing: fixed issue 20261027T214206Z-opensearch-2-net-new-causes-attachdatasource-performs-no.md Cause 2 covered a non-domain data-source ARN settling FAILED instead of PENDING/ATTACHED. Its fix added a value-level refusal for non-admitted ARN kinds and admitted only `es` domain/datasource plus the Glue catalog, which leaves the aoss collection — the data source every migration check uses — refused outright rather than attached.

## Steps to Reproduce
1. opensearchserverless: CreateSecurityPolicy(type="encryption") and CreateSecurityPolicy(type="network") for collection/<name>, then CreateCollection(name=<name>, type="SEARCH"); capture createCollectionDetail.arn (arn:aws:aoss:<region>:<account>:collection/<id>) and poll BatchGetCollection until status == ACTIVE.
2. opensearch: CreateApplication(name=<unique>) and poll GetApplication until status == ACTIVE.
3. opensearch: AttachDataSource(id=<appId>, dataSourceArn=<the aoss collection ARN from step 1>).
   Expected: HTTP 200 with a minted attachmentId and status PENDING or ATTACHED.
   Observed: HTTP 400 ValidationException "dataSourceArn <that ARN> does not name a supported data source."
4. Poll DescribeDataSourceAttachment(id=<appId>, dataSourceArn=<that ARN>) — expected to converge to status ATTACHED; observed: nothing was ever attached.
5. Retry step 3 unchanged after the collection has been ACTIVE for minutes — the refusal is identical, confirming it is a verdict on the ARN's value and not the transient provisioning ValidationException AWS emits.
6. opensearch: StartMigration(applicationId=<appId>, migrationOptions={source:{datasourceArn:<that ARN>}, workspace:{createWorkspace:true, name:<unique>, type:"SEARCH"}}) — unreachable as a 200 because step 3 never registered the data source.
