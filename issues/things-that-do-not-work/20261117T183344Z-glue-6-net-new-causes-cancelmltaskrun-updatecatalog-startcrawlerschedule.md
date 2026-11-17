Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: glue.cancel-ml-task-run-nonexistent-rejected glue.catalog-update-rejects-federated-catalog-and-unknown-id glue.crawler-schedule-start-resume-and-no-schedule-rejected glue.data-quality-profile-unknown-id-rejected glue.test-connection-rejects-incomplete-jdbc-properties glue.update-integration-resource-property-validation-boundaries
Issue-Shape-Id: com.amazonaws.glue#AWSGlue
Issue-Key: glue.cancel-ml-task-run-nonexistent-rejected|run#10|AssertionError|assert code1 == "EntityNotFoundException", f"expected EntityNotFoundException, got {code1}"
Issue-Key: glue.catalog-update-rejects-federated-catalog-and-unknown-id|run#10|AssertionError|assert code1 == "InvalidInputException", f"UpdateCatalog on a federated catalog gave {code1!r}"
Issue-Key: glue.crawler-schedule-start-resume-and-no-schedule-rejected|run#17|AssertionError|assert code == "NoScheduleException", f"starting a schedule-less crawler gave {code!r}, not NoScheduleException"
Issue-Key: glue.data-quality-profile-unknown-id-rejected|run#5|AssertionError|assert code1 == "EntityNotFoundException", f"GetDataQualityModel gave {code1!r}, not EntityNotFoundException"
Issue-Key: glue.test-connection-rejects-incomplete-jdbc-properties|run#3|AssertionError|assert code1 == "ValidationException", f"an inline JDBC TestConnectionInput gave {code1!r}"
Issue-Key: glue.update-integration-resource-property-validation-boundaries|run#8|AssertionError|assert code1 == "ValidationException", f"expected ValidationException for neither property, got {code1}"
Issue-Commit: 666f7954233f9ddfcb1a0dcc5f57d6c1921794e2

# glue: 6 net-new causes — CancelMLTaskRun, UpdateCatalog, StartCrawlerSchedule, the data-quality profile reads, TestConnection and UpdateIntegrationResourceProperty answer NotImplemented instead of their modeled refusals

> _Note: this issue was identified against the code base at commit `666f7954233f9ddfcb1a0dcc5f57d6c1921794e2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CancelMLTaskRun

## Description
Six distinct refusal paths in service-com-amazonaws-glue-awsglue answer with the unmodeled error code 'NotImplemented' where real Glue answers one of the operation's modeled 400 client errors. The checks below are reported by the behavioral suite with NO 'OWNED by' owner and NO 'ALREADY FILED' marker, so each is unowned. SIX causes must be closed before this issue is submitted.

Cause 1 of 6: CancelMLTaskRun for an id that does not exist.
AWS: Smithy (services/service-com-amazonaws-glue-awsglue/smithy-model/com-amazonaws-glue-awsglue.json): CancelMLTaskRun input {TransformId @required HashString, TaskRunId @required HashString}; declared errors EntityNotFoundException (client, 400), InvalidInputException (client, 400), OperationTimeoutException, InternalServiceException. Recorded behavior: a TransformId that names no transform is refused 400 EntityNotFoundException, and against a real transform a TaskRunId that names no run is refused 400 EntityNotFoundException naming the missing id; the fixed text is the not-found sentence and the variable part is the id taken from the request.
Emulator: the call is refused with Error.Code 'NotImplemented' — a code the Glue model does not declare for this operation (replay detail: "AssertionError: expected EntityNotFoundException, got NotImplemented").
Why: a client cannot distinguish 'this ML task run does not exist' from a transport/feature failure, botocore raises no modeled exception class (so `except glue.exceptions.EntityNotFoundException` never fires), and every ML-transform cleanup/retry path that keys on EntityNotFoundException breaks.
Fix: the CancelMLTaskRun handler and its ML-transform/task-run state lookup in the glue crate.
Traces: glue.cancel-ml-task-run-nonexistent-rejected

Cause 2 of 6: UpdateCatalog on a federated catalog and on an unknown catalog id.
AWS: Smithy: UpdateCatalog input {CatalogId @required CatalogIdString, CatalogInput @required}; declared errors include InvalidInputException (client, 400), EntityNotFoundException (client, 400), AccessDeniedException, FederationSourceException, ConcurrentModificationException. Recorded behavior: updating a federated (s3tables-backed) catalog is refused 400 InvalidInputException, and an unknown catalog id is refused with the operation's own not-found class — not with an unmodeled code.
Emulator: 'NotImplemented' (replay detail: "AssertionError: UpdateCatalog on a federated catalog gave 'NotImplemented'").
Why: a caller that distinguishes 'this catalog cannot be updated here' from 'no such catalog' has no signal at all, and the federated-catalog guard — the whole point of the refusal — is unobservable.
Fix: the UpdateCatalog handler (services/service-com-amazonaws-glue-awsglue/src/ops/update_catalog.rs) plus the catalog state it must read to classify federated vs. unknown. Note: an auto-filed stub issue names UpdateCatalog for the check glue__s3tables.update-catalog-unknown-catalog-id-rejected; this check (service heading 'glue') is not carried by it, and the federated-catalog refusal class above is additional to the unknown-id one.
Traces: glue.catalog-update-rejects-federated-catalog-and-unknown-id

Cause 3 of 6: StartCrawlerSchedule on a crawler that was never given a cron schedule.
AWS: Smithy: StartCrawlerSchedule input {CrawlerName @required NameString}; declared errors EntityNotFoundException, NoScheduleException, SchedulerRunningException, SchedulerTransitioningException, OperationTimeoutException (all client, 400). A crawler created with no Schedule has no schedule to start, so the call is refused 400 NoScheduleException; the same call on a scheduled crawler succeeds and the crawler's Schedule.State becomes SCHEDULED.
Emulator: 'NotImplemented' (replay detail: "AssertionError: starting a schedule-less crawler gave 'NotImplemented', not NoScheduleException").
Why: NoScheduleException is the only way a caller learns that a crawler has no schedule to resume; with an unmodeled code, schedule-management automation cannot tell that from a service failure and retries forever.
Fix: the StartCrawlerSchedule handler (services/service-com-amazonaws-glue-awsglue/src/ops/start_crawler_schedule.rs) and the crawler schedule state machine. An auto-filed stub issue names StartCrawlerSchedule for glue.crawler-schedule-lifecycle-toggles-state; this check is not carried by it and additionally asserts the start/resume transitions on a scheduled crawler.
Traces: glue.crawler-schedule-start-resume-and-no-schedule-rejected

Cause 4 of 6: the data-quality profile reads do not refuse an id no statistics run ever minted.
AWS: Smithy: GetDataQualityModel input {StatisticId, ProfileId @required}; declared errors EntityNotFoundException (client, 400), InvalidInputException, OperationTimeoutException, InternalServiceException. GetDataQualityModelResult and PutDataQualityProfileAnnotation declare the same not-found class. Recorded behavior: a well-formed profile id ('dqprofile-' + hex) that no statistics run produced is refused 400 EntityNotFoundException with the fixed message 'Unable to find Model Result' for the two model reads, and PutDataQualityProfileAnnotation's message names the requested ProfileId (fixed text plus the id from the request).
Emulator: 'NotImplemented' (replay detail: "AssertionError: GetDataQualityModel gave 'NotImplemented', not EntityNotFoundException").
Why: the data-quality profile surface is entirely unusable — a client polling for a model result cannot distinguish 'not produced yet / never existed' from a broken endpoint.
Fix: the GetDataQualityModel / GetDataQualityModelResult / PutDataQualityProfileAnnotation handlers and the data-quality statistics state in the glue crate.
Traces: glue.data-quality-profile-unknown-id-rejected

Cause 5 of 6: TestConnection with an inline TestConnectionInput.
AWS: Smithy: TestConnection input {ConnectionName, CatalogId, TestConnectionInput}; declared errors include ValidationException? no — declared client errors are AccessDeniedException, ConflictException, EntityNotFoundException, FederationSourceException, GlueEncryptionException, InvalidInputException, OperationTimeoutException, ResourceNumberLimitExceededException (all 400). Recorded behavior: an inline JDBC TestConnectionInput is refused 400 ValidationException with the fixed message 'TestConnection is only supported for the Unified Connections', and an inline SALESFORCE TestConnectionInput whose AuthenticationConfiguration declares OAUTH2 with no OAuth2Properties is refused 400 InvalidInputException with '[AuthenticationConfiguration.OAuth2Properties is required for OAUTH2 AuthenticationType.]' — both sentences fixed text, with the member path as AWS words it.
Emulator: 'NotImplemented' for both (replay detail: "AssertionError: an inline JDBC TestConnectionInput gave 'NotImplemented'").
Why: the two refusals carry different codes in AWS precisely so a caller can tell a type-level restriction from a field-level validation failure; one unmodeled code collapses both and tells the caller nothing.
Fix: the TestConnection handler (services/service-com-amazonaws-glue-awsglue/src/ops/test_connection.rs) and its TestConnectionInput validation. An auto-filed stub issue names TestConnection for glue.test-connection-unknown-name-rejected; this check is not carried by it and asserts the two inline-input refusal classes above.
Traces: glue.test-connection-rejects-incomplete-jdbc-properties

Cause 6 of 6: UpdateIntegrationResourceProperty never validates its input.
AWS: Smithy: UpdateIntegrationResourceProperty input {ResourceArn @required String512, SourceProcessingProperties, TargetProcessingProperties}; declared errors ValidationException (client, 400), EntityNotFoundException (client, 400), ResourceNotFoundException (404), AccessDeniedException, InvalidInputException, InternalServerException. Recorded behavior: a request with neither processing property is refused 400 ValidationException with the fixed sentence 'Either source processing properties or target processing properties must be provided in the request.'; a syntactically valid ARN whose resource type this API does not accept is refused 400 ValidationException with 'Encountered invalid resource arn <the ARN from the request>' (fixed text plus the request's ARN); and a well-formed Glue database ARN with TargetProcessingProperties succeeds and reads back through GetIntegrationResourceProperty, whose not-found message is 'The target resource <arn> is not found.'
Emulator: 'NotImplemented' (replay detail: "AssertionError: expected ValidationException for neither property, got NotImplemented").
Why: every integration-property write is refused with a code no client handles, so neither the validation boundaries nor the stored-value round trip is reachable.
Fix: the UpdateIntegrationResourceProperty handler, its input validation and the integration-resource-property state. Nearest open issue, which does NOT carry this check: 20261108T041805Z-glue-5-net-new-root-causes-createcolumnstatisticstasksettings-starts.md (it owns glue.integration-resource-property-lifecycle).
Traces: glue.update-integration-resource-property-validation-boundaries

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout: 503/724 passed. All six checks are listed under service heading 'glue' with no owner and no stub marker.

Cause 1 — glue.cancel-ml-task-run-nonexistent-rejected [mismatch]: check_glue_cancel_ml_task_run_nonexistent_rejected.py line 47: `assert code1 == "EntityNotFoundException"` -> "AssertionError: expected EntityNotFoundException, got NotImplemented". A positive control (GetMLTransform on the real transform) succeeds first, so the transform exists.
Cause 2 — glue.catalog-update-rejects-federated-catalog-and-unknown-id [mismatch]: line 68: `assert code1 == "InvalidInputException"` -> "AssertionError: UpdateCatalog on a federated catalog gave 'NotImplemented'".
Cause 3 — glue.crawler-schedule-start-resume-and-no-schedule-rejected [mismatch]: line 149: `assert code == "NoScheduleException"` -> "AssertionError: starting a schedule-less crawler gave 'NotImplemented', not NoScheduleException".
Cause 4 — glue.data-quality-profile-unknown-id-rejected [mismatch]: line 37: `assert code1 == "EntityNotFoundException"` -> "AssertionError: GetDataQualityModel gave 'NotImplemented', not EntityNotFoundException". The check then requires message 'Unable to find Model Result' for GetDataQualityModel and GetDataQualityModelResult, and a ProfileId-naming message for PutDataQualityProfileAnnotation.
Cause 5 — glue.test-connection-rejects-incomplete-jdbc-properties [mismatch]: line 39: `assert code1 == "ValidationException"` -> "AssertionError: an inline JDBC TestConnectionInput gave 'NotImplemented'". Expected (ValidationException, 'TestConnection is only supported for the Unified Connections') and (InvalidInputException, '[AuthenticationConfiguration.OAuth2Properties is required for OAUTH2 AuthenticationType.]'), and the check asserts the two codes DIFFER.
Cause 6 — glue.update-integration-resource-property-validation-boundaries [mismatch]: line 33: `assert code1 == "ValidationException"` -> "AssertionError: expected ValidationException for neither property, got NotImplemented".

Model evidence (com-amazonaws-glue-awsglue.json): CancelMLTaskRun errors = [EntityNotFoundException, InternalServiceException, InvalidInputException, OperationTimeoutException]; UpdateCatalog errors include [InvalidInputException, EntityNotFoundException, FederationSourceException]; StartCrawlerSchedule errors = [EntityNotFoundException, NoScheduleException, OperationTimeoutException, SchedulerRunningException, SchedulerTransitioningException]; GetDataQualityModel errors = [EntityNotFoundException, InternalServiceException, InvalidInputException, OperationTimeoutException]; TestConnection and UpdateIntegrationResourceProperty errors as quoted above. 'NotImplemented' appears in none of them.

## Steps to Reproduce
1. Clone this checkout and boot the emulator built from it.
2. Cause 1: CreateDatabase + CreateTable, CreateMLTransform(InputRecordTables=[that table], Parameters={TransformType: FIND_MATCHES, FindMatchesParameters:{PrimaryKeyColumnName:'id'}}, Role=<glue role>); GetMLTransform succeeds; then CancelMLTaskRun(TransformId='<absent id>', TaskRunId='<absent id>') and read Error.Code.
3. Cause 2: create an s3tables-federated catalog, then UpdateCatalog(CatalogId=<that catalog>, CatalogInput={...}); then UpdateCatalog with an unknown CatalogId. Read Error.Code/Message for both.
4. Cause 3: CreateCrawler with no Schedule; StartCrawlerSchedule(CrawlerName=<it>) -> expect NoScheduleException. Repeat on a crawler created with Schedule='cron(30 8 * * ? *)' -> expect success and Schedule.State=SCHEDULED.
5. Cause 4: GetDataQualityModel(ProfileId='dqprofile-'+40 hex chars); GetDataQualityModelResult(StatisticId='st-'+32 hex, ProfileId=same); PutDataQualityProfileAnnotation(ProfileId=same, InclusionAnnotation='INCLUDE'). Read Error.Code/Message for each.
6. Cause 5: TestConnection(TestConnectionInput={ConnectionType:'JDBC', ConnectionProperties:{JDBC_CONNECTION_URL, USERNAME, PASSWORD}}); then TestConnection(TestConnectionInput={ConnectionType:'SALESFORCE', ConnectionProperties:{}, AuthenticationConfiguration:{AuthenticationType:'OAUTH2'}}).
7. Cause 6: CreateDatabase, then UpdateIntegrationResourceProperty(ResourceArn='arn:aws:glue:<region>:<acct>:database/<db>') with no processing properties; then with ResourceArn of an rds cluster ARN and TargetProcessingProperties={RoleArn}; then the valid Glue database ARN with TargetProcessingProperties and read it back with GetIntegrationResourceProperty.
8. Re-replay the six checks listed in trace_ids to confirm the fix.
