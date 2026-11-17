Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb.sigv4-non-aws4-header-and-query-missing-signedheaders-rejected dynamodb.bulk-put-items-client-token-replay-returns-one-operation dynamodb.batch-write-item-aggregate-25-item-limit-spans-every-table-in-the-call dynamodb.transact-get-items-top-level-vs-cancellation-error-class dynamodb.search-vectors-dot-product-score-rounds-integer-above-2-24-to-float32
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb.sigv4-non-aws4-header-and-query-missing-signedheaders-rejected|expect_error#22|AssertionError|assert status == 400, f"expected status 400, got {status!r}: {body!r}"
Issue-Key: dynamodb|StartBulkPutItems|UnknownOperationException
Issue-Key: dynamodb.batch-write-item-aggregate-25-item-limit-spans-every-table-in-the-call|run#20|AssertionError|assert message == _AGGREGATE_MESSAGE, (
Issue-Key: dynamodb.transact-get-items-top-level-vs-cancellation-error-class|run#32|AssertionError|assert 'Item' not in responses[0], (
Issue-Key: dynamodb.search-vectors-dot-product-score-rounds-integer-above-2-24-to-float32|run#19|KeyError|projected_unrepresentable = by_pk['unrepresentable']['Item']['emb']['L'][0]['N']
Issue-Commit: 3d549b51f10ecf86bd29287e87008ba36d5f0d1d

# DynamoDB: 5 net-new causes — malformed SigV4 answers 403 instead of 400, StartBulkPutItems is not served, BatchWriteItem's over-25 refusal dumps the request, TransactGetItems emits an empty Item, and SearchVectors omits the vector attribute

> _Note: this issue was identified against the code base at commit `3d549b51f10ecf86bd29287e87008ba36d5f0d1d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutItem

## Description
Five net-new root causes spanning the HTTP/SigV4 front door, a missing operation, and three response-shape defects. All grounded in failing checks listed under '### dynamodb'. Related open issues that do NOT cover these checks: 20261111T170613Z-dynamodb-the-http-sigv4-layer-omits-x-amz.md, 20261111T170613Z-dynamodb-item-size-batch-size-and-request-size.md, 20261111T170613Z-dynamodb-vector-index-validation-is-not-enforced-its.md.

Cause 1 of 5: a malformed Authorization header, or a presigned query string missing X-Amz-SignedHeaders, answers 403 UnrecognizedClientException instead of 400.
AWS: a request whose Authorization header is not an AWS4 credential at all answers HTTP 400 with body {'__type': 'com.amazon.coral.service#MissingAuthenticationTokenException', 'message': 'Request is missing Authentication Token'}. A query-string SigV4 request carrying every parameter except X-Amz-SignedHeaders answers HTTP 400 with {'__type': 'com.amazon.coral.service#IncompleteSignatureException', 'message': "AWS query-string parameters must include 'X-Amz-SignedHeaders'. Re-examine the query-string parameters."} — naming only the one missing parameter. Both responses also carry an x-amz-crc32 header matching the body. Nothing is written.
Emulator: answers HTTP 403 with {'__type': 'UnrecognizedClientException', 'message': 'The security token included in the request is invalid.'} for both — wrong status, wrong (and unqualified) __type, wrong message.
Why: a client cannot distinguish 'your request was not signed / is incomplete' (a 400, fix the request) from 'your credentials are rejected' (a 403, fix the identity); retry and credential-refresh logic keyed on the status and code behaves wrongly.
Fix: the HTTP SigV4 authentication layer's classification of malformed/incomplete signatures, and the error serializer's __type namespace.
Traces: dynamodb.sigv4-non-aws4-header-and-query-missing-signedheaders-rejected

Cause 2 of 5: StartBulkPutItems is not served at all.
AWS: recorded behavior only — the recording calls StartBulkPutItems and replays its ClientToken to confirm one operation is returned, so AWS serves the operation and makes it idempotent per ClientToken. Note the on-disk Smithy model carries no StartBulkPutItems shape (smithy_query: "operation 'StartBulkPutItems' not found"), so the model also predates the operation.
Emulator: answers UnknownOperationException for the StartBulkPutItems target, so the whole bulk-put flow is unreachable.
Why: any client using the bulk put path gets an unknown-operation refusal instead of a result.
Fix: add StartBulkPutItems (and its ClientToken replay/idempotency state) to the DynamoDB operation router and refresh the service's Smithy model so the operation is modelled.
Traces: dynamodb.bulk-put-items-client-token-replay-returns-one-operation

Cause 3 of 5: BatchWriteItem's aggregate 25-item refusal dumps the whole request instead of AWS's message.
AWS: when the item count summed across EVERY table in one BatchWriteItem exceeds 25, the refusal is ValidationException 'Too many items requested for the BatchWriteItem call' — fixed text, no request echo.
Emulator: answers a constraint-style message that serialises the entire RequestItems map into the text: "1 validation error detected: Value '{<table>=[WriteRequest(putRequest=PutRequest(item={pk=AttributeValue(s=over-a-0, n=null, b=null, ...)}, workloadProfileName=null), deleteRequest=null), ..." — kilobytes of internal field names.
Why: the message is not AWS's, it leaks internal struct shapes, and it is unbounded in size.
Fix: the BatchWriteItem aggregate-count validation must raise the fixed message instead of the generic constraint reporter.
Traces: dynamodb.batch-write-item-aggregate-25-item-limit-spans-every-table-in-the-call

Cause 4 of 5: TransactGetItems returns 'Item': {} where AWS omits the member.
AWS: a Get whose ProjectionExpression matches no attribute of the found item answers a Responses entry with NO Item member at all (the member is absent, not an empty map).
Emulator: returns {'Item': {}}.
Why: clients test membership of Item to decide 'nothing projected / not found'; an empty map passes that test and the caller takes the wrong branch.
Fix: the TransactGetItems response serializer must omit an empty projected Item.
Traces: dynamodb.transact-get-items-top-level-vs-cancellation-error-class

Cause 5 of 5: a SearchVectors result omits the vector attribute from the projected item.
AWS: a SearchVectors match over an ALL-projected vector index returns the item including its vector attribute, so the stored vector's numbers (and their float32 rounding) can be read back from Item['<vectorAttr>']['L'][i]['N'].
Emulator: the returned Item has no such member — reading it raises KeyError('emb').
Why: a client cannot recover the vector it stored from a search result, so vector round-trip and score-precision behavior cannot be observed.
Fix: the SearchVectors response projection must include the index's vector attribute when the projection covers it.
Traces: dynamodb.search-vectors-dot-product-score-rounds-integer-above-2-24-to-float32

## Evidence
From the behavioral replay, per cause:
- SigV4: "AssertionError: expected status 400, got 403: {'__type': 'UnrecognizedClientException', 'message': 'The security token included in the request is invalid.'}" — the check's expected pairs are ('com.amazon.coral.service#MissingAuthenticationTokenException', 'Request is missing Authentication Token') and ('com.amazon.coral.service#IncompleteSignatureException', "AWS query-string parameters must include 'X-Amz-SignedHeaders'. Re-examine the query-string parameters.").
- StartBulkPutItems: "botocore.exceptions.ClientError: An error occurred (UnknownOperationException) when calling the StartBulkPutItems operation".
- BatchWriteItem over 25 across tables: "expected the aggregate-count message 'Too many items requested for the BatchWriteItem call', got \"1 validation error detected: Value '{sim2real-behav-ddbbw25-a-<id>=[WriteRequest(putRequest=PutRequest(item={pk=AttributeValue(s=over-a-0, n=null, b=null, iNT=null, ...)}, workloadProfileName=null), deleteRequest=null), ...\"".
- TransactGetItems: "a projection matching nothing must omit Item entirely, got {'Item': {}}".
- SearchVectors: "projected_unrepresentable = by_pk['unrepresentable']['Item']['emb']['L'][0]['N'] ... KeyError: 'emb'".

## Steps to Reproduce
1. Build and boot the emulator, then replay the behavioral suite (run_behavioral_suite).
2. Re-run the check ids in trace_ids; each reproduces one cause.
3. Representative manual repro: POST a PutItem body with header Authorization: X — AWS answers HTTP 400 MissingAuthenticationTokenException, the emulator answers HTTP 403 UnrecognizedClientException. Repeat with a presigned URL from which only X-Amz-SignedHeaders was removed.
4. Call StartBulkPutItems — the emulator answers UnknownOperationException.
5. Call BatchWriteItem with 13 puts into each of two tables (26 total) and compare the ValidationException message.
6. TransactGetItems a known item with a ProjectionExpression naming an absent attribute — AWS omits Item, the emulator returns Item: {}.
7. SearchVectors over an ALL-projected vector index and read Item['emb'] from a match.
