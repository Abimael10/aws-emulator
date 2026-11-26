Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__s3.function-sdk-creates-heads-lists-and-deletes-a-bucket lambda__s3.nodejs-function-sdk-creates-lists-and-deletes-a-bucket
Issue-Key: lambda__s3.function-sdk-creates-heads-lists-and-deletes-a-bucket|run#10|AssertionError|assert err is None, f"the lifecycle function failed: {err!r} {out!r}"
Issue-Key: lambda__s3.nodejs-function-sdk-creates-lists-and-deletes-a-bucket|run#10|AssertionError|assert err is None, f"the lifecycle function failed: {err!r} {out!r}"
Issue-Commit: d3d6261999f9a11a24fded61323b830533f5b9d0

# lambda__s3: the bundled in-runtime AWS SDK binds no S3 CreateBucket/DeleteBucket in either the Python or the Node.js runtime, so an in-handler bucket lifecycle always fails

> _Note: this issue was identified against the code base at commit `d3d6261999f9a11a24fded61323b830533f5b9d0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
2 net-new causes in lambda__s3. Both are the in-runtime bundled AWS SDK refusing S3 bucket-lifecycle operations, but they are two separate runtime shims with separate operation tables and separate error shapes, so they are numbered separately. (Related open issues 20261111T040849Z-lambda-s3-the-lambda-python-runtime-s-bundled-aws-sdk.md, 20261111T040849Z-lambda-s3-the-lambda-node-js-runtime-s-bundled-sdk.md and 20261111T142325Z-lambda-s3-the-python-runtime-s-bundled-sdk.md cover other operations of the same shims; neither carries these two checks' replay keys.)

Cause 1 of 2: the Python runtime's bundled SDK binds no S3 CreateBucket (nor DeleteBucket), so an in-handler bucket lifecycle raises an invented SdkCallNotMade.
Failing check ids: lambda__s3.function-sdk-creates-heads-lists-and-deletes-a-bucket
AWS: a function on the python3.12 managed runtime has boto3/botocore bundled, and that SDK is generated from the full Amazon S3 model — every operation the service models is callable, including CreateBucket, HeadBucket, ListBuckets and DeleteBucket (Smithy: s3 binds CreateBucket -> CreateBucketOutput with errors BucketAlreadyExists/BucketAlreadyOwnedByYou, and DeleteBucket -> no output). An in-handler s3.create_bucket(Bucket=...) therefore reaches S3 over the wire and either succeeds or fails with an S3-modeled error code; there is no SDK-level notion of an "unbound" operation and no AWS error type named SdkCallNotMade.
Emulator: the handler's first s3.create_bucket raises, from inside the bundled client itself (client.py _raise_refusal -> SdkCallNotMade), before any request is sent, with errorType "SdkCallNotMade" and message "the AWS SDK this runtime supplies binds no CreateBucket call on Amazon S3; the operations it binds are GetObject, HeadObject, PutObject, CopyObject, DeleteObject, ListObjectsV2, GetObjectTagging, PutObjectTagging, DeleteObjectTagging, HeadBucket, ListBuckets, GetBucketNotificationConfiguration, PutBucketNotificationConfiguration, GetBucketTagging, PutBucketTagging, ListObjectVersions, DeleteObjects". Invoke returns FunctionError "Unhandled". That enumerated list is the shim's hard-coded operation table; CreateBucket and DeleteBucket are both absent from it.
Why: any function code that provisions or tears down a bucket — the common pattern behind deployment helpers, CloudFormation custom resources and test fixtures — cannot run at all. It fails with an error type and message that exist nowhere in AWS, so a client cannot even classify it: there is no S3 error code to retry on and no way to distinguish it from a genuine authorization or naming failure.
Fix: the Lambda runtime's bundled in-process S3 SDK shim for the Python managed runtimes (its operation table and the _raise_refusal path in the shim's client module). CreateBucket and DeleteBucket must be bound and dispatched to the S3 endpoint like the operations already in the table.
Traces: lambda__s3.function-sdk-creates-heads-lists-and-deletes-a-bucket

Cause 2 of 2: the Node.js runtime's bundled @aws-sdk/client-s3 binds no CreateBucket/DeleteBucket, and refuses it as UnrecognizedClientException.
Failing check ids: lambda__s3.nodejs-function-sdk-creates-lists-and-deletes-a-bucket
AWS: a function on a managed nodejs runtime has @aws-sdk/client-s3 bundled, generated from the full S3 model, so CreateBucketCommand/DeleteBucketCommand are exported and callable; they reach S3 and fail only with S3-modeled errors. UnrecognizedClientException is the STS/credential error AWS returns when the access key in the signature is not recognized (Smithy: it is not an S3 error and is never raised client-side); it has nothing to do with which commands a client exports.
Emulator: the handler's CreateBucketCommand rejects inside the bundled client (notMade in /var/runtime/node_modules/@aws-sdk/client-s3) with errorType "UnrecognizedClientException" and the same "binds no CreateBucket call on Amazon S3; the operations it binds are ..." message and the same 17-operation table as Cause 1. Invoke returns FunctionError "Unhandled".
Why: as Cause 1, plus the refusal is mislabelled as a credential error. A client that retries or re-resolves credentials on UnrecognizedClientException loops forever against a condition no credential change can fix, and an operator reading the function's logs is pointed at authentication rather than at a missing SDK binding.
Fix: the Lambda runtime's bundled in-process S3 SDK shim for the Node.js managed runtimes (its command table and the notMade path). CreateBucket and DeleteBucket must be bound; and nothing in this shim should ever surface UnrecognizedClientException, which is not an S3-side classification.
Traces: lambda__s3.nodejs-function-sdk-creates-lists-and-deletes-a-bucket

## Evidence
Cause 1 — lambda__s3.function-sdk-creates-heads-lists-and-deletes-a-bucket (python3.12):
Expected: Invoke succeeds with no FunctionError; the handler's create_bucket / head_bucket / list_buckets / delete_bucket sequence completes, each call either succeeding or failing with an S3-modeled error code.
Actual: FunctionError "Unhandled" with payload
  errorType: 'SdkCallNotMade'
  errorMessage: 'the AWS SDK this runtime supplies binds no CreateBucket call on Amazon S3; the operations it binds are GetObject, HeadObject, PutObject, CopyObject, DeleteObject, ListObjectsV2, GetObjectTagging, PutObjectTagging, DeleteObjectTagging, HeadBucket, ListBuckets, GetBucketNotificationConfiguration, PutBucketNotificationConfiguration, GetBucketTagging, PutBucketTagging, ListObjectVersions, DeleteObjects'
  stackTrace: index.py line 25 in handler -> client.py line 354 in method -> client.py line 489 in _call -> client.py line 566 in _raise_refusal
Replay assertion: AssertionError: the lifecycle function failed: 'Unhandled' {...}

Cause 2 — lambda__s3.nodejs-function-sdk-creates-lists-and-deletes-a-bucket:
Expected: as above, for the Node.js runtime.
Actual: FunctionError "Unhandled" with payload
  errorType: 'UnrecognizedClientException'
  errorMessage: (identical "binds no CreateBucket call on Amazon S3; the operations it binds are ..." text and identical 17-operation list)
  trace: 'at notMade (/var/runtime/node_modules/@aws-sdk/client-s3:247:11)' ... 'at handler (file:///var/task/index.mjs:14:70)'
Replay assertion: AssertionError: the lifecycle function failed: 'Unhandled' {...}

Note on the message text: neither "SdkCallNotMade" nor this "binds no <Op> call on Amazon S3" wording exists in AWS. There is no correct wording to reproduce here — the operations must be bound so no such message is produced. The fixed text above is the shim's own; the operation name (CreateBucket) and the enumerated table are the variable parts it fills in.

## Steps to Reproduce
Cause 1:
1. Create a Lambda function on runtime python3.12 whose handler, using the runtime's bundled boto3, calls s3.create_bucket for a fresh bucket name (with CreateBucketConfiguration.LocationConstraint outside us-east-1), then head_bucket, then list_buckets, then delete_bucket.
2. Invoke the function.
3. Expected: no FunctionError; the lifecycle completes. Observed: FunctionError "Unhandled", errorType SdkCallNotMade, raised inside the bundled client before any request leaves the sandbox.

Cause 2:
1. Create a Lambda function on a managed nodejs runtime whose handler imports CreateBucketCommand/ListBucketsCommand/DeleteBucketCommand from the runtime's bundled @aws-sdk/client-s3 and runs the same create/list/delete sequence.
2. Invoke the function.
3. Expected: no FunctionError. Observed: FunctionError "Unhandled", errorType UnrecognizedClientException, raised by notMade inside the bundled client.
