Issue-Source: trace-sweep-divergence
Issue-Trace: dynamodb-bulk-operation-management-refusals
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: dynamodb-bulk-operation-management-refusals
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb|DescribeBulkOperation|4xx->4xx|$.__type
Issue-Key: dynamodb|DescribeBulkOperation|4xx->4xx|$.message
Issue-Key: dynamodb|CancelBulkOperation|4xx->4xx|$.__type
Issue-Key: dynamodb|CancelBulkOperation|4xx->4xx|$.message
Issue-Key: dynamodb|ListBulkOperations|4xx->4xx|$.__type
Issue-Key: dynamodb|ListBulkOperations|4xx->4xx|$.message
Issue-Key: dynamodb|StartBulkPutItems|4xx->4xx|$.__type
Issue-Key: dynamodb|StartBulkPutItems|4xx->4xx|$.message
Issue-Key: dynamodb|ListBulkOperations|2xx->4xx|$.BulkOperationsSummaryList
Issue-Key: dynamodb|ListBulkOperations|2xx->4xx|status
Issue-Commit: 656cbf9b11e99225d4fd85cb8b7394b019211c7b

# DynamoDB bulk-operation control plane is unrouted: five refusals and one listing answer UnknownOperationException (5 causes)

> _Note: this issue was identified against the code base at commit `656cbf9b11e99225d4fd85cb8b7394b019211c7b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeBulkOperation

## Description
Five net-new causes on dynamodb: the bulk-operation management API family (DescribeBulkOperation, CancelBulkOperation, ListBulkOperations, StartBulkPutItems) is not routed at all, so every call — valid or invalid — is answered with the framework's unknown-operation error instead of the service's own validation, not-found or success response.

AWS (all five causes): recorded behavior only — these operations are absent from the checked-out Smithy model (smithy_query reports 'operation ListBulkOperations not found' / 'StartBulkPutItems not found'), so the recording is the only specification. The recordings show DynamoDB routing each operation and answering with the service's own error shapes and success payloads, which means the operation names ARE dispatched by the wire frontend.

Cause 1 of 5: DescribeBulkOperation with a malformed bulk-operation ARN.
AWS: HTTP 400, __type 'com.amazon.coral.validate#ValidationException', message 'Invalid Bulk operation ARN' (fixed text, no variable parts).
Emulator: HTTP 400 with __type 'com.amazon.coral.service#UnknownOperationException' and NO message member at all.
Why: the client cannot distinguish a bad ARN from an unsupported API; SDK error handling for ValidationException never fires.
Fix: route the operation in the service's frontend dispatch table and add its handler's ARN validation.
Traces: dynamodb-bulk-operation-management-refusals#task#1.

Cause 2 of 5: CancelBulkOperation with a malformed bulk-operation ARN.
AWS: HTTP 400, __type 'com.amazon.coral.validate#ValidationException', message 'Invalid Bulk operation ARN'.
Emulator: HTTP 400 __type 'com.amazon.coral.service#UnknownOperationException', message absent.
Why: same as cause 1 — a cancel request is indistinguishable from an unknown API.
Fix: same frontend dispatch plus the CancelBulkOperation handler's ARN validation.
Traces: dynamodb-bulk-operation-management-refusals#task#2.

Cause 3 of 5: ListBulkOperations with an out-of-range MaxResults.
AWS: HTTP 400, __type 'com.amazon.coral.validate#ValidationException', message "1 validation error detected: Value '<value>' at 'maxResults' failed to satisfy constraint: Member must have value greater than or equal to 1" — fixed text is the '1 validation error detected: Value ... at ... failed to satisfy constraint: Member must have value greater than or equal to 1' template; the variable parts are the rejected value and the member name, both taken from the request.
Emulator: HTTP 400 __type 'com.amazon.coral.service#UnknownOperationException', message absent.
Why: the constraint-violation message a client logs or matches on is never produced.
Fix: route the operation and apply the input's MaxResults range constraint in its validation.
Traces: dynamodb-bulk-operation-management-refusals#task#3 (cluster of 3 steps).

Cause 4 of 5: StartBulkPutItems naming a table that does not exist.
AWS: HTTP 400, __type 'com.amazonaws.dynamodb.v20120810#ResourceNotFoundException', message 'Requested resource not found: Table: <tableName> not found' — fixed text 'Requested resource not found: Table: ' + the request's TableName + ' not found'.
Emulator: HTTP 400 __type 'com.amazon.coral.service#UnknownOperationException', message absent.
Why: a client cannot tell a missing table from a missing API, so the standard not-found retry/creation path never runs.
Fix: route the operation and have its handler resolve TableName against table state before anything else.
Traces: dynamodb-bulk-operation-management-refusals#task#6 (cluster of 3 steps).

Cause 5 of 5: ListBulkOperations on a valid request returns no listing.
AWS: HTTP 200 with BulkOperationsSummaryList, each entry carrying BulkOperationArn, BulkOperationStatus and StartTime/EndTime for the account's bulk operations (the particular ARNs, statuses and timestamps are per-run values, not requirements; the member and its structure are).
Emulator: HTTP 400 with UnknownOperationException and no BulkOperationsSummaryList.
Why: bulk-operation progress cannot be observed at all, so no workflow built on this API can proceed.
Fix: route the operation and serve the listing from bulk-operation state.
Traces: dynamodb-bulk-operation-management-refusals#task#9.

## Evidence
Cause 1 — DescribeBulkOperation: expected 400 __type 'com.amazon.coral.validate#ValidationException', message 'Invalid Bulk operation ARN'; actual 400 __type 'com.amazon.coral.service#UnknownOperationException', message missing (task#1).
Cause 2 — CancelBulkOperation: identical expected/actual pair (task#2).
Cause 3 — ListBulkOperations: expected message "1 validation error detected: Value '0' at 'maxResults' failed to satisfy constraint: Member must have value greater than or equal to 1"; actual UnknownOperationException, no message (task#3, 3 steps).
Cause 4 — StartBulkPutItems: expected 400 'com.amazonaws.dynamodb.v20120810#ResourceNotFoundException', message 'Requested resource not found: Table: <tableName> not found'; actual UnknownOperationException (task#6, 3 steps).
Cause 5 — ListBulkOperations: expected 200 with BulkOperationsSummaryList of {BulkOperationArn, BulkOperationStatus, StartTime, EndTime}; actual 400, member missing (task#9).
All five from fixture dynamodb-bulk-operation-management-refusals.

## Steps to Reproduce
1. Replay fixture dynamodb-bulk-operation-management-refusals (steps task#1 through task#9) against the emulator.
2. POST to the DynamoDB endpoint with X-Amz-Target: DynamoDB_20120810.DescribeBulkOperation and body {"BulkOperationArn":"not-an-arn"}; observe 400 UnknownOperationException with no message instead of ValidationException 'Invalid Bulk operation ARN'.
3. Repeat with CancelBulkOperation (bad ARN), ListBulkOperations ({"MaxResults":0} and then {} ), and StartBulkPutItems naming an absent table.
