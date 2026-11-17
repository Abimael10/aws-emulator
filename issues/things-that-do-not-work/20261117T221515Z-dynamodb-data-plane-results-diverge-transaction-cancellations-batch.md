Issue-Source: trace-sweep-divergence
Issue-Trace: ddb-txn-add-nested-set-validation
Issue-Step: task#8
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ddb-txn-add-nested-set-validation ddb-txn-update-validation-interleaves-ccf dynamodb-transact-write-items-static-validation-precedes-cancellation dynamodb-transact-write-items-update-runtime-validation-cancellation-reasons transact-write-items-update-over-size-limit-cancellation-reason twi-update-operand-type-error-orders-with-condition-check-failure twi-update-set-document-path-and-missing-attribute-cancellation-reasons ddbval-transact-write-items-nesting-depth-second-limit-edge-rejected ddbval-transact-write-items-placeholder-length-boundary ddbval-transact-write-items-update-condition-value-nesting-depth-rejected dynamodb-batch-execute-statement-select-no-where-and-keyed-attribute-exists dynamodb-batch-get-item-key-and-table-option-serialization-type-mismatches-rejected dynamodb-execute-statement-update-from-first-clause-order-and-absent-table-rejected dynamodb-scan-unfiltered-number-hash-key-returns-stable-order dynamodb-scan-unfiltered-string-hash-key-returns-stable-order dynamodb-transact-write-items-cancellation-omits-capacity-and-collection-metrics
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb|TransactWriteItems|4xx->4xx|$.CancellationReasons
Issue-Key: dynamodb|TransactWriteItems|4xx->4xx|$.Message
Issue-Key: dynamodb|TransactWriteItems|4xx->4xx|$
Issue-Key: dynamodb|BatchExecuteStatement|2xx->2xx|$.Responses[*].Error
Issue-Key: dynamodb|BatchGetItem|2xx->4xx|$.Responses
Issue-Key: dynamodb|BatchGetItem|2xx->4xx|$.UnprocessedKeys
Issue-Key: dynamodb|BatchGetItem|2xx->4xx|status
Issue-Key: dynamodb|GetItem|2xx->2xx|$.Item.status.S
Issue-Key: dynamodb|Scan|2xx->2xx|$.Items[*].pk.N
Issue-Key: dynamodb|Scan|2xx->2xx|$.Items[*].pk.S
Issue-Key: dynamodb|TransactWriteItems|2xx->2xx|$.ConsumedCapacity[*].CapacityUnits
Issue-Key: dynamodb|TransactWriteItems|2xx->2xx|$.ConsumedCapacity[*].LocalSecondaryIndexes.alt-index.CapacityUnits
Issue-Key: dynamodb|TransactWriteItems|2xx->2xx|$.ConsumedCapacity[*].LocalSecondaryIndexes.alt-index.WriteCapacityUnits
Issue-Key: dynamodb|TransactWriteItems|2xx->2xx|$.ConsumedCapacity[*].WriteCapacityUnits
Issue-Related: 20261111T221143Z-dynamodb-condition-key-and-filter-expression-defects-are.md
Issue-Commit: 656cbf9b11e99225d4fd85cb8b7394b019211c7b

# DynamoDB data-plane results diverge: transaction cancellations, batch and PartiQL errors, scan order and consumed capacity (8 causes)

> _Note: this issue was identified against the code base at commit `656cbf9b11e99225d4fd85cb8b7394b019211c7b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TransactWriteItems

## Description
Eight net-new causes on dynamodb's data plane: a transaction that fails inside an item action is reported as a flat validation error instead of a cancellation, a Coral error type is emitted unqualified, BatchExecuteStatement drops a per-statement error, BatchGetItem refuses a request AWS serves, a PartiQL UPDATE never lands, unfiltered Scan returns items in sorted rather than hash order, and TransactWriteItems over-counts consumed capacity. Related but NOT covering: open issues 20261117T172826Z-dynamodb-expression-evaluation-10-net-new-causes-transaction.md and 20261117T172826Z-dynamodb-data-plane-refusal-wording-and-partiql-8.md carry none of these replay keys.

Cause 1 of 8: a runtime error inside a TransactWriteItems action is answered as ValidationException instead of TransactionCanceledException.
AWS: Smithy: TransactWriteItems declares TransactionCanceledException (client, HTTP 400) with members Message and CancellationReasons (list of CancellationReason{Code, Message, Item}); its documentation states DynamoDB cancels the request when 'there is a user error, such as an invalid data format' and that the reason list is ordered according to the TransactItems ordering, with a Null code/message entry for an item that did not fail. So a mix of a per-item operand-type error and a failed condition yields HTTP 400 with __type 'com.amazonaws.dynamodb.v20120810#TransactionCanceledException', CancellationReasons = [{Code: 'None'}, {Code: 'ValidationError', Message: 'An operand in the update expression has an incorrect data type'}, {Code: 'ConditionalCheckFailed', Message: 'The conditional request failed'}] and Message 'Transaction cancelled, please refer cancellation reasons for specific reasons [<code>, <code>, ...]' — fixed text 'Transaction cancelled, please refer cancellation reasons for specific reasons [' + the per-item codes in request order + ']'.
Emulator: HTTP 400 with __type 'ValidationException' and neither CancellationReasons nor Message.
Why: a client cannot learn which action failed or why — the per-item reason list is the only way to attribute a transaction failure — and it sees a non-retryable shape-validation class where AWS reports a cancellation.
Fix: the TransactWriteItems handler must collect per-action runtime outcomes and raise TransactionCanceledException with the ordered reason list, rather than surfacing the first action's error directly.
Traces: ddb-txn-add-nested-set-validation#task#8 (cluster of 20 steps across 7 transaction fixtures).

Cause 2 of 8: the nesting-depth refusal emits an unqualified __type.
AWS: the error type on the wire is the fully qualified Coral shape id 'com.amazon.coral.validate#ValidationException'; the message is 'Nesting Levels have exceeded supported limits: Attributes in the item have nested levels beyond supported limit' (fixed text, no variable parts).
Emulator: the message matches, but __type is the bare 'ValidationException'.
Why: SDKs and clients that map the wire __type to an error class by its qualified id fail to classify the error, so it surfaces as an unknown/opaque failure.
Fix: the error serializer for the framework validation class must emit the qualified shape id.
Traces: ddbval-transact-write-items-nesting-depth-second-limit-edge-rejected#task#4 (cluster of 6 steps).

Cause 3 of 8: BatchExecuteStatement omits the per-statement Error for a SELECT with no primary key in its WHERE clause.
AWS: Smithy: BatchExecuteStatement returns Responses (PartiQLBatchResponse), one entry per statement, and a failing statement's entry carries Error{Code, Message} while the call itself stays HTTP 200. For a SELECT that does not constrain the primary key the entry is {Code: 'ValidationError', Message: 'Select statements within BatchExecuteStatement must specify the primary key in the where clause.'} (fixed text, terminating period included).
Emulator: HTTP 200 but the Error member of that response entry is absent.
Why: a per-statement failure is indistinguishable from a statement that matched nothing, so a batch silently loses results.
Fix: the BatchExecuteStatement handler's per-statement validation and its Responses serializer.
Traces: dynamodb-batch-execute-statement-select-no-where-and-keyed-attribute-exists#task#4.

Cause 4 of 8: BatchGetItem refuses a request AWS answers successfully.
AWS: Smithy: BatchGetItem returns Responses (map of table name to item list) and UnprocessedKeys; the recorded well-formed request answers HTTP 200 with the requested item under its table name and UnprocessedKeys = {} (an empty map, not an omitted member).
Emulator: HTTP 400 with neither Responses nor UnprocessedKeys.
Why: a legal batch read fails outright, and the empty-UnprocessedKeys contract clients loop on is never served.
Fix: the BatchGetItem handler's request validation (it must not reject the well-formed RequestItems/table-option combination) and its serializer's empty-map emission.
Traces: dynamodb-batch-get-item-key-and-table-option-serialization-type-mismatches-rejected#task#6.

Cause 5 of 8: a PartiQL UPDATE accepted by ExecuteStatement does not change the item.
AWS: the UPDATE statement is applied, so the following GetItem shows the attribute at its new value.
Emulator: the attribute still holds its previous value — the statement was reported as accepted but never took effect.
Why: a write acknowledged to the client is lost; read-after-write through PartiQL is unreliable.
Fix: the ExecuteStatement UPDATE execution path (statement clause handling) and the item state it must commit.
Traces: dynamodb-execute-statement-update-from-first-clause-order-and-absent-table-rejected#task#16.

Cause 6 of 8: an unfiltered Scan over a numeric hash key returns items in ascending key order instead of DynamoDB's hash-distribution order.
AWS: Smithy: Scan returns Items with no modeled ordering, but DynamoDB's physical order for an unfiltered scan is the key's internal hash distribution, which is stable for a given set of key values and is NOT the sorted order of the key (the recording shows a fixed, non-monotonic ordering of the same ten numeric keys).
Emulator: returns the items sorted ascending by the numeric key value.
Why: a client that pages an unfiltered scan, or that asserts on the stable order DynamoDB gives for a fixed key set, gets a different sequence — and the ordering the emulator invents is one AWS never produces, so pagination boundaries land on different items.
Fix: the Scan read path's item ordering (hash-order placement in the table's item index), not the serializer.
Traces: dynamodb-scan-unfiltered-number-hash-key-returns-stable-order#task#4 (cluster of 2 steps).

Cause 7 of 8: the same divergence for a string hash key.
AWS: the unfiltered Scan order over a fixed set of string keys is the stable hash-distribution order (the recording shows a specific non-lexical sequence).
Emulator: returns the keys in lexicographic order.
Why: same as cause 6.
Fix: same ordering component.
Traces: dynamodb-scan-unfiltered-string-hash-key-returns-stable-order#task#4 (cluster of 2 steps).

Cause 8 of 8: TransactWriteItems over-counts ConsumedCapacity, including per-LSI capacity.
AWS: Smithy: TransactWriteItems returns ConsumedCapacity (ConsumedCapacityMultiple) with CapacityUnits/WriteCapacityUnits and per-LocalSecondaryIndexes entries; the units reported correspond to the writes the transaction actually performed in the request's 1 KB write units (the recording reports 3.0 table units and 1.0 for the local secondary index).
Emulator: reports one extra write unit at table level (4.0) and twice the index capacity (2.0), i.e. its accounting charges an action or index write the transaction did not perform.
Why: callers size provisioned capacity and throttling budgets from these numbers, so a systematically inflated count mis-sizes the table and makes capacity assertions fail.
Fix: the write-capacity accounting in the TransactWriteItems handler (table and per-LSI tallies).
Traces: dynamodb-transact-write-items-cancellation-omits-capacity-and-collection-metrics#task#7.

## Evidence
Cause 1 — expected 400 __type 'com.amazonaws.dynamodb.v20120810#TransactionCanceledException' with CancellationReasons [{'Code':'None'},{'Code':'ValidationError','Message':'An operand in the update expression has an incorrect data type'},{'Code':'ConditionalCheckFailed','Message':'The conditional request failed'}] and Message 'Transaction cancelled, please refer cancellation reasons for specific reasons [None, ValidationError, ConditionalCheckFailed]'; actual 400 __type 'ValidationException' with both members missing (20 steps).
Cause 2 — expected __type 'com.amazon.coral.validate#ValidationException'; actual 'ValidationException' (message identical) (6 steps).
Cause 3 — expected Responses[1].Error = {'Code':'ValidationError','Message':'Select statements within BatchExecuteStatement must specify the primary key in the where clause.'}; actual missing.
Cause 4 — expected 200 with Responses={<table>:[item]} and UnprocessedKeys={}; actual 400 with both missing.
Cause 5 — expected Item.status.S='confirmed' after the PartiQL UPDATE; actual 'active'.
Cause 6 — expected Items ordered 8,…,2,255,150,17,500,61,42,99 (hash order of the recorded key set); actual ascending 2,…,17,42,61,99,150,255,500 (2 steps).
Cause 7 — expected neptune,pluto,earth,…,ceres,saturn; actual lexicographic ceres,earth,jupiter,…,venus (2 steps).
Cause 8 — expected ConsumedCapacity[0].CapacityUnits/WriteCapacityUnits=3.0 and LocalSecondaryIndexes.<index>.CapacityUnits/WriteCapacityUnits=1.0; actual 4.0 and 2.0.

## Steps to Reproduce
1. Replay fixtures ddb-txn-add-nested-set-validation (task#8), ddbval-transact-write-items-nesting-depth-second-limit-edge-rejected (task#4), dynamodb-batch-execute-statement-select-no-where-and-keyed-attribute-exists (task#4), dynamodb-batch-get-item-key-and-table-option-serialization-type-mismatches-rejected (task#6), dynamodb-execute-statement-update-from-first-clause-order-and-absent-table-rejected (task#16), dynamodb-scan-unfiltered-number-hash-key-returns-stable-order (task#4), dynamodb-scan-unfiltered-string-hash-key-returns-stable-order (task#4) and dynamodb-transact-write-items-cancellation-omits-capacity-and-collection-metrics (task#7).
2. Cause 1: TransactWriteItems with three actions — one that succeeds, one Update whose SET operand has the wrong data type, one with a condition that fails — and inspect __type/CancellationReasons/Message.
3. Cause 2: TransactWriteItems with an item nested beyond the supported depth and inspect __type.
4. Cause 3: BatchExecuteStatement with a SELECT that omits the primary key in WHERE; inspect Responses[n].Error.
5. Cause 4: the well-formed BatchGetItem at task#6 of the serialization fixture.
6. Cause 5: ExecuteStatement with an UPDATE … SET … WHERE on a seeded item, then GetItem.
7. Causes 6-7: seed ten items with distinct numeric (then string) hash keys and Scan with no filter; compare Items order.
8. Cause 8: TransactWriteItems with ReturnConsumedCapacity=INDEXES on a table with an LSI.
