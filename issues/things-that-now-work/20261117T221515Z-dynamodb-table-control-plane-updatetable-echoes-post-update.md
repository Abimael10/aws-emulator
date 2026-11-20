Issue-Source: trace-sweep-divergence
Issue-Trace: dynamodb-create-and-update-table-provisioned-throughput-bounds-rejected
Issue-Step: task#10
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: dynamodb-create-and-update-table-provisioned-throughput-bounds-rejected dynamodb-update-table-capacity-bounds-precedence dynamodb-update-table-decrease-call-response-reports-old-values-while-updating create-table-projection-null-type-enum-and-minlength-rejected dynamodb-create-table-secondary-index-validation-precedence dynamodb-ttl-contributor-insights-and-replica-autoscaling-config
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb|UpdateTable|2xx->2xx|$.TableDescription.WarmThroughput.ReadUnitsPerSecond
Issue-Key: dynamodb|UpdateTable|2xx->2xx|$.TableDescription.WarmThroughput.WriteUnitsPerSecond
Issue-Key: dynamodb|UpdateTable|2xx->2xx|$.TableDescription.ProvisionedThroughput.NumberOfDecreasesToday
Issue-Key: dynamodb|CreateTable|4xx->2xx|$.__type
Issue-Key: dynamodb|CreateTable|4xx->2xx|$.message
Issue-Key: dynamodb|CreateTable|4xx->2xx|status
Issue-Key: dynamodb|DescribeTable|4xx->2xx|$.__type
Issue-Key: dynamodb|DescribeTable|4xx->2xx|$.message
Issue-Key: dynamodb|DescribeTable|4xx->2xx|status
Issue-Key: dynamodb|UpdateContributorInsights|2xx->4xx|$.ContributorInsightsMode
Issue-Key: dynamodb|UpdateContributorInsights|2xx->4xx|$.ContributorInsightsStatus
Issue-Key: dynamodb|UpdateContributorInsights|2xx->4xx|$.TableName
Issue-Key: dynamodb|UpdateContributorInsights|2xx->4xx|status
Issue-Related: 20261113T115951Z-dynamodb-updatetable-reports-the-requested-capacity-immediately-instead.md
Issue-Commit: 656cbf9b11e99225d4fd85cb8b7394b019211c7b

# DynamoDB table control plane: UpdateTable echoes post-update throughput, CreateTable accepts a null ProjectionType (5 causes)

> _Note: this issue was identified against the code base at commit `656cbf9b11e99225d4fd85cb8b7394b019211c7b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateTable

## Description
Five net-new causes on dynamodb in the table control plane: UpdateTable's immediate response reports the requested throughput rather than the table's current throughput while the table is UPDATING (and bumps the decrease counter early), CreateTable accepts a secondary index whose ProjectionType is null — so a table AWS never creates exists in the emulator — and UpdateContributorInsights refuses a modeled ContributorInsightsMode.

Cause 1 of 5: UpdateTable's response reports the newly requested ProvisionedThroughput/WarmThroughput instead of the table's current values.
AWS: Smithy: UpdateTable returns TableDescription, whose ProvisionedThroughput and WarmThroughput describe the table as it currently is; a throughput change is asynchronous (TableStatus becomes UPDATING), so the synchronous response still carries the pre-update ReadCapacityUnits/WriteCapacityUnits and ReadUnitsPerSecond/WriteUnitsPerSecond, and only a later DescribeTable shows the new ones.
Emulator: the response already reports the requested values.
Why: a client that reads the UpdateTable response to confirm what the table is serving right now sees capacity it does not yet have, and a client that polls until the value changes sees it change immediately — masking the UPDATING transition entirely.
Fix: the UpdateTable handler must serialize the TableDescription snapshot taken BEFORE applying the pending throughput change, with the new values parked as pending state.
Traces: dynamodb-create-and-update-table-provisioned-throughput-bounds-rejected#task#10 (cluster of 2 steps, incl. dynamodb-update-table-capacity-bounds-precedence#task#8).

Cause 2 of 5: the same response also reports NumberOfDecreasesToday as already incremented.
AWS: TableDescription.ProvisionedThroughput.NumberOfDecreasesToday counts decreases that have taken effect; in the synchronous response to a decrease it is still the pre-update count, alongside the pre-update capacity values.
Emulator: reports the incremented count (and the decreased capacity) immediately.
Why: clients use NumberOfDecreasesToday to stay inside the daily-decrease quota; counting a decrease that has not yet taken effect makes the remaining budget wrong.
Fix: same handler — increment the decrease counter when the pending change is applied, not when it is accepted.
Traces: dynamodb-update-table-decrease-call-response-reports-old-values-while-updating#task#5.

Cause 3 of 5: CreateTable accepts a secondary index whose Projection.ProjectionType is null.
AWS: Smithy: Projection.ProjectionType targets the ProjectionType enum with values ALL, KEYS_ONLY, INCLUDE. A null there is not a member of the enum, so DynamoDB refuses the table: HTTP 400, __type 'com.amazon.coral.validate#ValidationException', message 'One or more parameter values were invalid: Unknown ProjectionType: null' — fixed text 'One or more parameter values were invalid: Unknown ProjectionType: ' plus the offending value from the request.
Emulator: HTTP 200 — the table is created, with no __type and no message.
Why: an invalid index definition is persisted; the client believes a table with a projection the service cannot honour exists, and every later read of that index diverges.
Fix: the CreateTable handler's secondary-index validation must reject a non-enum/absent ProjectionType before creating state.
Traces: create-table-projection-null-type-enum-and-minlength-rejected#task#1 (cluster of 3 steps, incl. dynamodb-create-table-secondary-index-validation-precedence#task#3).

Cause 4 of 5: DescribeTable answers 200 for the table AWS refused to create.
AWS: because the CreateTable of cause 3 is refused, the table does not exist, so DescribeTable answers HTTP 400, __type 'com.amazonaws.dynamodb.v20120810#ResourceNotFoundException', message 'Requested resource not found: Table: <tableName> not found' (fixed text around the request's TableName).
Emulator: HTTP 200 describing the wrongly created table; no __type, no message.
Why: the divergence is durable table state, not just one refusal — a client probing for the table's absence finds it present.
Fix: follows from cause 3 (no separate change expected once CreateTable refuses).
Traces: create-table-projection-null-type-enum-and-minlength-rejected#task#2 (cluster of 3 steps).

Cause 5 of 5: UpdateContributorInsights refuses a request carrying ContributorInsightsMode.
AWS: Smithy: UpdateContributorInsights.ContributorInsightsMode targets the ContributorInsightsMode enum (ACCESSED_AND_THROTTLED_KEYS, THROTTLED_KEYS) and the output carries TableName, ContributorInsightsStatus and ContributorInsightsMode. A request naming a modeled mode is accepted: HTTP 200 echoing TableName, the resulting ContributorInsightsStatus and the mode.
Emulator: HTTP 400 — none of the three output members is served.
Why: Contributor Insights cannot be configured through the modeled member at all, so the whole feature is unusable from an SDK that sends it.
Fix: the UpdateContributorInsights handler must accept the modeled ContributorInsightsMode and record it in table state, and its serializer must echo mode and status.
Traces: dynamodb-ttl-contributor-insights-and-replica-autoscaling-config#task#13.

## Evidence
Cause 1 — UpdateTable: expected TableDescription.ProvisionedThroughput.ReadCapacityUnits/WriteCapacityUnits and WarmThroughput.ReadUnitsPerSecond/WriteUnitsPerSecond to report the pre-update values (recorded 1); actual the requested values (3) (2 steps).
Cause 2 — UpdateTable decrease: expected NumberOfDecreasesToday=0 and the pre-update capacity (recorded 3/3); actual 1 and the post-update capacity (1/1).
Cause 3 — CreateTable: expected 400 {__type: 'com.amazon.coral.validate#ValidationException', message: 'One or more parameter values were invalid: Unknown ProjectionType: null'}; actual 200 with both members absent (3 steps).
Cause 4 — DescribeTable: expected 400 {__type: 'com.amazonaws.dynamodb.v20120810#ResourceNotFoundException', message: 'Requested resource not found: Table: <tableName> not found'}; actual 200 (3 steps).
Cause 5 — UpdateContributorInsights: expected 200 with ContributorInsightsMode='ACCESSED_AND_THROTTLED_KEYS', ContributorInsightsStatus='DISABLED', TableName=<table>; actual 400 with all three missing.

## Steps to Reproduce
1. Replay fixtures dynamodb-create-and-update-table-provisioned-throughput-bounds-rejected (task#10), dynamodb-update-table-capacity-bounds-precedence (task#8), dynamodb-update-table-decrease-call-response-reports-old-values-while-updating (task#5), create-table-projection-null-type-enum-and-minlength-rejected (task#1, task#2) and dynamodb-ttl-contributor-insights-and-replica-autoscaling-config (task#13).
2. Causes 1-2: CreateTable with PROVISIONED throughput, then UpdateTable changing ReadCapacityUnits/WriteCapacityUnits (both an increase and a decrease) and inspect the returned TableDescription.ProvisionedThroughput/WarmThroughput and NumberOfDecreasesToday.
3. Causes 3-4: CreateTable with a LocalSecondaryIndex/GlobalSecondaryIndex whose Projection is {"ProjectionType": null}; observe 200, then DescribeTable on that name and observe 200.
4. Cause 5: UpdateContributorInsights with ContributorInsightsAction plus ContributorInsightsMode=ACCESSED_AND_THROTTLED_KEYS.
