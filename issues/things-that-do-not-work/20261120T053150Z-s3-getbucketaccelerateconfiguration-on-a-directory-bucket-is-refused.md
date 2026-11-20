Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3.put-bucket-accelerate-configuration-directory-bucket-rejected
Issue-Shape-Id: com.amazonaws.s3#AmazonS3
Issue-Key: s3.put-bucket-accelerate-configuration-directory-bucket-rejected|run#7|AssertionError|assert unset_code == "MethodNotAllowed", (
Issue-Commit: 07a81b42592fdd972bbc8a5b9a2141006a4e2532

# s3: GetBucketAccelerateConfiguration on a directory bucket is refused 501 NotImplemented instead of 405 MethodNotAllowed

> _Note: this issue was identified against the code base at commit `07a81b42592fdd972bbc8a5b9a2141006a4e2532`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetBucketAccelerateConfiguration

## Description
One net-new root cause in the S3 Express One Zone (directory bucket) refusal path. The sibling cause on the same check — PutBucketAccelerateConfiguration being ACCEPTED on a directory bucket — no longer reproduces: the Put now correctly answers ('InvalidArgument', 'AccelerateConfiguration is not valid, expected CreateBucketConfiguration') and the check gets past that assertion. The check now fails one step later, on the GET of the same subresource, which is a different refusal classification and is not carried by any open issue's replay key. Open issue 20261117T143723Z-s3-8-net-new-data-plane-and-validation.md (Cause 7) names this same check but was filed for the Put-accepted divergence (its Issue-Key for the check is `..._error#3|AssertionError|raise AssertionError(... but it succeeded ...)`); it does not cover the Get refusal class, which is what fails now.

Cause 1 of 1: GetBucketAccelerateConfiguration against an S3 Express One Zone directory bucket is refused with the wrong code, the wrong HTTP status and the wrong message.

AWS: Smithy (s3) binds GetBucketAccelerateConfiguration at `GET /{Bucket}?accelerate` with success code 200, input Bucket (@httpLabel, required), ExpectedBucketOwner and RequestPayer, output Status (BucketAccelerateStatus: Enabled | Suspended) and RequestCharged; the model declares NO errors for the operation (errors: []). The operation's own modeled documentation opens with "This operation is not supported for directory buckets." Transfer Acceleration is not a plane a directory bucket has at all, so the Zonal endpoint does not serve the `?accelerate` sub-resource method: recorded behavior is HTTP 405 with Error.Code `MethodNotAllowed` and Error.Message "The specified method is not allowed against this resource." That message is entirely FIXED text — it has no variable parts: no bucket name, no key, no operation name and no feature name is interpolated into it. (The emulator's own Put-side refusal, which this check asserts first and which passes, confirms the pairing: AWS refuses the Put of the same sub-resource with ('InvalidArgument', 'AccelerateConfiguration is not valid, expected CreateBucketConfiguration'), i.e. the two directions of one unsupported sub-resource are refused in two different, specific classes — never in a generic "not implemented" class.)

Emulator: the call is refused with Error.Code `NotImplemented` (HTTP 501) and the message "This bucket does not support Transfer Acceleration." — the generic unsupported-feature sentence this service builds for every catalogued directory-bucket operation. Replay output: `AssertionError: expected GetBucketAccelerateConfiguration to be refused, got 'NotImplemented': 'This bucket does not support Transfer Acceleration.'`. Neither the code, the status nor the sentence is the one AWS answers; the message is in a wording family ("This bucket does not support <feature>.") that AWS uses for the object-tags plane, not for an unsupported sub-resource METHOD on a directory bucket.

Why: a client that branches on the refusal takes the wrong remediation. `MethodNotAllowed`/405 says "this resource kind does not serve this method — do not call it for a directory bucket", a permanent, caller-correctable fact; `NotImplemented`/501 is the class SDKs, CLIs and retry policies read as "the service or emulator has not built this yet", which callers log as an infrastructure gap and often retry. Code written against the emulator will branch on NotImplemented and then see MethodNotAllowed in AWS (and vice versa), and a 501 also mis-signals a server fault rather than a client-side misuse of a bucket class.

Fix: the directory-bucket refusal classifier, not the operation handler. `services/service-com-amazonaws-s3-amazons3/src/service/directory_bucket.rs` routes every entry of `UNSUPPORTED_BY_A_DIRECTORY_BUCKET` through `RefuseUnsupportedOperation::refusal`, which chooses `crate::state::INVALID_ARGUMENT_CODE`/400 + `not_a_create_bucket_configuration_message` only when the request submits a bucket-configuration document, and otherwise falls through to `crate::state::NOT_IMPLEMENTED_CODE`/`NOT_IMPLEMENTED_STATUS` + `unsupported_message(feature)` — so every catalogued READ, including the `?accelerate` GET, gets the 501 feature sentence. The read of an unsupported sub-resource needs its own branch answering S3's `MethodNotAllowed` (405) with that code's fixed sentence. `services/service-com-amazonaws-s3-amazons3/src/ops/get_bucket_accelerate_configuration.rs` is never reached for a directory bucket and does not need to change.

Traces: s3.put-bucket-accelerate-configuration-directory-bucket-rejected (the only failing check grounding this issue; its Put-side and general-purpose-bucket positive-control assertions pass).

## Evidence
Expected (AWS): GetBucketAccelerateConfiguration on an S3 Express One Zone directory bucket answers HTTP 405 with Error.Code 'MethodNotAllowed' and Error.Message 'The specified method is not allowed against this resource.' (wholly fixed text — no request or state values interpolated).

Actual (emulator): HTTP 501 with Error.Code 'NotImplemented' and Error.Message 'This bucket does not support Transfer Acceleration.'

Replay detail, verbatim:
  File "check_s3_put_bucket_accelerate_configuration_directory_bucket_rejected.py", line 96, in run
    assert unset_code == "MethodNotAllowed", (
  AssertionError: expected GetBucketAccelerateConfiguration to be refused, got 'NotImplemented': 'This bucket does not support Transfer Acceleration.'

Passing context in the same check (so the divergence is isolated to the GET): PutBucketAccelerateConfiguration on the same directory bucket is already refused with the AWS-correct ('InvalidArgument', 'AccelerateConfiguration is not valid, expected CreateBucketConfiguration'), and the positive control — PutBucketAccelerateConfiguration Status=Enabled on a general purpose bucket, read back as Enabled — also passes.

Source of the wrong class: services/service-com-amazonaws-s3-amazons3/src/service/directory_bucket.rs — UNSUPPORTED_BY_A_DIRECTORY_BUCKET line 79 maps ('GetBucketAccelerateConfiguration', '@directory-bucket-unsupported-GetBucketAccelerateConfiguration', ACCELERATION); RefuseUnsupportedOperation::refusal (lines 357-376) answers crate::state::NOT_IMPLEMENTED_CODE / NOT_IMPLEMENTED_STATUS with unsupported_message(self.feature) (line 308: format!("This bucket does not support {feature}.")) for any catalogued call that submits no configuration document, which is every read.

Model citation: smithy_query(query="operation", operation="GetBucketAccelerateConfiguration", model_path="services/service-com-amazonaws-s3-amazons3/smithy-model/com-amazonaws-s3-amazons3.json") -> http {method: GET, uri: "/{Bucket}?accelerate", code: 200}, errors: [], output {Status: BucketAccelerateStatus, RequestCharged}; operation documentation begins "This operation is not supported for directory buckets."

## Steps to Reproduce
1. Create an S3 Express One Zone directory bucket in the region under test, e.g. CreateBucket with Bucket='<name>--<az-id>--x-s3' and CreateBucketConfiguration={'Location': {'Type': 'AvailabilityZone', 'Name': '<az-id>'}, 'Bucket': {'DataRedundancy': 'SingleAvailabilityZone', 'Type': 'Directory'}}, using an s3 client configured with s3={'disable_s3_express_session_auth': True} (AWS serves a directory bucket's control calls without an S3 Express session).
2. Call PutBucketAccelerateConfiguration(Bucket=<that directory bucket>, AccelerateConfiguration={'Status': 'Enabled'}) and observe it is refused ('InvalidArgument', 'AccelerateConfiguration is not valid, expected CreateBucketConfiguration') — this part already matches AWS.
3. Call GetBucketAccelerateConfiguration(Bucket=<that directory bucket>).
   Expected: ClientError with HTTP status 405, Error.Code 'MethodNotAllowed', Error.Message 'The specified method is not allowed against this resource.'
   Observed: ClientError with HTTP status 501, Error.Code 'NotImplemented', Error.Message 'This bucket does not support Transfer Acceleration.'
4. Positive control (should keep passing): create a general purpose bucket, PutBucketAccelerateConfiguration Status='Enabled' on it, then GetBucketAccelerateConfiguration and confirm Status == 'Enabled'.

Replay the grounding check: s3.put-bucket-accelerate-configuration-directory-bucket-rejected
