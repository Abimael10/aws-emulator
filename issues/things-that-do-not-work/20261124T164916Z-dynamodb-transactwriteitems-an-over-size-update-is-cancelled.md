Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb.transact-write-items-update-grows-item-over-size-limit dynamodb.transact-write-items-update-over-size-limit-new-and-existing-item-both-regions dynamodb.transact-write-items-update-shares-update-items-floor-bound-ceiling-at-a-short-key
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb.transact-write-items-update-grows-item-over-size-limit|run#14|AssertionError|assert code == ERR_VALIDATION, (
Issue-Key: dynamodb|TransactWriteItems|TransactionCanceledException
Issue-Key: dynamodb.transact-write-items-update-shares-update-items-floor-bound-ceiling-at-a-short-key|expect_error#31|AssertionError|assert got_code == code, f"expected {code!r}, got {got_code!r} ({got_message!r})"
Issue-Commit: 9d6dbefbeeb79c5752051e10ea3d5ba278a8ab35

# dynamodb TransactWriteItems: an over-size Update is cancelled (TransactionCanceledException) instead of refused ValidationException "Item size to update has exceeded the maximum allowed size"

> _Note: this issue was identified against the code base at commit `9d6dbefbeeb79c5752051e10ea3d5ba278a8ab35`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TransactWriteItems

## Description
REOPENS 20261120T124018Z-dynamodb-6-net-new-causes-transactwriteitems-cancellation-reason.md: its fix left the call refused.

That issue's Cause 3 covered exactly these three checks and its Resolution describes the reconciliation it implemented (an item an Update ASKS for — the key it addresses plus what its clauses write — that cannot fit in an item at all refuses the CALL, while a result that only crosses the ceiling over bytes the stored item ALREADY held cancels the transaction). On replay all three checks still get the cancellation envelope, so the refusing branch is not reached for the payload-only case.

Cause 1 of 1: a TransactWriteItems Update whose own payload cannot fit in an item is answered with TransactionCanceledException instead of a top-level ValidationException.
Failing checks: dynamodb.transact-write-items-update-grows-item-over-size-limit, dynamodb.transact-write-items-update-over-size-limit-new-and-existing-item-both-regions, dynamodb.transact-write-items-update-shares-update-items-floor-bound-ceiling-at-a-short-key.

AWS:      recorded behavior for the status/code/message — an Update member whose target item, as the statement asks for it, exceeds the item-size ceiling is refused at the request level: HTTP 400, Error.Code ValidationException, Error.Message "Item size to update has exceeded the maximum allowed size" (the whole sentence is fixed text; it carries no value from the request), never TransactionCanceledException, and the stored item is left exactly as it was. This holds for an existing target item, for a key that never existed (no item is created), in us-west-2 and us-east-1 alike, and at the floor-bound ceiling a short partition key shares with a standalone UpdateItem (for a 1-byte key the accepted ceiling is 20 bytes below the flat 409600-byte cap, and the statement that targets the flat cap is refused with this same code and message by UpdateItem and by TransactWriteItems' Update identically). Smithy: TransactWriteItems models TransactionCanceledException (errorKind client, httpError 400) and ValidationException is the service-common refusal class; the model does not pick between them for this case, so the error class above is recorded behavior.

Emulator: every such request is answered HTTP 400 TransactionCanceledException with Message "Transaction cancelled, please refer cancellation reasons for specific reasons [ValidationError]" — the size defect is reported as a per-slot cancellation (the bracketed list is the per-slot code list derived from the request) rather than as a request-level refusal, so both the error code and the message diverge. The positive controls in all three checks pass (an in-bounds Update round-trips, and the short-key ceiling value is stored and read back whole), which isolates this to the over-ceiling branch.

Why:      a client cannot distinguish a request it must shrink from a transaction it may retry. TransactionCanceledException is the retry/refresh class — an SDK transaction helper unpacks CancellationReasons and re-submits — so an unshrinkable write is retried forever, while the ValidationException branch that would tell the caller to shrink the item is never entered. Code that asserts on the message also never sees AWS's sentence.

Fix:      the projected-item-size check in the TransactWriteItems Update path (the `crossed_the_item_ceiling` seam the prior fix names in services/service-com-amazonaws-dynamodb-dynamodb-20120810/src/ops/transact_write_items.rs) — the size the statement ASKS for (the addressed key plus the bytes its clauses write), including the floor-bound ceiling a short key gets, must be validated during request validation, before the transaction is staged, and raise the request-level ValidationException; only growth that crosses the ceiling over bytes the stored item already held may cancel.

Also note the open wire-corpus issues that pull the other way and do not carry these replay keys: 20261121T173852Z-dynamodb-transactwriteitems-an-over-size-update-is-refused.md and 20261122T202407Z-dynamodb-transaction-failures-take-the-wrong-error-class.md. The two surfaces must be reconciled over the bytes as described above; flipping a global switch closes one direction and reopens the other.

Traces:   the three checks above.

## Evidence
Verbatim from the replay (all three checks, same divergence):

- dynamodb.transact-write-items-update-grows-item-over-size-limit [mismatch]
  AssertionError: a TransactWriteItems Update growing an item past 409600 bytes must be ValidationException, not TransactionCanceledException, got 'TransactionCanceledException': 'Transaction cancelled, please refer cancellation reasons for specific reasons [ValidationError]'
  (existing item seeded at ~1 KB; one Update SETs a value making the asked-for item exactly 409601 bytes.)

- dynamodb.transact-write-items-update-over-size-limit-new-and-existing-item-both-regions [mismatch]
  AssertionError: expected 'ValidationException', got 'TransactionCanceledException': 'Transaction cancelled, please refer cancellation reasons for specific reasons [ValidationError]'
  (region-pinned: the same claim in us-west-2 and us-east-1, for an existing key and for a key that never existed.)

- dynamodb.transact-write-items-update-shares-update-items-floor-bound-ceiling-at-a-short-key [mismatch]
  AssertionError: expected 'ValidationException', got 'TransactionCanceledException' ('Transaction cancelled, please refer cancellation reasons for specific reasons [ValidationError]')
  (1-byte partition key: the value targeting the flat 409600-byte cap is 20 bytes past the ceiling; a standalone UpdateItem refuses it with ValidationException / 'Item size to update has exceeded the maximum allowed size' and TransactWriteItems' Update must match it. The same check's accepted leg — a value at ceiling 409600-20 round-tripping whole through TransactWriteItems Update — passes.)

Expected in every case: HTTP 400, Error.Code 'ValidationException', Error.Message 'Item size to update has exceeded the maximum allowed size' (fixed text, no variable part), and the stored item unchanged / the absent key not created.
Actual: HTTP 400, Error.Code 'TransactionCanceledException', Error.Message 'Transaction cancelled, please refer cancellation reasons for specific reasons [ValidationError]' (the bracketed per-slot code list is derived from the request).

The 409600 figure, the 22-byte floor and the 20-byte offset are the limit rules; the table names and key values are per-run values.

## Steps to Reproduce
1. CreateTable(pk HASH S, PAY_PER_REQUEST); wait ACTIVE.
2. PutItem {pk:'grow', data:<1 KB>}. TransactWriteItems with one Update on {pk:'grow'}, UpdateExpression 'SET #d = :v', :v a string sized so the asked-for item is 409601 bytes. Expect 400 ValidationException 'Item size to update has exceeded the maximum allowed size'; observe TransactionCanceledException '...[ValidationError]'. GetItem(ConsistentRead) must still return the original 1 KB value.
3. Repeat the same Update against a key that was never written: same refusal, and GetItem must find no item.
4. Repeat steps 2-3 against us-west-2 and us-east-1.
5. Short-key ceiling: with a 1-byte partition key ('k' = '3'), PutItem {k:'3', v:'start'}; a standalone UpdateItem whose value makes the item total 409600 bytes is refused ValidationException 'Item size to update has exceeded the maximum allowed size', and the identical statement through TransactWriteItems Update must be refused the same way, leaving v='start'. The accepted control is the same shape at total 409580 (409600 minus the 22-byte floor plus the 2 preexisting key bytes), which must succeed through both APIs.
6. Re-replay the three check ids in trace_ids.
