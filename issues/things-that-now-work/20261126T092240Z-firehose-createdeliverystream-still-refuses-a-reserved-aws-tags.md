Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: firehose.create-stream-tag-reserved-aws-prefix-rejected
Issue-Shape-Id: com.amazonaws.firehose#Firehose_20150804
Issue-Key: firehose.create-stream-tag-reserved-aws-prefix-rejected|run#13|AssertionError|assert code == "InvalidArgumentException", f"CODE={code}"
Issue-Commit: e935f6e0fb4111192d78ebcc86626ff508b94574

# firehose CreateDeliveryStream still refuses a reserved "aws:" Tags key with ValidationException instead of the modeled InvalidArgumentException

> _Note: this issue was identified against the code base at commit `e935f6e0fb4111192d78ebcc86626ff508b94574`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDeliveryStream

## Description
REOPENS 20261117T121751Z-firehose-4-net-new-causes-createdeliverystream-s-two.md: its fix left the call refused.

That fixed issue's Cause 4 was exactly this check (firehose.create-stream-tag-reserved-aws-prefix-rejected) and exactly this symptom; after the fix the call is still refused with the same wrong error code, so the reserved-tag refusal of CreateDeliveryStream is not fixed. Hypothesis for the residue: the neighbouring fixed issue 20261118T002623Z-firehose-3-net-new-causes-the-tagkey-pattern.md required the TagKey @pattern violation to be answered as the framework ValidationException for TagDeliveryStream/UntagDeliveryStream, and that validation now also runs ahead of CreateDeliveryStream's own tag-policy check, re-coding this refusal. The two operations differ in AWS and must be allowed to differ here.

Cause 1 of 1: a Tags entry whose Key carries the reserved "aws:" prefix is refused with the wrong error code.
Failing check: firehose.create-stream-tag-reserved-aws-prefix-rejected.
AWS: Smithy — CreateDeliveryStream.Tags targets TagDeliveryStreamInputTagList (@length 1..50) of com.amazonaws.firehose#Tag, whose required Key targets TagKey (@length 1..128, @pattern "^(?!aws:)[\\p{L}\\p{Z}\\p{N}_.:\\/=+\\-@%]*$"); the operation's modeled errors are InvalidArgumentException (errorKind client, httpError 400, doc "The specified input parameter has a value that is not valid"), InvalidKMSResourceException, LimitExceededException and ResourceInUseException — ValidationException is NOT in that set. Recorded behavior: CreateDeliveryStream answers this as a semantic tag-policy refusal, HTTP 400 code InvalidArgumentException, message "Some tag keys in the request started with 'aws:'. System tags cannot be modified." — that sentence is entirely fixed text for this error class (no member path, no index, no offending key or stream name is interpolated). The stream is not created: a following DescribeDeliveryStream for the same name answers ResourceNotFoundException whose message is the fixed template "Firehose under account <accountId> not found." with the caller's account id as its only variable part.
Emulator: the request IS refused 400, but with code ValidationException — the request-shape/constraint class — so the check stops at `assert code == "InvalidArgumentException"` with "AssertionError: CODE=ValidationException" and never reaches the message comparison.
Why: ValidationException is not a member of Firehose's error set, so an SDK cannot map the response to a modeled exception type; a caller's `except firehose.exceptions.InvalidArgumentException` (or any branch on that code) does not fire, and a reserved-tag mistake is mis-handled as a malformed-request error. The emulator also serves the wrong message class for this refusal.
Fix: the CreateDeliveryStream handler's tag validation in the firehose service crate — the reserved-"aws:"-prefix check for the Tags member must raise the modeled InvalidArgumentException with AWS's fixed sentence, and must not be routed through (or pre-empted by) the generic TagKey @pattern request-shape validation that TagDeliveryStream/UntagDeliveryStream legitimately use.
Traces: firehose.create-stream-tag-reserved-aws-prefix-rejected (the check's positive control — the same role/bucket with an ordinary tag key — creates a stream that reaches ACTIVE, so only the reserved-prefix refusal class diverges).

## Evidence
Replayed suite simparity-behavioral-aws-v1 against the emulator from this checkout: 300/307 passed; service heading '### firehose' 182/183 with 1 failing check, marked neither ALREADY FILED nor OWNED.

firehose.create-stream-tag-reserved-aws-prefix-rejected
Request: CreateDeliveryStream(DeliveryStreamName=<fresh name>, DeliveryStreamType='DirectPut', ExtendedS3DestinationConfiguration={'RoleARN': <firehose-trusted role>, 'BucketARN': 'arn:aws:s3:::<bucket>'}, Tags=[{'Key': 'aws:reserved', 'Value': '1'}]).
Expected (AWS): HTTP 400, Error.Code 'InvalidArgumentException', Error.Message "Some tag keys in the request started with 'aws:'. System tags cannot be modified." (all fixed text), and the stream absent afterwards (DescribeDeliveryStream -> ResourceNotFoundException "Firehose under account <accountId> not found.").
Actual (emulator): HTTP 400 with Error.Code 'ValidationException' — the replay fails at check_firehose_create_stream_tag_reserved_aws_prefix_rejected.py line 51: `assert code == "InvalidArgumentException", f"CODE={code}"` -> "AssertionError: CODE=ValidationException". The message and the stream-absence assertions follow the code assertion and were not reached.

Prior filing: 20261117T121751Z-firehose-4-net-new-causes-createdeliverystream-s-two.md (status fixed) listed this check id in Issue-Trace-Ids and its Cause 4 recorded the identical "CODE=ValidationException" divergence.

## Steps to Reproduce
1. Boot the emulator from this checkout and point botocore at it.
2. s3.create_bucket(Bucket=B) (add CreateBucketConfiguration.LocationConstraint outside us-east-1).
3. iam.create_role(R) trusted by firehose.amazonaws.com for sts:AssumeRole, with an inline policy allowing s3:PutObject, s3:PutObjectAcl, s3:GetBucketLocation, s3:ListBucket on arn:aws:s3:::B and arn:aws:s3:::B/*.
4. Positive control: firehose.create_delivery_stream(DeliveryStreamName=N1, DeliveryStreamType='DirectPut', ExtendedS3DestinationConfiguration={'RoleARN': R, 'BucketARN': 'arn:aws:s3:::B'}, Tags=[{'Key': 'simparity-check', 'Value': 'x'}]) and wait for DeliveryStreamStatus ACTIVE — this passes today.
5. firehose.create_delivery_stream(DeliveryStreamName=N2, DeliveryStreamType='DirectPut', ExtendedS3DestinationConfiguration={'RoleARN': R, 'BucketARN': 'arn:aws:s3:::B'}, Tags=[{'Key': 'aws:reserved', 'Value': '1'}]).
6. Expect ClientError with Error.Code 'InvalidArgumentException' and Error.Message "Some tag keys in the request started with 'aws:'. System tags cannot be modified."; observe Error.Code 'ValidationException'.
7. firehose.describe_delivery_stream(DeliveryStreamName=N2) must answer ResourceNotFoundException with "Firehose under account <accountId> not found." (the stream must never have been created).
8. Re-replay firehose.create-stream-tag-reserved-aws-prefix-rejected to confirm, and re-replay firehose.tag-reserved-aws-prefix-rejected and firehose.untag-delivery-stream-invalid-tag-key to confirm those two still answer ValidationException as 20261118T002623Z-firehose-3-net-new-causes-the-tagkey-pattern.md requires.
