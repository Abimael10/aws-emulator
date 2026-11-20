Issue-Source: trace-sweep-divergence
Issue-Trace: glue-aorigin-get-tags-resource-arn-forms
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: glue-aorigin-get-tags-resource-arn-forms glue-gjwr-traj-mutations-through-the-wrong-region glue-column-statistics-schedule-and-modify-integration-errors glue-table-optimizer-column-statistics-source-control-not-found-errors
Issue-Shape-Id: com.amazonaws.glue#AWSGlue
Issue-Key: glue|GetTags|4xx->4xx|$.Message
Issue-Key: glue|GetTags|4xx->4xx|$.__type
Issue-Key: glue|StartColumnStatisticsTaskRunSchedule|4xx->4xx|$.Message
Issue-Key: glue|StopColumnStatisticsTaskRunSchedule|4xx->4xx|$.Message
Issue-Commit: 3e9576030bb65d42a81780c53475bd039f2ab2f6

# Glue not-found refusals name the wrong entity and the wrong error class

> _Note: this issue was identified against the code base at commit `3e9576030bb65d42a81780c53475bd039f2ab2f6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetTags

## Description
This issue bundles 4 net-new causes, all of them the same fault: when a Glue operation cannot resolve the entity a request names, the emulator refuses with an error that identifies the wrong entity — and in one case with the wrong error class altogether — instead of AWS's EntityNotFoundException naming the entity that is actually absent. A fixer must close ALL FOUR causes below before submitting.

Cause 1 of 4: GetTags on a well-formed Glue resource ARN whose resource does not exist is refused with the wrong error class and with no Message at all.
AWS: Smithy (com-amazonaws-glue-awsglue) GetTags(ResourceArn required, target GlueResourceArn, @length 1..10240, @pattern ^arn:aws(-(cn|us-gov|iso(-[bef])?))?:glue:) models both EntityNotFoundException (client, HTTP 400) and InvalidInputException (client, HTTP 400). InvalidInputException is for an ARN that does not satisfy the shape's grammar; an ARN that IS well-formed but names a Glue entity that does not exist is an absence, so AWS answers 400 EntityNotFoundException. Recorded behavior for the message: the body carries __type 'EntityNotFoundException' and Message 'Entity Not Found' — that text is fixed, with no variable part interpolated from the request.
Emulator: the same request is answered 400 with __type 'InvalidInputException' and the Message member absent entirely (the replay reports `$.__type: value_mismatch (expected 'EntityNotFoundException', got 'InvalidInputException')` and `$.Message: missing (expected 'Entity Not Found', got None)`). So the emulator treats an unresolvable-but-legal ARN as a malformed ARN, and emits an error document with no message.
Why: a client branches on the error code — EntityNotFoundException means "the resource is gone / not created yet" and is retried or handled by creating the resource, while InvalidInputException means "your ARN is wrong" and is a non-retryable programming error. The emulator inverts that decision, and because Message is missing, any client or log that renders the error text gets an empty reason.
Fix: the GetTags handler's ARN resolution path — it must distinguish grammar validation (InvalidInputException) from entity lookup failure (EntityNotFoundException with the fixed Message 'Entity Not Found'), and its error serializer must always populate Message.
Traces: glue-aorigin-get-tags-resource-arn-forms#task#6.

Cause 2 of 4: GetTags on a Glue job ARN that resolves to no job reports a generic entity message instead of naming the job.
AWS: Smithy GetTags models EntityNotFoundException (client, HTTP 400). Recorded behavior: for a job ARN that names no existing job (here, the ARN of a job that lives in another region, so it is absent from the catalog being addressed), AWS answers 400 EntityNotFoundException with Message 'Job not found.' — the whole sentence is fixed text naming the entity TYPE that was looked up; no name or id from the request is interpolated.
Emulator: the same call answers 400 with Message 'Entity not found' — the emulator's own generic sentence, which names no entity type and differs in wording and in the absence of the trailing period.
Why: Glue returns a per-entity-type sentence precisely so a caller tagging/reading many resource kinds can tell WHICH lookup failed; a client (or test) that matches AWS's 'Job not found.' never matches, and an operator reading the generic text cannot tell a missing job from a missing database or crawler. This also masks the regional scoping the recording exercises: the request is legitimately addressing a different region's catalog.
Fix: the GetTags handler's not-found error construction (and the shared ARN-to-entity lookup it calls) must raise EntityNotFoundException with the per-entity-type sentence AWS uses for the resolved ARN's entity type, rather than one generic 'Entity not found'.
Traces: glue-gjwr-traj-mutations-through-the-wrong-region#task#4.

Cause 3 of 4: StartColumnStatisticsTaskRunSchedule reports a missing schedule as a missing settings-entry sentence of the emulator's own wording instead of AWS's account-qualified ColumnStatisticsTaskSettings identifier.
AWS: Smithy StartColumnStatisticsTaskRunSchedule(DatabaseName required, TableName required, CatalogID optional) models EntityNotFoundException (client, HTTP 400). Recorded behavior for the message: when no ColumnStatisticsTaskSettings exist for the (catalog, database, table) triple, the Message is 'ColumnStatisticsTaskSettings <settingsKey> not found.' — fixed text 'ColumnStatisticsTaskSettings ' and ' not found.', with the variable part being the settings identifier built from the request and the caller's account, '<catalogId>:<accountId>:<DatabaseName>-<TableName>' (for the default catalog both id components are the caller's account id).
Emulator: the same call answers 400 with Message 'ColumnStatisticsTaskSettings entry for input DatabaseName and TableName does not exist' — a fixed emulator-authored sentence that interpolates nothing, so neither the database name, the table name nor the account-qualified settings key ever appears.
Why: a client that must distinguish "this table has no column-statistics schedule yet" (recover by calling CreateColumnStatisticsTaskSettings) from other failures parses the composite settings key out of AWS's message to learn which catalog/database/table the absence is about; the emulator's sentence carries no identifier at all, so that recovery and any text match against AWS fail.
Fix: the error-construction path of the StartColumnStatisticsTaskRunSchedule handler (and the shared column-statistics-settings lookup it uses) must build the EntityNotFoundException message from the request's DatabaseName/TableName and the resolved catalog/account id.
Traces: glue-column-statistics-schedule-and-modify-integration-errors#task#3 (its cluster also covers task#4 of the same fixture).

Cause 4 of 4: StopColumnStatisticsTaskRunSchedule reports the missing settings entry as a missing DATABASE.
AWS: Smithy StopColumnStatisticsTaskRunSchedule(DatabaseName required, TableName required, CatalogID optional) models EntityNotFoundException (client, HTTP 400). Recorded behavior: the absence of ColumnStatisticsTaskSettings for the (catalog, database, table) triple is reported with the same template as the Start leg — 'ColumnStatisticsTaskSettings <catalogId>:<accountId>:<DatabaseName>-<TableName> not found.', fixed text plus the request/account-derived settings key. The entity that is absent is the settings record, not the database.
Emulator: the same call answers 400 with Message 'Database <DatabaseName> not found.' (fixed text 'Database ' / ' not found.' with the requested database name interpolated), i.e. it fails on an earlier database lookup and reports that instead of looking up and reporting the settings entity.
Why: the caller is told the database does not exist, which is a different and more severe condition than "no schedule configured"; a client takes the catalog-bootstrap branch (recreate the database, losing or duplicating catalog state) rather than simply creating the settings. Start and Stop must also agree on the message, and here they disagree with each other as well as with AWS.
Fix: the StopColumnStatisticsTaskRunSchedule handler must order its lookups so the settings entry is the reported absence, and construct the same EntityNotFoundException message template as the Start leg.
Traces: glue-table-optimizer-column-statistics-source-control-not-found-errors#task#1.

Related open issues that do NOT carry these replay keys and do not cover these causes: 20261117T133709Z-glue-entity-not-found-refusals-carry-emulator-authored.md and 20261119T084143Z-glue-refusal-messages-diverge-startcrawlerschedule-s-entitynotfoundexception-and.md (emulator-authored not-found wording for other operations); 20261120T030206Z-glue-iam-s3-glue-startcolumnstatisticstaskrunschedule-stopcolumnstatisticstaskrunschedule-report-a-missing-schedule-as.md (a BEHAVIORAL check on the same two column-statistics operations, which records the emulator answering 'Table <name> not found.'; the wire replay here shows two different wrong messages — 'ColumnStatisticsTaskSettings entry for input DatabaseName and TableName does not exist' for Start and 'Database <name> not found.' for Stop — and carries the wire replay keys that issue has none of); 20261120T051845Z-glue-gettags-refuses-an-existing-untagged-resource-with.md and 20261117T183344Z-glue-catalog-surface-7-net-new-causes-gettags.md (other GetTags faults: an existing untagged resource refused, and database/registry ARN forms refused — neither is the not-found error class/message divergence filed here).

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up, /__admin/health 200). Four net-new divergence clusters, all service=glue, all verdict=mismatch, none with a stub_path and none owned_by an open issue:

1) glue-aorigin-get-tags-resource-arn-forms#task#6 — GetTags, status 400 -> 400
   expected: __type = 'EntityNotFoundException', Message = 'Entity Not Found'
   actual:   __type = 'InvalidInputException', Message absent (None)
   detail: "$.__type: value_mismatch (expected 'EntityNotFoundException', got 'InvalidInputException')\n$.Message: missing (expected 'Entity Not Found', got None)"

2) glue-gjwr-traj-mutations-through-the-wrong-region#task#4 — GetTags, status 400 -> 400
   expected: Message = 'Job not found.'   (entire sentence is fixed text)
   actual:   Message = 'Entity not found' (emulator-authored, no entity type, no period)

3) glue-column-statistics-schedule-and-modify-integration-errors#task#3 (cluster also covers #task#4) — StartColumnStatisticsTaskRunSchedule, status 400 -> 400
   expected: Message = 'ColumnStatisticsTaskSettings 000000000000:000000000000:<database>-<table> not found.'
             fixed text 'ColumnStatisticsTaskSettings ' + ' not found.'; the middle is '<catalogId>:<accountId>:<DatabaseName>-<TableName>' from the request and the caller's account (the recorded run's account id and the recorded db/table names are that run's values filling the template, not requirements)
   actual:   Message = 'ColumnStatisticsTaskSettings entry for input DatabaseName and TableName does not exist' (no identifier interpolated at all)

4) glue-table-optimizer-column-statistics-source-control-not-found-errors#task#1 — StopColumnStatisticsTaskRunSchedule, status 400 -> 400
   expected: Message = 'ColumnStatisticsTaskSettings <catalogId>:<accountId>:<database>-<table> not found.' (same template as the Start leg)
   actual:   Message = 'Database <DatabaseName> not found.' — fixed text 'Database ' / ' not found.' with the requested database name interpolated; reports the wrong entity type

In all four the HTTP status (400) is right; what diverges is the error code (cause 1) and the error message (causes 1-4), both of which are requirements.

Model evidence (smithy_query, services/service-com-amazonaws-glue-awsglue/smithy-model/com-amazonaws-glue-awsglue.json):
- GetTags: errors = EntityNotFoundException(400), InvalidInputException(400), OperationTimeoutException(400), InternalServiceException(500); ResourceArn required, GlueResourceArn @length 1..10240 @pattern ^arn:aws(-(cn|us-gov|iso(-[bef])?))?:glue:
- StartColumnStatisticsTaskRunSchedule: DatabaseName/TableName required, CatalogID optional; errors = AccessDeniedException, EntityNotFoundException, InvalidInputException, OperationTimeoutException (all client, 400)
- StopColumnStatisticsTaskRunSchedule: same input; errors = EntityNotFoundException, InvalidInputException, OperationTimeoutException (all client, 400)

## Steps to Reproduce
Cause 1 — GetTags, wrong error class and missing Message:
1. Call glue:GetTags with a syntactically valid Glue resource ARN (matching ^arn:aws:glue:<region>:<account>:...) that names a Glue entity which does not exist.
2. AWS answers 400 EntityNotFoundException with Message 'Entity Not Found'; the emulator answers 400 InvalidInputException with no Message member.
   Replay: fixture glue-aorigin-get-tags-resource-arn-forms, step task#6.

Cause 2 — GetTags, generic not-found sentence instead of naming the job:
1. Create a Glue job in one region, then call glue:GetTags against another region's endpoint using that job's ARN (so the job is absent from the addressed catalog).
2. AWS answers 400 EntityNotFoundException Message 'Job not found.'; the emulator answers 400 with Message 'Entity not found'.
   Replay: fixture glue-gjwr-traj-mutations-through-the-wrong-region, step task#4.

Cause 3 — StartColumnStatisticsTaskRunSchedule not-found message:
1. Create a Glue database D and a table T in it, but create NO ColumnStatisticsTaskSettings for (D, T).
2. Call glue:StartColumnStatisticsTaskRunSchedule(DatabaseName=D, TableName=T).
3. AWS answers 400 EntityNotFoundException Message 'ColumnStatisticsTaskSettings <catalogId>:<accountId>:D-T not found.'; the emulator answers 400 with the fixed sentence 'ColumnStatisticsTaskSettings entry for input DatabaseName and TableName does not exist'.
   Replay: fixture glue-column-statistics-schedule-and-modify-integration-errors, steps task#3 and task#4.

Cause 4 — StopColumnStatisticsTaskRunSchedule reports the database as missing:
1. Call glue:StopColumnStatisticsTaskRunSchedule(DatabaseName=D, TableName=T) for a (D, T) pair that has no stored ColumnStatisticsTaskSettings.
2. AWS answers 400 EntityNotFoundException with the same ColumnStatisticsTaskSettings '<catalogId>:<accountId>:D-T not found.' message as the Start leg; the emulator answers 400 with 'Database D not found.'
   Replay: fixture glue-table-optimizer-column-statistics-source-control-not-found-errors, step task#1.

Re-replay the trace set simparity-wire-aws-v1 and confirm all four fixture steps above pass.
