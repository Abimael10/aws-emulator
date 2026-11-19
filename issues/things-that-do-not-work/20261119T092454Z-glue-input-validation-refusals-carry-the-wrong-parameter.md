Issue-Source: trace-sweep-divergence
Issue-Trace: glue-integration-resource-property-lifecycle-and-validation
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: glue-integration-resource-property-lifecycle-and-validation glue-data-quality-v2-publish-get-and-list-filters glue-database-update-and-table-batch-delete
Issue-Shape-Id: com.amazonaws.glue#AWSGlue
Issue-Key: glue|UpdateIntegrationResourceProperty|2xx->4xx|$.ResourceArn
Issue-Key: glue|UpdateIntegrationResourceProperty|2xx->4xx|$.ResourcePropertyArn
Issue-Key: glue|UpdateIntegrationResourceProperty|2xx->4xx|$.TargetProcessingProperties
Issue-Key: glue|UpdateIntegrationResourceProperty|2xx->4xx|status
Issue-Key: glue|GetDataQualityModelResult|4xx->4xx|$.Message
Issue-Key: glue|BatchDeleteTableVersion|2xx->2xx|$.Errors[*].ErrorDetail.ErrorMessage
Issue-Key: glue|UpdateIntegrationResourceProperty|4xx->4xx|$.Message
Issue-Key: glue|UpdateIntegrationResourceProperty|4xx->4xx|$.__type
Issue-Commit: a1015014620f1f215b776c4cca11b42cfc3a6893

# Glue input-validation refusals carry the wrong parameter, error type and status

> _Note: this issue was identified against the code base at commit `a1015014620f1f215b776c4cca11b42cfc3a6893`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateIntegrationResourceProperty

## Description
Four net-new Glue causes in which the emulator's input-validation / refusal path diverges from AWS: it names the wrong request member, emits the wrong exception type, drops a value out of a per-entry error message, and refuses with 400 a request AWS accepts with 200. All four are glue; this bundle carries 4 causes and all four must be closed.

Cause 1 of 4 — GetDataQualityModelResult blames the wrong request member (fixture glue-data-quality-v2-publish-get-and-list-filters, step task#2).
AWS: Smithy GetDataQualityModelResult input requires both StatisticId and ProfileId (target HashString, @length min 1 max 255); modeled client error InvalidInputException (httpError 400). The recording shows AWS validating the StatisticId identifier grammar and refusing HTTP 400 with the message 'Invalid statisticId format. Valid identifiers begin with st-'. That whole sentence is fixed text — it names the member being validated (statisticId) and the required identifier prefix (st-); nothing in it comes from the request.
Emulator: HTTP 400 with Message 'Invalid profileId format. Valid identifiers begin with dqprofile-' — it validates/reports the OTHER member of the same request, so the refusal describes a constraint on ProfileId instead of the StatisticId grammar AWS rejected.
Why: a client that reads the message corrects the wrong field (its ProfileId, which AWS accepted) and the call keeps failing; error-message assertions and user-facing surfacing of which argument is malformed are both wrong.
Fix: the GetDataQualityModelResult input validation — validate the StatisticId identifier grammar (and report it) before/independently of ProfileId, and word the refusal as AWS does.
Traces: glue-data-quality-v2-publish-get-and-list-filters#task#2.

Cause 2 of 4 — BatchDeleteTableVersion's per-version ErrorDetail.ErrorMessage omits the version id and re-words the sentence (fixture glue-database-update-and-table-batch-delete, step task#12).
AWS: Smithy BatchDeleteTableVersion returns HTTP 200 with Errors: list<TableVersionError>, each carrying TableName, VersionId and ErrorDetail (ErrorCode, ErrorMessage @length 0..2048). For a version id that does not exist the recording shows ErrorDetail.ErrorMessage = 'Table version (0) not found'. The template is the fixed text 'Table version (' <VersionId> ') not found' with no trailing period; the only variable part is the version id taken from the request's VersionIds entry.
Emulator: ErrorMessage 'Table version not found.' — different fixed text, a trailing period AWS does not emit, and the requested VersionId dropped entirely.
Why: BatchDeleteTableVersion reports failures per entry inside a 200, so this message is the only signal of WHICH version failed; a client that logs or re-drives per-version failures cannot tell them apart.
Fix: the BatchDeleteTableVersion handler's per-entry error construction (the TableVersionError/ErrorDetail it builds in its state lookup) — interpolate the requested version id into AWS's sentence.
Traces: glue-database-update-and-table-batch-delete#task#12.

Cause 3 of 4 — UpdateIntegrationResourceProperty refuses with 400 a request AWS accepts with 200 (fixture glue-integration-resource-property-lifecycle-and-validation, step task#2 — the representative of this bundle).
AWS: Smithy UpdateIntegrationResourceProperty takes a required ResourceArn (String512, @length 1..512) plus optional SourceProcessingProperties / TargetProcessingProperties, and returns ResourceArn, ResourcePropertyArn, SourceProcessingProperties and TargetProcessingProperties. The recording shows HTTP 200 for an update against an existing Glue database ARN: ResourceArn echoed back as the database ARN that was sent, ResourcePropertyArn as the integration-resource-property ARN derived from it (arn:aws:glue:<region>:<account>:integrationresourceproperty/database/<database-name>), and TargetProcessingProperties echoed back as supplied (the RoleArn the caller sent).
Emulator: HTTP 400 — the call is refused, and ResourceArn, ResourcePropertyArn and TargetProcessingProperties are all absent from the response.
Why: the operation never succeeds, so the integration-resource-property lifecycle cannot be driven at all — a client cannot set processing properties on a resource, and every subsequent read/delete in the lifecycle has nothing to act on. Open issue 20261117T133709Z-glue-integration-resource-property-is-never-persisted-so.md describes a related persistence gap but does not carry this fixture step's replay key, so this cause is filed here.
Fix: the UpdateIntegrationResourceProperty handler and its validation — accept an update whose ResourceArn names an existing Glue resource, persist the processing properties, and serialise the ResourceArn / derived ResourcePropertyArn / echoed processing properties.
Traces: glue-integration-resource-property-lifecycle-and-validation#task#2.

Cause 4 of 4 — UpdateIntegrationResourceProperty's malformed-ARN refusal uses the wrong exception type and adds a Message AWS does not send (fixture glue-integration-resource-property-lifecycle-and-validation, step task#4).
AWS: UpdateIntegrationResourceProperty models both InvalidInputException and ValidationException (both client, httpError 400). For a ResourceArn that is not an ARN at all the recording shows HTTP 400 with __type 'InvalidInputException' and NO Message member on the body (recorded behavior only — the model documents Message as optional on InvalidInputException, and AWS omits it here).
Emulator: HTTP 400 with __type 'ValidationException' and Message 'Encountered invalid resource arn <the supplied ResourceArn>' — wrong error code, plus a message member the recorded response does not carry.
Why: the error code is the part clients dispatch on; an SDK's catch of InvalidInputException (or a retry/branch keyed on it) does not match ValidationException, so a malformed-ARN refusal is misclassified as an unrelated validation failure.
Fix: the UpdateIntegrationResourceProperty validation path — raise InvalidInputException (no Message) for a ResourceArn that fails ARN parsing, rather than ValidationException with an emulator-authored sentence.
Traces: glue-integration-resource-property-lifecycle-and-validation#task#4.

## Evidence
Replay of trace set simparity-wire-aws-v1 (1613 steps, 1370 passed, score 84.9%); 4 net-new divergence clusters, all service=glue, none blocked by a stub and none owned by an open issue.

Cause 1 — GetDataQualityModelResult, glue-data-quality-v2-publish-get-and-list-filters#task#2: expected status 400, actual 400. $.Message: expected 'Invalid statisticId format. Valid identifiers begin with st-', got 'Invalid profileId format. Valid identifiers begin with dqprofile-'.

Cause 2 — BatchDeleteTableVersion, glue-database-update-and-table-batch-delete#task#12: expected status 200, actual 200. $.Errors[0].ErrorDetail.ErrorMessage: expected 'Table version (0) not found', got 'Table version not found.' (the '0' is the VersionIds entry from the request; the trailing period is emulator-added).

Cause 3 — UpdateIntegrationResourceProperty, glue-integration-resource-property-lifecycle-and-validation#task#2: expected status 200, got 400. $.ResourceArn missing (AWS returned the supplied database ARN, e.g. arn:aws:glue:<region>:<account>:database/<db-name>); $.ResourcePropertyArn missing (AWS returned arn:aws:glue:<region>:<account>:integrationresourceproperty/database/<db-name>); $.TargetProcessingProperties missing (AWS echoed {'RoleArn': <role arn supplied in the request>}). Account id, region and the generated database/role names are environment-specific and are not part of the requirement; the 200 status and the three members are.

Cause 4 — UpdateIntegrationResourceProperty, glue-integration-resource-property-lifecycle-and-validation#task#4: expected status 400, actual 400. $.__type: expected 'InvalidInputException', got 'ValidationException'. $.Message: unexpected_present — expected absent, got 'Encountered invalid resource arn <supplied ResourceArn>'.

## Steps to Reproduce
Replay the simparity-wire-aws-v1 wire corpus against the emulator built from this checkout, then inspect these steps:

1. Cause 1: fixture glue-data-quality-v2-publish-get-and-list-filters, step task#2 — call glue GetDataQualityModelResult with a StatisticId that does not use the 'st-' prefix (and a well-formed ProfileId). Observe the 400 Message names profileId/dqprofile- instead of statisticId/st-.
2. Cause 2: fixture glue-database-update-and-table-batch-delete, step task#12 — create a database and table, then call BatchDeleteTableVersion with a VersionIds entry that does not exist. Observe Errors[0].ErrorDetail.ErrorMessage is 'Table version not found.' instead of 'Table version (<VersionId>) not found'.
3. Cause 3: fixture glue-integration-resource-property-lifecycle-and-validation, step task#2 — create a Glue database and a role, then call UpdateIntegrationResourceProperty with that database's ARN as ResourceArn and TargetProcessingProperties.RoleArn set. Observe HTTP 400 instead of a 200 carrying ResourceArn, ResourcePropertyArn and TargetProcessingProperties.
4. Cause 4: same fixture, step task#4 — call UpdateIntegrationResourceProperty with ResourceArn set to a string that is not an ARN. Observe __type 'ValidationException' with a Message, instead of __type 'InvalidInputException' with no Message.
