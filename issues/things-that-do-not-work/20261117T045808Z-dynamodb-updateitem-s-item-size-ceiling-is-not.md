Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb.update-item-request-shape-byte-charges-by-clause-kind
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb.update-item-request-shape-byte-charges-by-clause-kind|_code_and_message#16|AssertionError|raise AssertionError(f'expected a refusal, but the call succeeded: {result!r}')
Issue-Commit: a05566070b4435a7fb4fc76517a491d115525a96

# dynamodb: UpdateItem's item-size ceiling is not stable — a plain SET one byte past the measured boundary is accepted 200 instead of refused, so the refusal point shifts with the item's key

> _Note: this issue was identified against the code base at commit `a05566070b4435a7fb4fc76517a491d115525a96`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateItem

## Description
Cause 1 of 1: UpdateItem's over-size refusal is not applied at a stable boundary — the same plain `SET v = :v` clause whose operand is one byte past the boundary the emulator itself enforces is accepted on another item/table.

AWS: UpdateItem refuses an update whose resulting item plus the update clause's own request shape exceeds the 400 KB (409,600-byte) item limit, with HTTP 400 ValidationException and the fixed text `Item size to update has exceeded the maximum allowed size`, and the refused update leaves no trace on the item (Smithy: UpdateItem declares no dedicated size error — ConditionalCheckFailedException, ItemCollectionSizeLimitExceededException, ProvisionedThroughputExceededException, ResourceNotFoundException, TransactionConflictException, ThrottlingException, RequestLimitExceeded, ReplicatedWriteConflictException, InternalServerError, InvalidEndpointException — so the size refusal is the service-common ValidationException; UpdateItem.UpdateExpression and ExpressionAttributeValues carry no modelled size constraint, so the ceiling is documented behavior: https://docs.aws.amazon.com/amazondynamodb/latest/developerguide/ServiceQuotas.html#limits-items). The whole message is fixed text; it carries no values from the request. Recorded behavior shows the refusal point for a given clause kind is a property of that clause kind (and identical in us-west-2 and us-east-1): once the smallest refused operand length `hi` has been established for a plain `SET v = :v` on a fresh single-attribute item, an `UpdateExpression='SET v = :v'` with a `hi`-byte S operand is refused on ANOTHER fresh table and item of the same shape too — the recording bisects the boundary on one table and then re-asserts the refusal at `hi` on a second (control) table whose item was just written with only its hash key.

Emulator: the bisection and the per-clause-kind deltas now agree with AWS (path_operand 0, document_path 26, arithmetic 71, function 76, add 21, remove_delete 2, in both regions, re-verified at lo accepted / hi refused). But the very same plain `SET v = :v` with a `hi`-byte operand, replayed against a freshly created table on an item written with only its hash key, is ACCEPTED: HTTP 200 with an empty awsJson1_0 body (`content-length: 2`), no ValidationException at all. So the emulator's ceiling is computed from something that differs between the two items rather than from the resulting item plus the clause's request shape. Hypothesis: the emulator charges the key attribute's value bytes into the ceiling with 1-byte sensitivity, so the control item's 7-character hash-key value (`refused`) buys exactly the one byte of headroom the 8-character keys used during bisection did not have; AWS refuses both.

Why: a client cannot discover or rely on the item-size limit. The same write is refused for one key and accepted for another, so (a) oversized items AWS would never store are persisted, and (b) a client that probes/validates its own payload budget against one item gets a budget that is wrong for the next item. Because the refusal is silent in the accepted direction, the divergence surfaces later as items that real AWS cannot hold.

Fix: UpdateItem's size validation/accounting in the item write path — the over-limit check must derive the ceiling from the resulting item's bytes plus the per-clause request-shape charge consistently for every item and table, so the smallest refused operand length for a given clause kind does not move between items. Open issues 20261111T170613Z-dynamodb-item-size-batch-size-and-request-size.md (size ceilings not enforced from the stored item) and 20261115T010844Z-dynamodb-9-net-new-causes-updateitem-s-over.md (UpdateItem's over-size message and substitution-value accounting) touch the same validation but carry neither this failure's replay key nor this instability — this check's earlier failure mode (PutItem's wording on the refusal) is gone and the failure has moved to the positive control.

Traces: dynamodb.update-item-request-shape-byte-charges-by-clause-kind (check source line 179 — the positive-control refusal after the six clause families bisected and re-verified successfully).

## Evidence
Failing check: dynamodb.update-item-request-shape-byte-charges-by-clause-kind [mismatch].

All six clause families bisected to adjacent bytes and matched AWS's per-clause-kind shortfall from the plain-SET baseline in both us-west-2 and us-east-1 (path_operand 0, document_path 26, arithmetic 71, function 76, add 21, remove_delete 2), and each (lo accepted, hi refused) pair re-verified — so the emulator did produce `ValidationException: Item size to update has exceeded the maximum allowed size` at `hi` on the measurement tables.

Positive control, fresh table, item written as `{pk: {S: 'refused'}}` only, then:
  UpdateItem(TableName=<control table>, Key={pk:{S:'refused'}}, UpdateExpression='SET v = :v', ExpressionAttributeValues={':v': {'S': 'x' * hi}})
where `hi` is the smallest operand length the SAME emulator refused for the identical plain-SET clause during bisection.

Expected (AWS): HTTP 400 ValidationException, Message exactly `Item size to update has exceeded the maximum allowed size` (all fixed text), and a subsequent ConsistentRead GetItem showing no `v` attribute.
Actual (emulator): HTTP 200, empty body — `{'ResponseMetadata': {'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'application/x-amz-json-1.0', 'content-length': '2', ...}}}` — the check fails with `AssertionError: expected a refusal, but the call succeeded`. (Request id, date and host in that response are per-run values and not part of the requirement.)

## Steps to Reproduce
Replay dynamodb.update-item-request-shape-byte-charges-by-clause-kind against the emulator, or by hand:
1. CreateTable `t1` (hash key `pk` of type S, BillingMode PAY_PER_REQUEST); wait ACTIVE.
2. Bisect the plain-SET boundary on `t1`: for each candidate n, PutItem `{pk:{S:'00000001'}}` (any 8-character key), then UpdateItem with UpdateExpression `SET v = :v` and `:v = {'S': 'x'*n}`; record `lo` = largest n accepted and `hi` = lo+1 = smallest n refused with ValidationException `Item size to update has exceeded the maximum allowed size`.
3. CreateTable a second table `t2` with the same key schema; wait ACTIVE.
4. PutItem `{pk:{S:'refused'}}` into `t2` (a 7-character key value), then UpdateItem on that key with UpdateExpression `SET v = :v` and `:v = {'S': 'x'*hi}`.
5. Expected: the same ValidationException `Item size to update has exceeded the maximum allowed size`, and a ConsistentRead GetItem of that key shows no `v`. Observed: HTTP 200 with an empty body, and `v` is stored — the refusal boundary moved between the two items.
