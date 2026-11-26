Issue-Source: trace-sweep-divergence
Issue-Trace: firehose-malformed-request-body-rejected
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: firehose-malformed-request-body-rejected firehose-protocol-header-validation firehose-extended-s3-buffering-update-delivers-a-put-record-read-back-from-s3 firehose-extended-s3-explicit-defaults-read-back-tag-change-remove-last-and-not-found
Issue-Shape-Id: com.amazonaws.firehose#Firehose_20150804
Issue-Key: firehose|ListDeliveryStreams|4xx->4xx|$.__type
Issue-Key: firehose|ListDeliveryStreams|4xx->4xx|$.message
Issue-Key: firehose|ListDeliveryStreams|4xx->4xx|$.Message
Issue-Key: firehose|ListDeliveryStreams|4xx->5xx|$.__type
Issue-Key: firehose|ListDeliveryStreams|4xx->5xx|$.message
Issue-Key: firehose|ListDeliveryStreams|4xx->5xx|status
Issue-Key: firehose|ListDeliveryStreams|2xx->2xx|$.Output
Issue-Key: firehose|ListDeliveryStreams|2xx->2xx|$.Version
Issue-Key: firehose|ListDeliveryStreams|2xx->2xx|$headers.content-type
Issue-Key: firehose|DescribeDeliveryStream|2xx->2xx|$.DeliveryStreamDescription.CreateTimestamp
Issue-Key: firehose|DescribeDeliveryStream|2xx->2xx|$.DeliveryStreamDescription.LastUpdateTimestamp
Issue-Commit: 077021ed17173fe4a565f7526f07b6650a74c582

# firehose: 5 net-new causes — the awsJson1_1 protocol error surface is re-coded and re-worded, and DescribeDeliveryStream timestamps carry sub-millisecond precision

> _Note: this issue was identified against the code base at commit `077021ed17173fe4a565f7526f07b6650a74c582`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListDeliveryStreams

## Description
Five net-new root causes in the firehose front end, all reached before (or independently of) any operation's business logic: four in the awsJson1_1 protocol layer that answers requests which cannot be deserialized or cannot be routed, and one in how DescribeDeliveryStream serializes its timestamp members. A fixer must close all five.

Cause 1 of 5: an unparseable (truncated) JSON request body is answered ValidationException with a lowercase `message` instead of SerializationException.
AWS: Smithy: the Firehose service is awsJson1_1 (service protocol trait); a body that fails to deserialize never reaches a bound operation, so none of ListDeliveryStreams' modeled errors applies — ListDeliveryStreams models no errors at all. Recorded behavior only: HTTP 400 with body member `__type` = "SerializationException" and NO lowercase `message` member. The fixed part of the requirement is the status 400 and the error code SerializationException; there is no variable part, because the response carries no message.
Emulator: HTTP 400 with `__type` = "ValidationException" and an extra lowercase `message` = "json parse error: Error at offset 12: unexpected end of stream" (the byte offset and the parser prose are the emulator's own text, not an AWS template).
Why: a client that branches on the error code sees a modeled input-validation failure and may retry or surface "invalid parameter" for what is actually a wire/serialization fault; SDK error-type dispatch on `__type` never matches SerializationException, so the request looks like a service-side validation rejection of well-formed input.
Fix: the firehose awsJson1_1 request deserializer's protocol-error mapping (the pre-dispatch path that turns a JSON parse failure into a response), not any operation handler or its validation.
Traces: firehose-malformed-request-body-rejected#task#1

Cause 2 of 5: a request body whose top level is a JSON array is answered with the wrong error code and the wrong message text, in the wrong member.
AWS: recorded behavior only (same protocol reasoning as cause 1): HTTP 400, `__type` = "SerializationException", and the detail in a CAPITALIZED `Message` member whose text is the fixed sentence "Start of list found where not expected" — that whole sentence is fixed text with no variable parts (it names the JSON token class found, not any request value). No lowercase `message` member is present.
Emulator: HTTP 400 with `__type` = "ValidationException", `Message` = "expected object start, got array start" and a duplicate lowercase `message` = "expected object start, got array start".
Why: same misclassification as cause 1, plus the message a client logs or asserts on is the emulator's parser prose rather than AWS's wording, and the duplicated lowercase member is a shape AWS never emits.
Fix: the firehose awsJson1_1 request deserializer's protocol-error mapping — error code, message wording, and which member carries it.
Traces: firehose-malformed-request-body-rejected#task#2

Cause 3 of 5: an X-Amz-Target naming an operation that does not exist in the Firehose API is answered 501 NotImplemented instead of 400 UnknownOperationException.
AWS: recorded behavior only: HTTP 400 with `__type` = "UnknownOperationException" and NO `message` member. The target in this step names "DescribeDeliveryStreams" (plural), which is not a Firehose operation — Smithy: the service binds DescribeDeliveryStream, not DescribeDeliveryStreams — so this is the protocol layer's unknown-operation refusal, NOT an unimplemented-operation signal: there is nothing to implement.
Emulator: HTTP 501 with `__type` = "NotImplemented" and `message` = "operation DescribeDeliveryStreams is not implemented in this emulator" (the operation name is taken from the target header; the surrounding sentence is emulator-authored text AWS never emits).
Why: 501 is not a status AWS's awsJson1_1 front end returns for a bad target, and it is treated as a transient/server fault by retry policies, so a client that typo'd a target spins on retries instead of failing fast on a 400 client error; the emulator also advertises itself in the message.
Fix: the firehose awsJson1_1 target-routing layer — an unresolvable target must produce the protocol's 400 UnknownOperationException, and only targets that name a real but unimplemented operation may take the emulator's not-implemented path.
Traces: firehose-protocol-header-validation#task#1

Cause 4 of 5: a request the protocol layer cannot route is served as a successful operation instead of the Coral unknown-operation envelope.
AWS: recorded behavior only: HTTP 200 with content-type "application/json" and the Coral JSON-RPC envelope body {"Output": {"__type": "com.amazon.coral.service#UnknownOperationException"}, "Version": "1.0"} — fixed text throughout, no values from the request.
Emulator: HTTP 200 with content-type "application/x-amz-json-1.1" and an ordinary ListDeliveryStreams result body — no `Output` member and no `Version` member. The emulator routed and executed the operation for a request AWS declines to route.
Why: the emulator accepts and acts on requests AWS refuses, so a client's protocol-level mistake silently succeeds against the emulator and then fails in production; a client that inspects the `Output`/`Version` envelope (or the content-type) to detect the Coral refusal never sees it.
Fix: the firehose awsJson1_1 target-routing layer — the same pre-dispatch path as cause 3, which must refuse rather than default to an operation, and must answer the Coral envelope with content-type application/json.
Traces: firehose-protocol-header-validation#task#2

Cause 5 of 5: DescribeDeliveryStream serializes CreateTimestamp and LastUpdateTimestamp with sub-millisecond fractional precision, a shape AWS never emits.
AWS: Smithy: DescribeDeliveryStream output DeliveryStreamDescription.CreateTimestamp and .LastUpdateTimestamp both target com.amazonaws.firehose#Timestamp, a plain timestamp with no @timestampFormat, so under awsJson1_1 they serialize as epoch-seconds numbers; the recorded responses' values are shape-asserted (not value-asserted) and carry at most millisecond precision.
Emulator: the numbers carry seven fractional digits (sub-millisecond), e.g. a CreateTimestamp of the form 1794933719.4609642 and a LastUpdateTimestamp of the form 1794933724.925319, which fails the recorded timestamp shape. The specific instants are per-run values and are not the requirement; the precision/format is. Hypothesis: the handler writes the wall-clock reading straight out as f64 seconds instead of truncating to the millisecond resolution AWS emits.
Why: a client that formats or round-trips the timestamp (or compares it against a value it re-serializes) gets digits AWS never produces, and strict consumers/recorders that expect epoch-seconds-with-millis reject or mangle the value. The same defect shows on a freshly created stream (CreateTimestamp alone) and after UpdateDestination (CreateTimestamp and LastUpdateTimestamp together).
Fix: the DescribeDeliveryStream output serializer / the state's timestamp representation for DeliveryStreamDescription.
Traces: firehose-extended-s3-buffering-update-delivers-a-put-record-read-back-from-s3#task#5 (CreateTimestamp; same cluster also covers firehose-extended-s3-explicit-defaults-read-back-tag-change-remove-last-and-not-found#task#5) and firehose-extended-s3-buffering-update-delivers-a-put-record-read-back-from-s3#task#7 (CreateTimestamp and LastUpdateTimestamp).

No open issue owns these steps: the replay reported them with an empty owned_by and no blocking stub. Existing open firehose issues on neighbouring ground (20261102T123530Z-firehose-iceberg-create-update-validation-misses-the-glue.md, 20261117T121751Z-firehose-4-net-new-causes-createdeliverystream-s-two.md) concern operation-level validation wording, not the protocol front end or timestamp serialization.

## Evidence
Replayed against the emulator from this checkout (trace set simparity-wire-aws-v1, 303 steps, 274 passed).

Cause 1 — fixture firehose-malformed-request-body-rejected, step task#1, ListDeliveryStreams: expected status 400 / actual 400; $.__type: expected 'SerializationException', got 'ValidationException'; $.message: expected absent, got 'json parse error: Error at offset 12: unexpected end of stream'.

Cause 2 — fixture firehose-malformed-request-body-rejected, step task#2, ListDeliveryStreams: expected 400 / actual 400; $.__type: expected 'SerializationException', got 'ValidationException'; $.Message: expected 'Start of list found where not expected', got 'expected object start, got array start'; $.message: expected absent, got 'expected object start, got array start'.

Cause 3 — fixture firehose-protocol-header-validation, step task#1: expected status 400, got 501; $.__type: expected 'UnknownOperationException', got 'NotImplemented'; $.message: expected absent, got 'operation DescribeDeliveryStreams is not implemented in this emulator'.

Cause 4 — fixture firehose-protocol-header-validation, step task#2: status 200 both sides; $.Output: expected {'__type': 'com.amazon.coral.service#UnknownOperationException'}, got None; $.Version: expected '1.0', got None; $headers.content-type: expected 'application/json', got 'application/x-amz-json-1.1'.

Cause 5 — fixture firehose-extended-s3-buffering-update-delivers-a-put-record-read-back-from-s3, step task#5, DescribeDeliveryStream: status 200 both sides; $.DeliveryStreamDescription.CreateTimestamp: shape-asserted epoch-seconds expected, got 1794933719.4609642 (cluster also covers firehose-extended-s3-explicit-defaults-read-back-tag-change-remove-last-and-not-found#task#5). Step task#7 of the same fixture: $.DeliveryStreamDescription.CreateTimestamp got 1794933719.4609642 and $.DeliveryStreamDescription.LastUpdateTimestamp got 1794933724.925319, both failing the recorded shape. The literal instants are per-run; the seven-digit fractional precision is the divergence.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the simparity-wire-aws-v1 corpus (replay_wire_corpus).
2. Cause 1: POST a firehose awsJson1_1 request (X-Amz-Target Firehose_20150804.ListDeliveryStreams) with a truncated/unparseable JSON body — fixture firehose-malformed-request-body-rejected step task#1. Observe __type ValidationException plus a lowercase message instead of __type SerializationException with no message.
3. Cause 2: same target with a body whose top level is a JSON array — step task#2 of that fixture. Observe __type ValidationException and Message 'expected object start, got array start' instead of SerializationException / 'Start of list found where not expected'.
4. Cause 3: POST with X-Amz-Target naming the non-existent operation DescribeDeliveryStreams — fixture firehose-protocol-header-validation step task#1. Observe 501 NotImplemented instead of 400 UnknownOperationException.
5. Cause 4: replay step task#2 of firehose-protocol-header-validation (the request AWS answers with the Coral envelope). Observe a successful x-amz-json-1.1 ListDeliveryStreams result instead of 200 application/json {"Output":{"__type":"com.amazon.coral.service#UnknownOperationException"},"Version":"1.0"}.
6. Cause 5: create an ExtendedS3 delivery stream, call DescribeDeliveryStream, then UpdateDestination and DescribeDeliveryStream again — fixture firehose-extended-s3-buffering-update-delivers-a-put-record-read-back-from-s3 steps task#5 and task#7. Inspect CreateTimestamp/LastUpdateTimestamp: they carry seven fractional digits instead of epoch-seconds at millisecond resolution.
