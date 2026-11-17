Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: glue.get-tags-exact-map-through-tag-untag-round-trip glue.get-tags-after-delete-database-and-registry-not-found glue.delete-column-statistics-repeat-preserves-sibling-column glue.get-column-statistics-for-table-nonexistent-rejected glue.database-rename-rejected-leaves-no-database-under-new-name glue.catalog-encryption-without-a-key-resolves-to-the-aws-managed-glue-key glue.delete-form-type-referenced-by-asset-type-fails glue.form-type-delete-referenced-by-asset-type-rejected glue.stop-crawler-schedule-on-a-crawler-with-no-schedule-rejected
Issue-Shape-Id: com.amazonaws.glue#AWSGlue
Issue-Key: glue|GetTags|InvalidInputException
Issue-Key: glue|DeleteColumnStatisticsForTable|EntityNotFoundException
Issue-Key: glue.get-column-statistics-for-table-nonexistent-rejected|expect_error#7|AssertionError|assert got_message == message, f"expected message {message!r}, got {got_message!r}"
Issue-Key: glue.database-rename-rejected-leaves-no-database-under-new-name|expect_error#17|AssertionError|raise AssertionError(f"expected {(code, message)!r}, but the call succeeded: {result!r}")
Issue-Key: glue.catalog-encryption-without-a-key-resolves-to-the-aws-managed-glue-key|run#13|AssertionError|assert key_ref in (managed_key["KeyId"], managed_key["Arn"], "alias/aws/glue"), (
Issue-Key: glue.delete-form-type-referenced-by-asset-type-fails|run#22|AssertionError|assert conflict == ("ConflictException", conflict_message), (
Issue-Key: glue.form-type-delete-referenced-by-asset-type-rejected|run#21|AssertionError|assert conflict_code == "ConflictException", f"unexpected error code: {conflict_code}"
Issue-Key: glue.stop-crawler-schedule-on-a-crawler-with-no-schedule-rejected|run#13|AssertionError|assert message == "Scheduler is already stopped.", (
Issue-Commit: 666f7954233f9ddfcb1a0dcc5f57d6c1921794e2

# glue catalog surface: 7 net-new causes — GetTags refuses database/registry ARNs, a repeat column-statistics delete is not idempotent, a database rename is accepted, catalog SSE-KMS resolves no key, DeleteFormType ignores asset-type references, and two refusal messages are emulator-authored

> _Note: this issue was identified against the code base at commit `666f7954233f9ddfcb1a0dcc5f57d6c1921794e2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetTags

## Description
Seven distinct root causes in the Glue catalog/tagging/metadata-forms surface of service-com-amazonaws-glue-awsglue. All nine failing checks below are listed under service heading 'glue' with NO 'OWNED by' owner and NO 'ALREADY FILED' marker. SEVEN causes must be closed before this issue is submitted.

Cause 1 of 7: GetTags refuses a well-formed Glue DATABASE or REGISTRY ARN with InvalidInputException and an EMPTY message.
AWS: Smithy: GetTags input {ResourceArn @required GlueResourceArn, @length 1..10240, pattern ^arn:aws(-(cn|us-gov|iso(-[bef])?))?:glue:}; output {Tags: TagsMap}; errors EntityNotFoundException, InvalidInputException, OperationTimeoutException (client, 400) and InternalServiceException. Recorded behavior: GetTags on 'arn:aws:glue:<region>:<account>:database/<name>' and on a registry ARN returned by CreateRegistry answers HTTP 200 with exactly the tag map TagResource/UntagResource last left (the map CreateDatabase/CreateRegistry seeded, never a superset or subset); after the resource is deleted the same call is 400 EntityNotFoundException — 'Database <name> not found.' for a database and 'Registry is not found. RegistryName: null, RegistryArn: <arn>' for a registry (fixed text plus the name/ARN from the request).
Emulator: the call is refused 400 InvalidInputException with an EMPTY Error.Message, for a database and a registry that both exist (replay detail: "botocore.errorfactory.InvalidInputException: An error occurred (InvalidInputException) when calling the GetTags operation:" with nothing after the colon). Job ARNs are accepted by the same handler, so the ARN-form parsing for these two resource types is the gap.
Why: tags on databases and registries become entirely unreadable, so tag-driven inventory, cost allocation and TagResource/UntagResource round trips cannot be verified at all; and an empty message gives an operator nothing to diagnose. AWS never answers a refusal with an empty message.
Fix: the GetTags handler's ResourceArn parsing/resolution in the glue crate — recognise database/ and registry/ resource types and read their stored tag maps; the not-found branch must then word the two messages above.
Traces: glue.get-tags-exact-map-through-tag-untag-round-trip, glue.get-tags-after-delete-database-and-registry-not-found

Cause 2 of 7: a repeated DeleteColumnStatisticsForTable is not a no-op — it raises at the top level.
AWS: Smithy: DeleteColumnStatisticsForTable input {CatalogId, DatabaseName @required, TableName @required, ColumnName @required}; output {} (empty); declares EntityNotFoundException/InvalidInputException/GlueEncryptionException/OperationTimeoutException. Recorded behavior: deleting a column's table statistics twice succeeds both times (the second call is a no-op at HTTP 200), the sibling column's statistics stay readable, and the now-missing column is reported IN BAND by GetColumnStatisticsForTable's Errors list with ErrorDetail.ErrorCode='EntityNotFoundException' and ErrorMessage='Statistics do not exist for this column'.
Emulator: the SECOND delete raises 400 EntityNotFoundException 'Statistics do not exist for this column' — the in-band per-column error text promoted into a top-level refusal (replay detail: "botocore.errorfactory.EntityNotFoundException: ... when calling the DeleteColumnStatisticsForTable operation: Statistics do not exist for this column").
Why: cleanup and reconciliation code deletes statistics unconditionally; a non-idempotent delete turns a retry or a re-run into a hard failure, and the error surfaces at the wrong layer (top-level instead of GetColumnStatisticsForTable.Errors).
Fix: the DeleteColumnStatisticsForTable handler — make the delete idempotent for an absent column, and keep the 'Statistics do not exist for this column' text where AWS puts it, in GetColumnStatisticsForTable's ColumnErrors entries.
Traces: glue.delete-column-statistics-repeat-preserves-sibling-column

Cause 3 of 7: GetColumnStatisticsForTable's table-not-found message is the generic one.
AWS: Smithy: GetColumnStatisticsForTable input {CatalogId, DatabaseName @required, TableName @required, ColumnNames @required (0..100)}; declares EntityNotFoundException (client, 400). Recorded behavior: a TableName that was never created in an existing database is refused with 'Table <table name> not found.' — fixed text 'Table ' / ' not found.' with the TableName from the request interpolated.
Emulator: 400 EntityNotFoundException with the generic 'Entity Not Found' (replay detail: "expected message 'Table sim2real-behav-colstat-missing-<id> not found.', got 'Entity Not Found'"; the name is this run's requested table filling the template).
Why: the refusal does not say WHICH entity is missing, so a caller reading several tables' statistics cannot attribute the failure, and message-keyed handling fails.
Fix: the GetColumnStatisticsForTable handler's table-lookup branch message.
Traces: glue.get-column-statistics-for-table-nonexistent-rejected

Cause 4 of 7: UpdateDatabase ACCEPTS a rename.
AWS: Smithy: UpdateDatabase input {CatalogId, Name @required NameString, DatabaseInput @required}; declares InvalidInputException (client, 400). Recorded behavior: an UpdateDatabase whose DatabaseInput.Name differs from the Name being updated is refused 400 InvalidInputException with the fixed message 'Database cannot be renamed' (no variable part); the original database is left untouched, and GetDatabase on the new name is EntityNotFoundException. A same-name UpdateDatabase round-trips Description and LocationUri normally.
Emulator: the call succeeds — HTTP 200 with an empty body (replay detail: "AssertionError: expected ('InvalidInputException', 'Database cannot be renamed'), but the call succeeded: {... 'HTTPStatusCode': 200 ...}").
Why: this is an overly permissive write on catalog identity. A rename AWS rejects is applied here, so a client's database can silently move (or be duplicated) under a new name, and every table/partition reference keyed on the old name diverges between emulator and AWS.
Fix: the UpdateDatabase handler — compare DatabaseInput.Name with the request's Name and refuse with the modeled InvalidInputException before touching state.
Traces: glue.database-rename-rejected-leaves-no-database-under-new-name

Cause 5 of 7: catalog SSE-KMS with no key does not resolve to the AWS managed Glue key.
AWS: Smithy: PutDataCatalogEncryptionSettings input {CatalogId, DataCatalogEncryptionSettings @required}; EncryptionAtRest carries CatalogEncryptionMode and SseAwsKmsKeyId. Recorded behavior: enabling CatalogEncryptionMode='SSE-KMS' with NO SseAwsKmsKeyId resolves the setting to the AWS managed key alias/aws/glue, so GetDataCatalogEncryptionSettings reports an SseAwsKmsKeyId that names that alias — its KeyId, its key ARN, or the literal 'alias/aws/glue' — and a table created right after is readable.
Emulator: GetDataCatalogEncryptionSettings reports CatalogEncryptionMode='SSE-KMS' but SseAwsKmsKeyId as the EMPTY STRING, naming no key at all (replay detail: "catalog resolved SseAwsKmsKeyId to '', which names neither alias/aws/glue's KeyId nor its Arn nor the alias itself"; the KeyId/ARN in the detail are this run's values and are not part of the requirement).
Why: the resolved key id is how an operator verifies WHICH CMK protects the catalog; an empty value says the catalog is SSE-KMS encrypted by nothing, and any client that reads the setting to grant kms:Decrypt on that key has no key to grant on.
Fix: the PutDataCatalogEncryptionSettings/GetDataCatalogEncryptionSettings path and the catalog-encryption state — on SSE-KMS with no key supplied, resolve and persist the account's alias/aws/glue managed key and project it in the read.
Traces: glue.catalog-encryption-without-a-key-resolves-to-the-aws-managed-glue-key

Cause 6 of 7: DeleteFormType deletes a form type an asset type still references.
AWS: Smithy: DeleteFormType input {Identifier @required FormTypeId @length 1..256}; declares ConflictException (client, 400) alongside AccessDenied/ConcurrentModification/InvalidInput/Throttling. Recorded behavior: DeleteFormType on a form type that an existing asset type lists in its Forms is refused 400 ConflictException and leaves the form type intact; the same call succeeds once the referencing asset type is deleted, and the deleted form type then 404s on read. (The companion check also records that PutAssetType naming a missing form type is EntityNotFoundException with the JSON-envelope message '{"Message":"Metadata form type <name> not found."}', and GetAssetType on a never-created asset type is '{"Message":"Asset type <name> not found."}' — fixed text plus the name from the request.)
Emulator: the delete SUCCEEDS while the reference exists — no error is raised at all (replay details: "AssertionError: expected ConflictException, got ('<succeeded>', '')" and "AssertionError: unexpected error code: None").
Why: this is an overly permissive delete that corrupts the metadata-forms graph: an asset type is left referencing a form type that no longer exists, so assets validated against it behave differently from AWS, and the ConflictException a client handles never occurs.
Fix: the DeleteFormType handler and the asset-type/form-type reference state in the glue crate — refuse with ConflictException while any asset type references the form type.
Traces: glue.delete-form-type-referenced-by-asset-type-fails, glue.form-type-delete-referenced-by-asset-type-rejected

Cause 7 of 7: StopCrawlerSchedule's SchedulerNotRunningException message is emulator-authored.
AWS: Smithy: StopCrawlerSchedule input {CrawlerName @required}; declares SchedulerNotRunningException (client, 400) alongside EntityNotFoundException/SchedulerTransitioningException/OperationTimeoutException. Recorded behavior: on a crawler that was never given a cron schedule the refusal message is the fixed sentence 'Scheduler is already stopped.' with NO variable part — the crawler name is not echoed. On a scheduled crawler the same call succeeds, the schedule reads back NOT_SCHEDULED and the stored ScheduleExpression is preserved.
Emulator: the code is right (SchedulerNotRunningException) but the message is 'Scheduler for crawler with name <crawler name> is not running' — different words with the requested crawler name interpolated (the name in the replay detail is this run's value filling the emulator's template).
Why: the message is a requirement of the refusal; clients and runbooks that match or display Glue's sentence do not recognise this one, and the per-request interpolation makes the text vary where AWS's is constant.
Fix: the StopCrawlerSchedule handler's not-running branch message.
Traces: glue.stop-crawler-schedule-on-a-crawler-with-no-schedule-rejected

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout: 503/724 passed. All nine checks appear under service heading 'glue', unowned and unmarked.

Cause 1 — glue.get-tags-exact-map-through-tag-untag-round-trip [error] and glue.get-tags-after-delete-database-and-registry-not-found [error], both: "botocore.errorfactory.InvalidInputException: An error occurred (InvalidInputException) when calling the GetTags operation:" (message empty). The first call in each check is GetTags on 'arn:aws:glue:<region>:<account>:database/<db created with Tags>'; AWS returns 200 with that exact map. The second check further requires, after DeleteDatabase/DeleteRegistry, EntityNotFoundException with 'Database <db> not found.' and 'Registry is not found. RegistryName: null, RegistryArn: <registry arn>'.
Cause 2 — glue.delete-column-statistics-repeat-preserves-sibling-column [error]: after UpdateColumnStatisticsForTable for two columns, the SECOND DeleteColumnStatisticsForTable(ColumnName='shipment_code') raises EntityNotFoundException 'Statistics do not exist for this column'. AWS: both deletes are 200, and the following GetColumnStatisticsForTable returns the sibling column's stats plus Errors=[{ColumnName:'shipment_code', Error:{ErrorCode:'EntityNotFoundException', ErrorMessage:'Statistics do not exist for this column'}}].
Cause 3 — glue.get-column-statistics-for-table-nonexistent-rejected [mismatch]: expected ('EntityNotFoundException', 'Table <missing table> not found.'); got ('EntityNotFoundException', 'Entity Not Found').
Cause 4 — glue.database-rename-rejected-leaves-no-database-under-new-name [mismatch]: expected ('InvalidInputException', 'Database cannot be renamed'); got HTTP 200 with an empty JSON body.
Cause 5 — glue.catalog-encryption-without-a-key-resolves-to-the-aws-managed-glue-key [mismatch], line 56: "catalog resolved SseAwsKmsKeyId to '', which names neither alias/aws/glue's KeyId nor its Arn nor the alias itself". CatalogEncryptionMode='SSE-KMS' did settle correctly.
Cause 6 — glue.delete-form-type-referenced-by-asset-type-fails [mismatch], line 70: "expected ConflictException, got ('<succeeded>', '')"; glue.form-type-delete-referenced-by-asset-type-rejected [mismatch], line 62: "unexpected error code: None". Both reach that point only after PutFormType + PutAssetType(Forms={<form>:{FormTypeIdentifier:<form>}}) and a GetAssetType confirming the reference.
Cause 7 — glue.stop-crawler-schedule-on-a-crawler-with-no-schedule-rejected [mismatch], line 41: "expected the SchedulerNotRunningException message 'Scheduler is already stopped.', got 'Scheduler for crawler with name sim2real-behav-glue-stopbare-<id> is not running'".

Model evidence (services/service-com-amazonaws-glue-awsglue/smithy-model/com-amazonaws-glue-awsglue.json): GetTags {ResourceArn @required GlueResourceArn pattern ^arn:aws(-(cn|us-gov|iso(-[bef])?))?:glue:} -> {Tags: TagsMap @length 0..50}, errors [EntityNotFoundException, InternalServiceException, InvalidInputException, OperationTimeoutException]; DeleteColumnStatisticsForTable output {} ; GetColumnStatisticsForTable output {ColumnStatisticsList, Errors: ColumnErrors}; UpdateDatabase errors include InvalidInputException; PutDataCatalogEncryptionSettings {DataCatalogEncryptionSettings @required}; DeleteFormType errors include ConflictException; StopCrawlerSchedule errors = [EntityNotFoundException, OperationTimeoutException, SchedulerNotRunningException, SchedulerTransitioningException].

## Steps to Reproduce
1. Clone this checkout and boot the emulator built from it.
2. Cause 1: CreateDatabase(DatabaseInput={Name}, Tags={'billing-unit':'warehouse-eng','retention':'short'}) and CreateRegistry(RegistryName, Tags={...}); GetTags('arn:aws:glue:<region>:<acct>:database/<db>') and GetTags(<RegistryArn>) — both must be 200 with exactly those maps; exercise TagResource/UntagResource on each and re-read; then DeleteDatabase/DeleteRegistry and read the EntityNotFoundException messages.
3. Cause 2: CreateDatabase + CreateTable with two columns; UpdateColumnStatisticsForTable for both; DeleteColumnStatisticsForTable(ColumnName=<first>) TWICE — the second must be 200; then GetColumnStatisticsForTable(ColumnNames=[both]) and read the Errors list.
4. Cause 3: CreateDatabase, then GetColumnStatisticsForTable(DatabaseName=<db>, TableName=<never created>, ColumnNames=['amount']) and read Error.Message.
5. Cause 4: CreateDatabase(Name=A); UpdateDatabase(Name=A, DatabaseInput={Name:B,...}) must be InvalidInputException 'Database cannot be renamed'; then GetDatabase(A) must still return the original and GetDatabase(B) must be EntityNotFoundException.
6. Cause 5: GetDataCatalogEncryptionSettings (save it), PutDataCatalogEncryptionSettings(EncryptionAtRest={CatalogEncryptionMode:'SSE-KMS'}) with NO SseAwsKmsKeyId, poll GetDataCatalogEncryptionSettings and read EncryptionAtRest.SseAwsKmsKeyId — it must name alias/aws/glue (alias, KeyId or key ARN); restore the original settings.
7. Cause 6: PutFormType(Name, Schema); PutAssetType(Name, Forms={<form>:{FormTypeIdentifier:<form>}}); DeleteFormType(Identifier=<form>) must be ConflictException and the form type must still be readable; DeleteAssetType then DeleteFormType must succeed and the form type must 404. Also PutAssetType naming a missing form type must be EntityNotFoundException '{"Message":"Metadata form type <name> not found."}' and create nothing.
8. Cause 7: CreateCrawler with no Schedule; StopCrawlerSchedule(CrawlerName=<it>) and read Error.Message; then on a crawler created with Schedule='cron(30 8 * * ? *)' StopCrawlerSchedule must succeed, the schedule state must read NOT_SCHEDULED and ScheduleExpression must be preserved.
9. Re-replay the nine checks listed in trace_ids to confirm the fix.
