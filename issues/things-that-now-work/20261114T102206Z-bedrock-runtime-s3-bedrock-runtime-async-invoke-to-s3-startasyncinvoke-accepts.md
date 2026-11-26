Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-runtime__s3.bedrock-runtime-async-invoke-with-a-nonexistent-output-bucket-rejected bedrock-runtime__s3.bedrock-runtime-async-invoke-writes-a-playable-video-artifact-to-s3
Issue-Key: bedrock-runtime__s3.bedrock-runtime-async-invoke-with-a-nonexistent-output-bucket-rejected|_error_code#18|AssertionError|raise AssertionError(
Issue-Key: bedrock-runtime__s3.bedrock-runtime-async-invoke-writes-a-playable-video-artifact-to-s3|run#23|AssertionError|assert written_uri == f"{requested_uri}{invocation_id}", (
Issue-Commit: 2b46cf3df3e6c8d83849f7668fee251a8b60f8ab

# bedrock-runtime async invoke to S3: StartAsyncInvoke accepts an output bucket that does not exist, and the recorded s3Uri never gets the minted invocation id appended (2 causes)

> _Note: this issue was identified against the code base at commit `2b46cf3df3e6c8d83849f7668fee251a8b60f8ab`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartAsyncInvoke

## Description
This issue bundles 2 distinct net-new root causes in the bedrock-runtime asynchronous-invocation surface as it is reached from S3. A fixer must close BOTH causes before submitting.

Smithy consulted (services/service-com-amazonaws-bedrockruntime-amazonbedrockfrontendservice/smithy-model/com-amazonaws-bedrockruntime-amazonbedrockfrontendservice.json): StartAsyncInvoke POST /async-invoke -> 200, @idempotent; input modelId / modelInput (document, @sensitive) / outputDataConfig all required; errors AccessDeniedException 403, ConflictException 400, InternalServerException 500, ResourceNotFoundException 404, ServiceQuotaExceededException 400, ServiceUnavailableException 503, ThrottlingException 429, ValidationException 400. GetAsyncInvoke GET /async-invoke/{invocationArn} -> 200, with required status (enum AsyncInvokeStatus) and required outputDataConfig; InvocationArn pattern ^arn:aws(-[^:]+)?:bedrock:[a-z0-9-]{1,20}:[0-9]{12}:async-invoke/[a-z0-9]{12}$; AsyncInvokeS3OutputDataConfig.s3Uri is documented only as "An object URI starting with s3://", so the model states nothing about destination validation or URI rewriting.

Relationship to what is already filed: the open issue 20261024T094806Z-bedrock-runtime-async-invoke-startasyncinvoke-over-validates-modelinput.md lists both of these check ids, but the replay keys it carries are the `job["status"] == "Completed"` assertions. The emulator now does advance an accepted invocation to Completed, and both checks fail further downstream on assertions that issue does not carry. The two causes below are what is left behind, and neither was marked ALREADY FILED or OWNED by the replay report.

---
Cause 1 of 2: StartAsyncInvoke accepts an outputDataConfig whose S3 bucket does not exist, instead of refusing the submission up front.
AWS: the destination is validated at submit time, before any job is minted, and the call is refused with HTTP 400 ValidationException (modeled, httpError 400). The message is the fixed sentence "Invalid S3 credentials" — a missing output bucket is reported with the same message as unwritable S3 credentials, there is no dedicated "bucket not found" wording, and the message carries no variable part at all (message text: recorded behavior only; the model types outputDataConfig but documents no validation rule). No invocation exists afterwards, so ListAsyncInvokes reports nothing whose outputDataConfig.s3OutputDataConfig.s3Uri names that bucket.
Emulator: the identical request naming a syntactically valid, absent bucket is ACCEPTED with HTTP 200 and a minted invocationArn, and the invocation is recorded against the absent destination. The absence of the bucket is first established in the same run by GetBucketLocation answering NoSuchBucket, so this is not a stale-name race.
Why: a caller is billed for, and must poll, work whose artifact can never be delivered; the request that AWS rejects synchronously with a correctable 400 instead returns a success the client treats as accepted work. Any client that relies on the up-front refusal to catch a typo'd or not-yet-created bucket discovers the problem only as an undeliverable artifact, and a caller cannot distinguish "destination unusable" from "job running".
Fix: the StartAsyncInvoke handler's validation stage — resolve and authorize outputDataConfig.s3OutputDataConfig's bucket before minting an invocation, and raise the modeled ValidationException when it does not resolve.
Traces: bedrock-runtime__s3.bedrock-runtime-async-invoke-with-a-nonexistent-output-bucket-rejected.

---
Cause 2 of 2: the output URI recorded on an invocation is the caller's requested prefix verbatim; the service-minted invocation id is never appended.
AWS: recorded behavior only (the model documents s3Uri only as an object URI). The output location is DERIVED, not echoed: GetAsyncInvoke and ListAsyncInvokes both report outputDataConfig.s3OutputDataConfig.s3Uri as the requested s3Uri with the minted invocation id appended — i.e. <requested s3Uri><invocation id>, where <invocation id> is the final segment of the returned invocationArn (12 lowercase alphanumerics, per the InvocationArn pattern above). The artifacts are written under that same per-invocation prefix: exactly one .mp4 (an ISO base media file whose bytes 4..8 are the 'ftyp' box) and exactly one manifest.json carrying the invocation's requestId. The requirement is the rule "requested prefix + minted invocation id", never one run's literal id.
Emulator: GetAsyncInvoke reports s3Uri equal to the requested prefix unchanged, with no invocation-id segment, so the recorded destination for every invocation into the same prefix is identical and a reader cannot tell which invocation wrote what.
Why: a client that locates an async artifact from the record the service returns (read s3Uri, then list under it) is pointed at the shared caller prefix rather than the invocation's own prefix, so concurrent invocations into one prefix are indistinguishable and the per-invocation artifact cannot be addressed. It also makes GetAsyncInvoke and ListAsyncInvokes disagree with the layout under which the artifact is actually delivered.
Fix: the async-invoke state's output-location derivation plus the GetAsyncInvoke / ListAsyncInvokes serializers — record and report the derived per-invocation URI, and write the artifact and manifest under it.
Traces: bedrock-runtime__s3.bedrock-runtime-async-invoke-writes-a-playable-video-artifact-to-s3.

## Evidence
Cause 1 — StartAsyncInvoke(modelId='luma.ray-v2:0', modelInput={prompt,aspect_ratio,loop,duration,resolution}, outputDataConfig={'s3OutputDataConfig':{'s3Uri':'s3://<absent-bucket>/video/'}}), after GetBucketLocation on <absent-bucket> answered NoSuchBucket and after the control invocation into an existing bucket reached Completed.
  Expected (AWS): HTTP 400, Error.Code 'ValidationException', Error.Message 'Invalid S3 credentials'; no invocation minted, and ListAsyncInvokes reports no invocation whose s3Uri names <absent-bucket>.
  Actual (emulator): HTTP 200 — AssertionError: "StartAsyncInvoke was ACCEPTED against a bucket that does not exist. ... response={'ResponseMetadata': {'HTTPStatusCode': 200, ...}, 'invocationArn': 'arn:aws:bedrock:us-west-2:000000000000:async-invoke/c77df1465434'}". (The request id, account and invocation id are per-run values; the requirement is the 400/ValidationException/'Invalid S3 credentials' refusal and the absence of a minted invocation.)

Cause 2 — GetAsyncInvoke(invocationArn=<arn returned for a request whose outputDataConfig.s3Uri was 's3://<bucket>/video/'>), polled to status 'Completed'.
  Expected (AWS): outputDataConfig.s3OutputDataConfig.s3Uri == 's3://<bucket>/video/' + <invocation id>, i.e. the requested prefix with the final ARN segment appended; and under that prefix exactly one .mp4 (bytes 4..8 == b'ftyp', ContentLength > 10240) plus exactly one manifest.json carrying a non-empty requestId, with ListAsyncInvokes reporting the same derived s3Uri.
  Actual (emulator): AssertionError: "the recorded output URI must be the requested prefix with the service-minted invocation id appended; requested 's3://sim2real-async-out-c120176fa7d3/video/' invocation '453dcc37c335' recorded 's3://sim2real-async-out-c120176fa7d3/video/'" — the recorded URI is the requested prefix verbatim. (Bucket name and invocation id are per-run values; the requirement is the derivation rule.)

## Steps to Reproduce
1. git_clone, then run_behavioral_suite against the booted emulator.
2. Cause 1: s3.create_bucket(<out-bucket>); pick a second syntactically valid bucket name and confirm s3.get_bucket_location on it raises NoSuchBucket; start and poll a control bedrock-runtime.start_async_invoke(modelId='luma.ray-v2:0', modelInput={'prompt':'A single blue cube rotating slowly on a white background.','aspect_ratio':'16:9','loop':False,'duration':'5s','resolution':'540p'}, outputDataConfig={'s3OutputDataConfig':{'s3Uri':'s3://<out-bucket>/video/'}}) to Completed; then repeat the identical call with s3Uri='s3://<absent-bucket>/video/'. Observe HTTP 200 with a minted invocationArn instead of 400 ValidationException 'Invalid S3 credentials', and observe the invocation appearing in list_async_invokes against the absent bucket.
3. Cause 2: from the accepted control invocation, take invocation_id = invocationArn.rsplit('/',1)[-1] and read get_async_invoke(invocationArn=...)['outputDataConfig']['s3OutputDataConfig']['s3Uri']. Observe it equals 's3://<out-bucket>/video/' rather than 's3://<out-bucket>/video/'+invocation_id, and that list_objects_v2(Bucket=<out-bucket>, Prefix='video/'+invocation_id+'/') is empty.
4. Replay the two check ids in trace_ids to confirm a fix.
