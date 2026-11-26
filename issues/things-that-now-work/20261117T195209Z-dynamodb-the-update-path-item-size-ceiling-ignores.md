Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb.transact-write-items-update-shares-update-items-floor-bound-ceiling-at-a-short-key
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb.transact-write-items-update-shares-update-items-floor-bound-ceiling-at-a-short-key|_error#16|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Commit: f80a150e2fdc5318306fa43cfca9c63b6933d812

# dynamodb: the update-path item-size ceiling ignores the short-key floor — a statement 20 bytes past the floor-bound ceiling is accepted 200 instead of refused with ValidationException "Item size to update has exceeded the maximum allowed size"

> _Note: this issue was identified against the code base at commit `f80a150e2fdc5318306fa43cfca9c63b6933d812`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TransactWriteItems

## Description
Cause 1 of 1: an update whose resulting item lands above the floor-bound item-size ceiling — but at or below the flat 409,600-byte item cap — is accepted with HTTP 200 instead of being refused, on both update paths the check exercises (standalone UpdateItem and TransactWriteItems' Update action). Failing check: dynamodb.transact-write-items-update-shares-update-items-floor-bound-ceiling-at-a-short-key.

AWS: an update is refused when the resulting item plus the update clause's own request-shape charge exceeds the item-size limit, with HTTP 400 ValidationException and the message `Item size to update has exceeded the maximum allowed size` — the whole message is fixed text and carries no values from the request or state (Smithy: UpdateItem's modelled errors are ConditionalCheckFailedException, ItemCollectionSizeLimitExceededException, ProvisionedThroughputExceededException, ReplicatedWriteConflictException, ResourceNotFoundException, TransactionConflictException, ThrottlingException, RequestLimitExceeded, InvalidEndpointException, InternalServerError — none of them is a size error, so the size refusal is the service-common ValidationException; UpdateItem.UpdateExpression and ExpressionAttributeValues carry no modelled size constraint, so the ceiling is documented behavior: https://docs.aws.amazon.com/amazondynamodb/latest/developerguide/ServiceQuotas.html#limits-items). The accounting is floor-bound, not flat: the key portion is charged at a minimum floor (recorded behavior shows a 22-byte floor), so for an item whose key bytes are below that floor the acceptable total tops out BELOW the flat 409,600-byte cap — for a single 1-byte hash key (name `k`, value one character, 2 key bytes) the recording accepts a total of 409,600 − 20 bytes and refuses 409,600 bytes. TransactWriteItems' Update action is bound by exactly the same ceiling as a standalone UpdateItem, and the over-ceiling statement makes the TransactWriteItems CALL fail with that same HTTP 400 ValidationException code and message — not a TransactionCanceledException wrapping a per-item cancellation reason (Smithy: TransactWriteItems models TransactionCanceledException, TransactionInProgressException, IdempotentParameterMismatchException, ProvisionedThroughputExceededException, RequestLimitExceeded, ResourceNotFoundException, ThrottlingException, InvalidEndpointException, InternalServerError — again no size error, so the refusal is the common ValidationException). A refused update must leave the item at its previous value.

Emulator: with a 1-byte hash key the emulator accepts the total the recording refuses. The 409,600-byte-total update on a 1-byte key (20 bytes past the floor-bound ceiling) returns HTTP 200 with an empty awsJson1_0 body (`content-type: application/x-amz-json-1.0`, `content-length: 2`) and no error at all, so the check's `expect_error` fails with `AssertionError: expected the call to be refused, but it succeeded`. The emulator's positive control at the floor-bound ceiling (409,600 − 20) does pass, so the ceiling is being computed as the flat 409,600-byte cap with the key's actual bytes rather than the floor-bound value; the oversized item is persisted. Open issue 20261117T045808Z-dynamodb-updateitem-s-item-size-ceiling-is-not.md reports the same ceiling as key-length sensitive on UpdateItem and 20261111T170613Z-dynamodb-item-size-batch-size-and-request-size.md reports size ceilings not enforced from the stored item, but neither carries this check's replay key nor the short-key floor rule, and neither covers TransactWriteItems' Update sharing UpdateItem's ceiling.

Why: a client cannot discover or rely on the item-size limit for update writes. Items real AWS would never store are persisted (so the same workload fails on AWS at write time, or fails later when the item is read/re-updated), and a client that sizes its payload budget against a short-keyed item gets a budget 20 bytes too large. On the transaction path the divergence is worse: a client that expects the whole TransactWriteItems call to be refused for a shape error instead sees a committed transaction, so the sibling actions in that transaction land too.

Fix: DynamoDB's item-size validation in the item write path — the over-limit check must derive the ceiling from the resulting item's bytes with the key portion charged at its minimum floor (so a short key yields a ceiling below the flat 409,600-byte cap), and TransactWriteItems' Update action must run the identical check and surface it as a top-level HTTP 400 ValidationException with the fixed message, leaving the item unchanged.

Traces: dynamodb.transact-write-items-update-shares-update-items-floor-bound-ceiling-at-a-short-key (the check's `expect_error` on an update whose total is the flat 409,600 bytes on a 1-byte key, after the floor-bound-ceiling positive control succeeded).

## Evidence
Failing check: dynamodb.transact-write-items-update-shares-update-items-floor-bound-ceiling-at-a-short-key [mismatch].

Setup: CreateTable with a single hash key `k` of type S, BillingMode PAY_PER_REQUEST; items keyed by one-character values, so the key contributes 2 bytes (`k` + 1 value byte) — far below the 22-byte floor.

Step that diverges — an update whose resulting item totals the flat cap (409,600 bytes of attribute-name + value bytes), i.e. 20 bytes past the floor-bound ceiling:
  UpdateExpression `SET v = :v`, `:v = {'S': 'x' * (409600 - item_bytes(key))}`, issued as a standalone UpdateItem and as TransactWriteItems(TransactItems=[{Update: {...same key/expression...}}]).

Expected (recorded AWS): HTTP 400, error code `ValidationException`, Message exactly `Item size to update has exceeded the maximum allowed size` (entirely fixed text — no request or state values interpolated), and a subsequent ConsistentRead GetItem still showing the seeded value `start`. For the TransactWriteItems form the CALL fails with that same code/message, not TransactionCanceledException.

Actual (emulator): HTTP 200 with an empty body — `{'ResponseMetadata': {'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'application/x-amz-json-1.0', 'content-length': '2', ...}}}` — and the oversized value is stored. The check aborts with `AssertionError: expected the call to be refused, but it succeeded: {...'HTTPStatusCode': 200...}` raised from its `_error`/`expect_error` helper (check source lines 23 and 27). The request id, date and host in that response are per-run values and are not part of the requirement.

Positive control that PASSED, pinning the boundary: the same `SET v = :v` at a total of 409,600 − 20 bytes on a 1-byte key is accepted by the emulator and round-trips whole, which is exactly AWS's floor-bound ceiling — so the emulator accepts both the ceiling and 20 bytes beyond it.

## Steps to Reproduce
Replay dynamodb.transact-write-items-update-shares-update-items-floor-bound-ceiling-at-a-short-key against the emulator, or by hand:
1. CreateTable `t` with KeySchema [{k, HASH}], AttributeDefinitions [{k, S}], BillingMode PAY_PER_REQUEST; wait until ACTIVE.
2. PutItem {k: {S: '1'}, v: {S: 'start'}}.
3. UpdateItem(TableName=t, Key={k:{S:'1'}}, UpdateExpression='SET v = :v', ExpressionAttributeValues={':v': {'S': 'x' * (409600 - 2 - 1)}}) so the resulting item's attribute-name + value bytes total exactly 409,600 (`k` 1 + `1` 1 + `v` 1 + operand).
   Expected: HTTP 400 ValidationException, Message `Item size to update has exceeded the maximum allowed size`; a ConsistentRead GetItem still returns v = 'start'.
   Observed: HTTP 200 with an empty body; the oversized value is stored.
4. PutItem {k: {S: '3'}, v: {S: 'start'}} and issue the same statement through TransactWriteItems(TransactItems=[{'Update': {TableName: t, Key: {k:{S:'3'}}, UpdateExpression: 'SET v = :v', ExpressionAttributeValues: {':v': {'S': 'x' * (409600 - 2 - 1)}}}}]).
   Expected: the call itself fails with HTTP 400 ValidationException and the same fixed message (not TransactionCanceledException), and the item stays at 'start'.
   Observed: HTTP 200, empty body, transaction committed.
5. Control (already passing, do not change it): the same update at a total of 409,600 − 20 bytes must be ACCEPTED on both paths and round-trip whole.
