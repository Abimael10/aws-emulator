Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync__lambda.appsync-batch-invoke-partial-item-error-isolates-the-failing-sibling
Issue-Key: appsync__lambda.appsync-batch-invoke-partial-item-error-isolates-the-failing-sibling|run#32|AssertionError|assert post.get("detail") == {"forId": expected_id}, (
Issue-Commit: 6ad539f79015bc27d2d0777d4c5c145ae7d45c50

# AppSync Lambda BatchInvoke: one element's per-item errorMessage fails its sibling elements too — the siblings resolve to null instead of their own data

> _Note: this issue was identified against the code base at commit `6ad539f79015bc27d2d0777d4c5c145ae7d45c50`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
BatchInvoke

## Description
Cause 1 of 1: a per-element error in a BatchInvoke response is not isolated to that element.

AWS: with a Lambda data source in BatchInvoke mode, the resolver's response is an array with one element per field being resolved, and each element may use the documented per-element envelope {"data": ..., "errorMessage": ..., "errorType": ...} (AWS AppSync developer guide, 'Tutorial: Lambda resolvers' -> 'Returning individual errors'). When exactly ONE element carries a non-null errorMessage/errorType, AppSync raises exactly one GraphQL error, for that element's own field, and every OTHER element of the same invocation still resolves its own "data" normally. HTTP status is 200 and the error travels in the GraphQL response's "errors" array; the surviving siblings' "data" is present. This is recorded/ documented behavior only — the AppSync Smithy model does not describe resolver response mapping.
Emulator: when one element of the batch carries an error envelope, the sibling element's field resolves to null — the check reads post[0].detail as None where the batch element's data was {"forId": <that element's id>}. One item's failure therefore fails the whole batch's data.
Why: a client using the documented per-item error envelope (the standard way to report a single bad record in a batched resolver) loses all the good data in the batch. A partially-failing list query returns all-null rows instead of the N-1 rows AWS returns alongside one field error, so error handling that AWS makes granular becomes all-or-nothing.
Fix: the AppSync resolver response mapping for Lambda data sources in BatchInvoke mode — split the returned array element-by-element, attach each element's errorMessage/errorType as a GraphQL error on that element's own field path, and propagate every other element's "data" to its field unchanged.
Traces: appsync__lambda.appsync-batch-invoke-partial-item-error-isolates-the-failing-sibling.

## Evidence
Behavioral suite replay against the emulator built from this checkout; service appsync__lambda 8/10 (80%).

[mismatch] appsync__lambda.appsync-batch-invoke-partial-item-error-isolates-the-failing-sibling:
  File check_appsync__lambda_appsync_batch_invoke_partial_item_error_isolates_the_failing_sibling.py, line 423, in run
  assert post.get("detail") == {"forId": expected_id}
  AssertionError: sibling post[0] must still resolve its data despite the OTHER item's batch error, got None

Expected: HTTP 200 with exactly one GraphQL error naming the failing element's field, and the sibling element's field carrying its own "data" payload. Actual: the sibling's field is null.

## Steps to Reproduce
1. Clone the repo and build/boot the emulator from this branch.
2. Create an AppSync API with a Lambda data source and a resolver configured for BatchInvoke.
3. Have the function return an N-element array where exactly one element is {"errorMessage": ..., "errorType": ...} and the others are {"data": {...}}.
4. Run a GraphQL query that resolves N sibling fields in one batch.
5. Observe every sibling field null instead of only the erroring field; replay appsync__lambda.appsync-batch-invoke-partial-item-error-isolates-the-failing-sibling.
