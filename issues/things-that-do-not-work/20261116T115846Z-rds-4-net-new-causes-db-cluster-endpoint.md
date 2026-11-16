Issue-Source: trace-sweep-divergence
Issue-Trace: rds-db-cluster-endpoint-lifecycle
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rds-db-cluster-endpoint-lifecycle
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds|DescribeDBClusterEndpoints|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: rds|CreateDBClusterEndpoint|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: rds|ModifyDBClusterEndpoint|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: rds|DeleteDBClusterEndpoint|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: 13e968f2d712b86cbfec63dd795b2bc21b7f9b4c

# rds: 4 net-new causes — DB cluster endpoint not-found refusals carry the wrong message text

> _Note: this issue was identified against the code base at commit `13e968f2d712b86cbfec63dd795b2bc21b7f9b4c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBClusterEndpoints

## Description
Four net-new wire causes on the rds custom DB-cluster-endpoint lifecycle (DescribeDBClusterEndpoints, CreateDBClusterEndpoint, ModifyDBClusterEndpoint, DeleteDBClusterEndpoint). In every one the HTTP status and the awsQuery error class already match AWS; what diverges is the error MESSAGE, which the emulator authors itself instead of emitting the sentence RDS emits. Two distinct wrong sentences are involved (one for the cluster-not-found class, one for the endpoint-not-found class), and each of the four is its own replay cluster, so each must be closed on its own.

Cause 1 of 4: DescribeDBClusterEndpoints' cluster-not-found refusal is worded by the emulator.
AWS: DescribeDBClusterEndpoints takes an optional DBClusterIdentifier / DBClusterEndpointIdentifier and models exactly one error (Smithy: DescribeDBClusterEndpoints -> errors [DBClusterNotFoundFault], errorKind client, httpError 404, awsQueryError code 'DBClusterNotFoundFault' httpResponseCode 404; DBClusterNotFoundFault documentation: 'DBClusterIdentifier doesn't refer to an existing DB cluster.'; message member -> ExceptionMessage). The model does not fix the message text, so recorded behavior supplies it: HTTP 404 with the awsQuery envelope ErrorResponse/Error whose Message is the fixed text 'The source cluster could not be found or cannot be accessed: ' followed by the DBClusterIdentifier exactly as the request sent it. The only variable part of that sentence is the request-supplied cluster identifier; everything up to and including the colon-space is fixed. The RequestId in the recording is a per-run value and is not a requirement.
Emulator: status 404 and the DBClusterNotFoundFault code match, but the Message is the emulator's internal lookup formatting — the fixed words 'DBCluster ' + the identifier + ' not found.' This is the emulator's internal state-type name ('DBCluster') in a generic '<Type> <id> not found.' debug form; it shares no fixed text with AWS's sentence and drops the 'or cannot be accessed' half of the condition entirely.
Why: a client that branches on the refusal text — and every log scraper, SDK integration assertion or operator runbook written against AWS's 'The source cluster could not be found or cannot be accessed: <id>' — never matches, so an ordinary missing/inaccessible-cluster condition is reported as an unrecognised failure. Because the status and error code are correct, the mismatch is silent: the caller sees the right error class carrying text no AWS-targeted test accepts.
Fix: the DescribeDBClusterEndpoints handler's cluster-resolution path, where DBClusterNotFoundFault is constructed — emit RDS's fixed sentence with the request's DBClusterIdentifier interpolated instead of the generic '<Type> <id> not found.' formatter shared by the emulator's state lookups.
Traces: rds-db-cluster-endpoint-lifecycle#task#1.

Cause 2 of 4: CreateDBClusterEndpoint's cluster-not-found refusal is worded the same emulator way.
AWS: CreateDBClusterEndpoint requires DBClusterIdentifier, DBClusterEndpointIdentifier and EndpointType, and models DBClusterNotFoundFault among its errors (Smithy: CreateDBClusterEndpoint -> errors include DBClusterNotFoundFault, httpError 404, awsQueryError code 'DBClusterNotFoundFault' httpResponseCode 404). Recorded behavior for a DBClusterIdentifier that names no cluster: HTTP 404, ErrorResponse/Error/Message = the same fixed text 'The source cluster could not be found or cannot be accessed: ' + the request's DBClusterIdentifier. Same fixed text, same single request-derived variable as cause 1.
Emulator: status 404 and code match; the Message is again 'DBCluster ' + the identifier + ' not found.'
Why: same client impact as cause 1, on the create half of the lifecycle. A provisioning flow that distinguishes 'the cluster is not there yet / not reachable' from a genuine fault by message text mis-classifies the refusal and retries or aborts wrongly.
Fix: the CreateDBClusterEndpoint handler's cluster-resolution path — the same DBClusterNotFoundFault construction as cause 1; both read paths and this write path must emit RDS's wording.
Traces: rds-db-cluster-endpoint-lifecycle#task#2.

Cause 3 of 4: ModifyDBClusterEndpoint's endpoint-not-found refusal emits the model's documentation sentence instead of RDS's message.
AWS: ModifyDBClusterEndpoint requires DBClusterEndpointIdentifier and models DBClusterEndpointNotFoundFault (Smithy: ModifyDBClusterEndpoint -> errors include DBClusterEndpointNotFoundFault, errorKind client, httpError 400, awsQueryError code 'DBClusterEndpointNotFoundFault' httpResponseCode 400; DBClusterEndpointNotFoundFault documentation: 'The specified custom endpoint doesn't exist.'). The documentation string describes the error class; it is NOT the wire message. Recorded behavior: HTTP 400 with ErrorResponse/Error/Message = the fixed text 'DBClusterEndpoint ' + the DBClusterEndpointIdentifier exactly as the request sent it + ' not found' (no trailing period). The endpoint identifier is the only variable part.
Emulator: status 400 and the DBClusterEndpointNotFoundFault code match, but the Message is the model's documentation sentence verbatim — "The specified custom endpoint doesn't exist." — which names neither the member nor the identifier that was not found.
Why: the refusal carries no identifier, so a client modifying several endpoints in one flow cannot tell WHICH endpoint was missing from the response alone, and any assertion or parser keyed on AWS's 'DBClusterEndpoint <id> not found' fails. Hypothesis: the handler is populating the fault's message from the shape's documentation trait rather than formatting RDS's message — which is why this operation's wording differs in kind from causes 1 and 2.
Fix: the ModifyDBClusterEndpoint handler's endpoint-resolution path (the DBClusterEndpointNotFoundFault construction) — format AWS's message from the request's DBClusterEndpointIdentifier rather than reusing the error shape's documentation text.
Traces: rds-db-cluster-endpoint-lifecycle#task#3.

Cause 4 of 4: DeleteDBClusterEndpoint's endpoint-not-found refusal has the same documentation-string message.
AWS: DeleteDBClusterEndpoint requires only DBClusterEndpointIdentifier and models DBClusterEndpointNotFoundFault with the identical classification (Smithy: DeleteDBClusterEndpoint -> errors include DBClusterEndpointNotFoundFault, httpError 400, awsQueryError code 'DBClusterEndpointNotFoundFault' httpResponseCode 400). Recorded behavior: HTTP 400, ErrorResponse/Error/Message = 'DBClusterEndpoint ' + the request's DBClusterEndpointIdentifier + ' not found' — same fixed text and same single variable as cause 3.
Emulator: status 400 and code match; the Message is again the documentation sentence "The specified custom endpoint doesn't exist."
Why: same impact as cause 3 on the delete half. An idempotent teardown that treats 'already gone' as success by matching the not-found text never recognises the response, so cleanup reports a hard failure against a cluster endpoint that is simply absent.
Fix: the DeleteDBClusterEndpoint handler's endpoint-resolution path, the same DBClusterEndpointNotFoundFault construction as cause 3.
Traces: rds-db-cluster-endpoint-lifecycle#task#4.

All four clusters are reported unowned by the replay. The open issue '20261111T130206Z-rds-enable-disablehttpendpoint-not-found-refusals-carry-emulator.md' is the same FAMILY of defect (emulator-authored RDS refusal text) but covers EnableHttpEndpoint/DisableHttpEndpoint on fixture rds-db-cluster-http-endpoint and does not carry these four replay keys; '20261112T074631Z-rds-snapshot-copy-restore-read-replica-flows-never.md' concerns DB proxy endpoint hostnames, not these refusal messages. Neither covers the four causes above.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up=true, 99 fixtures replayed, 993 steps (549 passed), score 55.3%, 82 divergence clusters. These four are the only clusters with an empty owned_by and an empty stub_path — every other cluster is already owned by an open issue.

Cause 1 — rds-db-cluster-endpoint-lifecycle#task#1, DescribeDBClusterEndpoints, status expected 404, got 404:
  $.ErrorResponse.Error.Message: value_mismatch
    expected: 'The source cluster could not be found or cannot be accessed: <DBClusterIdentifier sent by the request>'
    got:      'DBCluster <DBClusterIdentifier sent by the request> not found.'
  Fixed text of AWS's message: 'The source cluster could not be found or cannot be accessed: '. Variable part: the request's DBClusterIdentifier (in the recording, the fixture's nonexistent cluster identifier).

Cause 2 — rds-db-cluster-endpoint-lifecycle#task#2, CreateDBClusterEndpoint, status expected 404, got 404:
  $.ErrorResponse.Error.Message: value_mismatch
    expected: 'The source cluster could not be found or cannot be accessed: <DBClusterIdentifier sent by the request>'
    got:      'DBCluster <DBClusterIdentifier sent by the request> not found.'

Cause 3 — rds-db-cluster-endpoint-lifecycle#task#3, ModifyDBClusterEndpoint, status expected 400, got 400:
  $.ErrorResponse.Error.Message: value_mismatch
    expected: 'DBClusterEndpoint <DBClusterEndpointIdentifier sent by the request> not found'
    got:      "The specified custom endpoint doesn't exist."
  Fixed text of AWS's message: 'DBClusterEndpoint ' ... ' not found' (no trailing period). Variable part: the request's DBClusterEndpointIdentifier. The emulator's text is the DBClusterEndpointNotFoundFault documentation string and contains no identifier at all.

Cause 4 — rds-db-cluster-endpoint-lifecycle#task#4, DeleteDBClusterEndpoint, status expected 400, got 400:
  $.ErrorResponse.Error.Message: value_mismatch
    expected: 'DBClusterEndpoint <DBClusterEndpointIdentifier sent by the request> not found'
    got:      "The specified custom endpoint doesn't exist."

In all four clusters the ONLY diverging member is $.ErrorResponse.Error.Message — the HTTP status (404 for the DBClusterNotFoundFault pair, 400 for the DBClusterEndpointNotFoundFault pair) and the awsQuery error code/Type already match the recording, so the defect is strictly the message wording.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. Replay the wire corpus (trace set simparity-wire-aws-v1).
3. Cause 1: replay fixture rds-db-cluster-endpoint-lifecycle step task#1 — DescribeDBClusterEndpoints with a DBClusterIdentifier that names no DB cluster answers 404 DBClusterNotFoundFault with Message 'DBCluster <id> not found.' instead of AWS's 'The source cluster could not be found or cannot be accessed: <id>'.
4. Cause 2: replay fixture rds-db-cluster-endpoint-lifecycle step task#2 — CreateDBClusterEndpoint against the same nonexistent DBClusterIdentifier answers the same emulator-worded 404 message instead of AWS's sentence.
5. Cause 3: replay fixture rds-db-cluster-endpoint-lifecycle step task#3 — ModifyDBClusterEndpoint with a DBClusterEndpointIdentifier that names no custom endpoint answers 400 DBClusterEndpointNotFoundFault with the documentation sentence "The specified custom endpoint doesn't exist." instead of AWS's 'DBClusterEndpoint <id> not found'.
6. Cause 4: replay fixture rds-db-cluster-endpoint-lifecycle step task#4 — DeleteDBClusterEndpoint on the same nonexistent endpoint identifier answers the same documentation sentence instead of AWS's message.
7. In each response compare $.ErrorResponse.Error.Message against the recorded expectation; the status and error code already match, so only the message needs to change.
