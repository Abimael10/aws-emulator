Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: firehose.create-stream-extended-s3-and-http-endpoint-destinations-rejected firehose.create-stream-http-endpoint-name-whitespace-only-rejected firehose.create-stream-http-endpoint-url-not-https-rejected firehose.create-stream-tag-reserved-aws-prefix-rejected
Issue-Shape-Id: com.amazonaws.firehose#Firehose_20150804
Issue-Key: firehose.create-stream-extended-s3-and-http-endpoint-destinations-rejected|run#12|AssertionError|assert message == expected, f"expected message {expected!r}, got {message!r}"
Issue-Key: firehose.create-stream-http-endpoint-name-whitespace-only-rejected|_error_code_and_message#13|AssertionError|raise AssertionError(f"expected a ClientError, call succeeded and returned {result!r}")
Issue-Key: firehose.create-stream-http-endpoint-url-not-https-rejected|run#16|AssertionError|assert message == expected, f"expected message {expected!r}, got {message!r}"
Issue-Key: firehose.create-stream-tag-reserved-aws-prefix-rejected|run#13|AssertionError|assert code == "InvalidArgumentException", f"CODE={code}"
Issue-Commit: 9d0149c39ab5c73582ec7b48950d23789498e457

# firehose: 4 net-new causes — CreateDeliveryStream's two-destination and reserved-tag refusals are re-worded/re-coded, a whitespace-only HTTP endpoint Name is accepted, and the non-https URL message quotes the wrong pattern

> _Note: this issue was identified against the code base at commit `9d0149c39ab5c73582ec7b48950d23789498e457`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDeliveryStream

## Description
FOUR net-new root causes in the firehose CreateDeliveryStream request-validation path, all grounded in firehose.* checks the replay marks neither ALREADY FILED nor OWNED. Relationship to open issues: 20261028T075033Z-firehose-3-net-new-root-causes-the-resourcenotfoundexception.md and 20261102T123530Z-firehose-iceberg-create-update-validation-misses-the-glue.md carry OTHER CreateDeliveryStream refusal wordings (ResourceNotFound/KMS/Iceberg); none of them covers the four refusals below, and the replay does not mark these checks as owned by them.

=== Cause 1 of 4: naming two destination configurations is refused with the emulator's own wording ===
Failing check: firehose.create-stream-extended-s3-and-http-endpoint-destinations-rejected.
AWS: Smithy — CreateDeliveryStream's destination members (ExtendedS3DestinationConfiguration, HttpEndpointDestinationConfiguration, S3DestinationConfiguration, RedshiftDestinationConfiguration, ...) are each optional and mutually exclusive; a request naming two of them is refused with InvalidArgumentException (errorKind client, httpError 400, "The specified input parameter has a value that is not valid"). Recorded behavior: the message is the fixed text "Exactly one destination configuration is supported for a Firehose" — it has NO variable part, it does not name which members were supplied.
Emulator: the code is right (InvalidArgumentException) but the message is emulator-authored: "Exactly one of ExtendedS3DestinationConfiguration or HttpEndpointDestinationConfiguration must be specified."
Why: a client that matches on the refusal text (test assertions, error classifiers, support runbooks) matches nothing in production, and the emulator-authored sentence also implies only those two members exist.
Fix: the CreateDeliveryStream handler's destination-arity validation in the firehose service crate must emit AWS's fixed sentence for this class.
Traces: the check id above.

=== Cause 2 of 4: a whitespace-only HttpEndpointConfiguration.Name is ACCEPTED and the stream is created ===
Failing check: firehose.create-stream-http-endpoint-name-whitespace-only-rejected.
AWS: Smithy — HttpEndpointConfiguration.Name targets HttpEndpointName with @length(min 1, max 256) and @pattern "^(?!\\s*$).+$", so "   " violates the request shape and is refused before any destination work: HTTP 400 with code ValidationException (the protocol-level request-shape error; it is not one of the modeled Firehose errors) and the standard constraint message "1 validation error detected: Value at 'httpEndpointDestinationConfiguration.endpointConfiguration.name' failed to satisfy constraint: Member must satisfy regular expression pattern: ^(?!\\s*$).+". Fixed text = "N validation error(s) detected: Value at '<member path>' failed to satisfy constraint: Member must satisfy regular expression pattern: <pattern>"; the variable parts are the camelCased member path taken from the request and the pattern taken from the model (AWS quotes it without the trailing anchor, per the recording). No stream exists afterwards: DescribeDeliveryStream for that name answers ResourceNotFoundException "Firehose under account <accountId> not found."
Emulator: the call SUCCEEDS — HTTP 200 with {"DeliveryStreamARN": "arn:aws:firehose:<region>:<account>:deliverystream/<name>"} — so the pattern/length constraint on this member is never enforced and an invalid stream is left in state.
Why: this is an overly-permissive accept. A configuration real AWS refuses at create time is silently created against the emulator, so the client only discovers the invalid endpoint name in production, and the negative test it wrote passes for the wrong reason.
Fix: request-shape validation for CreateDeliveryStream (the firehose crate's input validation layer, not the destination handler) must enforce the modeled @pattern/@length on HttpEndpointConfiguration.Name and answer the constraint-violation ValidationException with the member path and pattern, creating nothing.
Traces: the check id above.

=== Cause 3 of 4: the non-https endpoint URL refusal quotes the model anchor instead of AWS's pattern text ===
Failing check: firehose.create-stream-http-endpoint-url-not-https-rejected.
AWS: Smithy — HttpEndpointConfiguration.Url targets HttpEndpointUrl, required, @length(1,1000), @pattern "^https://". A "http://..." URL is refused with the constraint message "1 validation error detected: Value at 'httpEndpointDestinationConfiguration.endpointConfiguration.url' failed to satisfy constraint: Member must satisfy regular expression pattern: https://.*" (recorded behavior for the quoted pattern text: AWS renders it as "https://.*", un-anchored and with the trailing ".*").
Emulator: same code and same sentence, but the quoted pattern is truncated to "https://" — "... Member must satisfy regular expression pattern: https://".
Why: the pattern is the actionable part of a constraint message; clients surface it to users and tests pin it. A different pattern string means the emulator's refusal text can never be matched against the production one.
Fix: the constraint-message renderer for firehose request validation must emit the pattern AWS quotes for this member rather than the raw model anchor.
Traces: the check id above.

=== Cause 4 of 4: a reserved "aws:" tag key is refused with the wrong error code ===
Failing check: firehose.create-stream-tag-reserved-aws-prefix-rejected.
AWS: a CreateDeliveryStream whose Tags carry a key beginning with the reserved prefix "aws:" is refused with the modeled InvalidArgumentException (errorKind client, httpError 400) — a semantic tag-policy refusal, not a request-shape one — with the fixed message "Some tag keys in the request started with 'aws:'. System tags cannot be modified." (fixed text only; the offending keys are not interpolated). The stream is not created: DescribeDeliveryStream answers ResourceNotFoundException "Firehose under account <accountId> not found."
Emulator: the request IS refused, but with code ValidationException ("CODE=ValidationException"), i.e. it is reported as a request-shape violation rather than the modeled InvalidArgumentException.
Why: ValidationException is not even a member of Firehose's error set, so an SDK cannot map it to a modeled exception type; retry/catch logic written against InvalidArgumentException does not fire, and callers that branch on the code mis-handle a reserved-tag mistake.
Fix: the tag validation in the CreateDeliveryStream handler must raise the modeled InvalidArgumentException with AWS's sentence instead of routing through the generic request-shape validation error.
Traces: the check id above.

## Evidence
Replayed suite: simparity-behavioral-aws-v1 — firehose 178/183 (97%), 5 failing checks of which these four are marked neither ALREADY FILED nor OWNED.

Cause 1 — firehose.create-stream-extended-s3-and-http-endpoint-destinations-rejected
Expected (AWS): InvalidArgumentException, message "Exactly one destination configuration is supported for a Firehose".
Actual (emulator): "AssertionError: expected message 'Exactly one destination configuration is supported for a Firehose', got 'Exactly one of ExtendedS3DestinationConfiguration or HttpEndpointDestinationConfiguration must be specified.'"

Cause 2 — firehose.create-stream-http-endpoint-name-whitespace-only-rejected
Expected (AWS): HTTP 400 ValidationException, message "1 validation error detected: Value at 'httpEndpointDestinationConfiguration.endpointConfiguration.name' failed to satisfy constraint: Member must satisfy regular expression pattern: ^(?!\\s*$).+", and the stream absent afterwards.
Actual (emulator): "AssertionError: expected a ClientError, call succeeded and returned {'DeliveryStreamARN': 'arn:aws:firehose:us-west-2:000000000000:deliverystream/sim2real-behav-fhhttpname-bad-<suffix>', 'ResponseMetadata': {'HTTPStatusCode': 200, ...}}" — the positive control with a non-blank Name reached ACTIVE in the same check, so only the constraint is missing.

Cause 3 — firehose.create-stream-http-endpoint-url-not-https-rejected
Expected (AWS): "... Value at 'httpEndpointDestinationConfiguration.endpointConfiguration.url' failed to satisfy constraint: Member must satisfy regular expression pattern: https://.*"
Actual (emulator): "... Member must satisfy regular expression pattern: https://" (the assertion diff quotes exactly these two strings).

Cause 4 — firehose.create-stream-tag-reserved-aws-prefix-rejected
Expected (AWS): InvalidArgumentException, message "Some tag keys in the request started with 'aws:'. System tags cannot be modified."
Actual (emulator): "AssertionError: CODE=ValidationException" — the check stops at the code comparison, before the message is compared.

## Steps to Reproduce
Common setup: boot the emulator, point botocore at it; s3.create_bucket(Bucket=B) (+ LocationConstraint outside us-east-1); iam.create_role(R) trusted by firehose.amazonaws.com with s3:PutObject/PutObjectAcl/GetBucketLocation/ListBucket on B and B/*.

Cause 1:
1. firehose.create_delivery_stream(DeliveryStreamName=N1, DeliveryStreamType='DirectPut', ExtendedS3DestinationConfiguration={'RoleARN': R, 'BucketARN': 'arn:aws:s3:::B'}, HttpEndpointDestinationConfiguration={'EndpointConfiguration': {'Url': 'https://example.com/firehose', 'Name': 'sim2real-http-endpoint'}, 'S3Configuration': {'RoleARN': R, 'BucketARN': 'arn:aws:s3:::B'}, 'RoleARN': R}).
2. Expect InvalidArgumentException with message 'Exactly one destination configuration is supported for a Firehose'; observe the emulator's own 'Exactly one of ... must be specified.' sentence.

Cause 2:
1. Same call with ONLY HttpEndpointDestinationConfiguration, EndpointConfiguration={'Url': 'https://example.com/firehose', 'Name': '   '}.
2. Expect HTTP 400 ValidationException quoting the member path '...endpointConfiguration.name' and the pattern '^(?!\\s*$).+'; observe HTTP 200 and a DeliveryStreamARN.
3. describe_delivery_stream(DeliveryStreamName=that name) — expect ResourceNotFoundException; observe the stream exists.

Cause 3:
1. Same HTTP-endpoint call with Url='http://example.com/firehose'.
2. Expect the constraint message to quote 'https://.*'; observe 'https://'.

Cause 4:
1. create_delivery_stream(DeliveryStreamName=N4, DeliveryStreamType='DirectPut', ExtendedS3DestinationConfiguration={'RoleARN': R, 'BucketARN': 'arn:aws:s3:::B'}, Tags=[{'Key': 'aws:reserved', 'Value': '1'}]).
2. Expect InvalidArgumentException with "Some tag keys in the request started with 'aws:'. System tags cannot be modified."; observe ValidationException.

Finally re-replay the four check ids in trace_ids.
