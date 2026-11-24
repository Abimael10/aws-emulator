Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb.transact-get-items-aggregate-size-over-4mb-rejected
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb.transact-get-items-aggregate-size-over-4mb-rejected|run#14|AssertionError|assert (code, msg) == _TOO_LARGE, (
Issue-Commit: ae3639dd8257a8bd4005cbdd9f8198854274b8d4

# dynamodb: TransactGetItems refused for exceeding the 4 MB aggregate response size uses emulator-authored message text instead of AWS's "Payload in response is too large."

> _Note: this issue was identified against the code base at commit `ae3639dd8257a8bd4005cbdd9f8198854274b8d4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TransactGetItems

## Description
One net-new root cause in the DynamoDB emulator, grounded in a failing check the behavioral suite lists under '### dynamodb'. Related items that do NOT carry this replay key and do not cover it: open 20261120T093536Z-dynamodb-3-net-new-causes-the-transactitems-100.md and fixed 20261023T142648Z-dynamodb-transactgetitems-and-transactwriteitems-cap-validation-messages-diverge-from.md both concern the TransactItems *action-count* (@length max 100) constraint report, not the aggregate payload-size refusal; open 20261111T170613Z-dynamodb-non-constraint-validationexception-messages-are-wrapped-in.md and open 20261117T172826Z-dynamodb-expression-evaluation-10-net-new-causes-transaction.md concern other operations' refusal wording and transaction-internal runtime errors.

Cause 1 of 1: the aggregate-size refusal of TransactGetItems is reported with the request-side transaction-payload sentence (and the computed byte count) rather than AWS's response-side sentence. Failing check: dynamodb.transact-get-items-aggregate-size-over-4mb-rejected.

AWS: Smithy: TransactGetItems.TransactItems targets TransactGetItemList, @required, @length(min:1,max:100); the operation's modeled client errors are InvalidEndpointException, ProvisionedThroughputExceededException, RequestLimitExceeded, ResourceNotFoundException, ThrottlingException and TransactionCanceledException, plus the service-common ValidationException. Its documentation states "The aggregate size of the items in the transaction cannot exceed 4 MB" and lists "The aggregate size of the items in the transaction exceeded 4 MB" among the conditions on which "DynamoDB rejects the entire TransactGetItems request". Recorded behavior for that rejection: HTTP 400, Error.Code 'ValidationException', Error.Message exactly "Payload in response is too large." — the whole sentence is FIXED text, it has NO variable parts (no byte count, no item count, no table or key names are interpolated into it) — and the response carries NO CancellationReasons (it is a top-level validation refusal, not a cancellation). Nothing is read: none of the addressed items comes back, and every addressed item is still readable afterwards by GetItem. The same set of items one entry short (aggregate under 4 MB) must succeed and return every item whole.

Emulator: answers the correct status and error code (400 ValidationException) but with its own message text, "Transaction payload size cannot exceed 4MB. Payload Size: 4393092" — the request-payload wording, with the aggregate byte count of the run appended. Both the fixed sentence and the presence of a value-bearing suffix diverge; the number shown is just that run's aggregate (11 items of ~390 KB) and is not part of any requirement.

Why: TransactGetItems' 4 MB limit is on the aggregate RESPONSE, and AWS's wording says so, which is how a client distinguishes "my read set is too big to return" from a request-payload/limit problem on the write path. A client that keys on AWS's message — to decide whether to split the read set and retry in halves, or to surface a user-facing "result set too large" diagnostic — never matches the emulator's sentence, so the retry path is skipped and the refusal is reported as an unknown validation error. The appended payload size also makes the message run-dependent, so assertions and log fingerprints on it are unstable.

Fix: the TransactGetItems handler's aggregate-size guard in the DynamoDB transactional read path must raise the ValidationException whose message is the fixed sentence "Payload in response is too large." with nothing interpolated, instead of reusing the transaction request-payload size message; the write path's own payload-size refusal must keep its own wording. The response must remain a top-level ValidationException with no CancellationReasons member, and no item may be returned.

Traces: dynamodb.transact-get-items-aggregate-size-over-4mb-rejected (the under-4 MB arm with 10 items returned all 10 correctly and the post-refusal GetItem of the survivor passed; the failure is on the over-4 MB arm's message assertion).

## Evidence
Check dynamodb.transact-get-items-aggregate-size-over-4mb-rejected, over-limit arm — TransactGetItems with 11 Gets whose items total 4,393,092 bytes (each item ~390 KB, each individually under 400 KB), one table, PAY_PER_REQUEST:
  expected: HTTP 400, Error.Code 'ValidationException', Error.Message "Payload in response is too large." (entire sentence fixed text; no variable parts), no CancellationReasons in the response, no items returned
  actual:   HTTP 400, Error.Code 'ValidationException', Error.Message "Transaction payload size cannot exceed 4MB. Payload Size: 4393092"
  replay line: AssertionError: a 4393092-byte TransactGetItems gave 'ValidationException': 'Transaction payload size cannot exceed 4MB. Payload Size: 4393092'
       (check_dynamodb_transact_get_items_aggregate_size_over_4mb_rejected.py line 117, `assert (code, msg) == _TOO_LARGE`, where _TOO_LARGE = ("ValidationException", "Payload in response is too large."))
The status and the error code already agree with AWS; only the message text diverges. '4393092' is this run's aggregate size and belongs to neither the AWS message nor the requirement — AWS interpolates no value here at all.
Preceding arm that passed: the same items minus one (10 Gets, aggregate under 4 MB) returned all 10 items whole, so the limit boundary itself and the read path are correct.

## Steps to Reproduce
1. Build and boot the emulator and replay the behavioral suite (run_behavioral_suite), then re-run dynamodb.transact-get-items-aggregate-size-over-4mb-rejected.
2. CreateTable with KeySchema [{pk, HASH}], AttributeDefinitions [{pk, S}], BillingMode PAY_PER_REQUEST; wait until TableStatus is ACTIVE.
3. PutItem 11 items of the form {"pk": {"S": "big-NN"}, "data": {"S": "x" * 399360}} (each ~390 KB, individually under the 400 KB item limit).
4. Call TransactGetItems with 10 of those keys (aggregate < 4 MB): it must answer 200 and return all 10 items whole.
5. Call TransactGetItems with all 11 keys (aggregate > 4 MB): it must be refused HTTP 400 ValidationException with Error.Message exactly "Payload in response is too large." and the response must carry no CancellationReasons. Observe instead the message "Transaction payload size cannot exceed 4MB. Payload Size: <aggregate bytes>".
6. GetItem (ConsistentRead) on any of the 11 keys must still return that item unchanged.
