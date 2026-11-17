Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-runtime.async-invoke-output-retention-window
Issue-Shape-Id: com.amazonaws.bedrockruntime#AmazonBedrockFrontendService
Issue-Key: bedrock-runtime.async-invoke-output-retention-window|run#10|Timeout|time.sleep(2)
Issue-Commit: 1cf5471edbe600272181c2f2fbc34afdc114dc10

# bedrock-runtime async invoke: an accepted job never reaches a terminal status, so GetAsyncInvoke polling never ends

> _Note: this issue was identified against the code base at commit `1cf5471edbe600272181c2f2fbc34afdc114dc10`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetAsyncInvoke

## Description
One net-new root cause in the bedrock-runtime asynchronous-invocation surface. A fixer must close 1 cause before submitting.

Related open issue (NOT a duplicate, and it does not cover this check's replay key): 20261024T094806Z-bedrock-runtime-async-invoke-startasyncinvoke-over-validates-modelinput.md. Its Cause 4 describes the emulator answering GetAsyncInvoke with an IMMEDIATE terminal status='Failed' plus an emulator-authored failureMessage. The behaviour observed in this replay is different and is not covered by that recorded key: the job is accepted and then never goes terminal at all, so a poll loop never terminates. A fixer closing that issue's Cause 4 by making the job terminate as 'Failed' would still leave this check hanging, and a fixer closing this one must make the job reach 'Completed'.

Cause 1 of 1: an accepted async invocation stays non-terminal indefinitely — GetAsyncInvoke never returns Completed (nor any terminal status), so a client's poll-to-terminal loop runs until it is killed.
AWS: Smithy (services/service-com-amazonaws-bedrockruntime-amazonbedrockfrontendservice/smithy-model/com-amazonaws-bedrockruntime-amazonbedrockfrontendservice.json): StartAsyncInvoke POST /async-invoke -> 200 returning a required invocationArn (pattern ^arn:aws(-[^:]+)?:bedrock:[a-z0-9-]{1,20}:[0-9]{12}:async-invoke/[a-z0-9]{12}$); GetAsyncInvoke GET /async-invoke/{invocationArn} -> 200 with required status of enum AsyncInvokeStatus = InProgress|Completed|Failed, required submitTime and outputDataConfig, optional lastModifiedTime/endTime/failureMessage. The enum has no non-terminal state other than InProgress, i.e. an accepted invocation is REQUIRED to converge to Completed or Failed; AWS's recorded behaviour for an accepted luma.ray-v2:0 video job is InProgress -> Completed within the job's render time (recorded wall clock on the order of a minute), with the invocation's artifacts written under the configured outputDataConfig.s3OutputDataConfig.s3Uri and failureMessage absent on that path. Retention is undocumented, so the only requirement after completion is that the record stays resolvable with an unchanged s3Uri and the artifact stays present (recorded behaviour only).
Emulator: the job is accepted (StartAsyncInvoke returns an invocationArn) and GetAsyncInvoke then keeps answering without ever reaching a terminal status, so the check's poll-to-terminal loop makes no progress and the check is killed with 'Check timed out after 60.0s'. The emulator does no model inference, so it has nothing to wait for — convergence is unbounded rather than merely slow, and the post-completion assertions (record still resolvable, s3Uri unchanged, artifact present under the prefix) are never reached because the Completed precondition is never met.
Why: every client of the async surface is a poll loop. A caller that waits for Completed (or for any terminal status) blocks forever against the emulator while succeeding on AWS: it never gets an output location it can read, never learns the job failed, and never times out on a modelled condition — it hangs or is killed by its own watchdog. Nothing downstream of a terminal status (output retention, terminal-state immutability, endTime ordering, ListAsyncInvokes status partitioning, the server-side S3 write) is observable at all.
Fix: the bedrock-runtime async-invoke job state machine and the worker that advances a minted job — it must drive an accepted invocation to a terminal AsyncInvokeStatus promptly and, on the success path, set status='Completed' with endTime/lastModifiedTime and write the invocation's output under the configured s3Uri before reporting Completed. A synthetic artifact is sufficient; the status enum reaching a terminal value, the unchanged s3Uri and the presence of a non-empty object under that prefix are the requirement, not the artifact's bytes. Its sibling read path (GetAsyncInvoke's serializer) is not implicated — the status value it serves is.
Traces: bedrock-runtime.async-invoke-output-retention-window.

## Evidence
Check bedrock-runtime.async-invoke-output-retention-window (service heading '### bedrock-runtime'), verdict mismatch.

Sequence: s3.create_bucket(<out-bucket>) -> bedrock-runtime.start_async_invoke(modelId='luma.ray-v2:0', modelInput={'prompt': <one sentence>, 'aspect_ratio':'16:9','loop':False,'duration':'5s','resolution':'540p'}, outputDataConfig={'s3OutputDataConfig':{'s3Uri':'s3://<out-bucket>/'}}) -> poll get_async_invoke(invocationArn=<minted arn>) until status != 'InProgress'.

Expected (AWS): GetAsyncInvoke.status converges to the terminal enum value 'Completed'; outputDataConfig.s3OutputDataConfig.s3Uri is returned unchanged on every subsequent read; at least one non-empty object exists under the key prefix of that s3Uri, and both remain true across a ~60 s re-read window.

Actual (emulator): no terminal status is ever served. StartAsyncInvoke is accepted (so this is not the synchronous ValidationException path of the related open issue), and the poll loop never leaves its non-terminal branch; the check is terminated by the harness with:
  Error: Check timed out after 60.0s
No assertion after the Completed gate is ever evaluated, so the s3Uri stability and artifact-presence requirements are unverifiable against the emulator.

## Steps to Reproduce
1. git_clone, then run_behavioral_suite against the booted emulator.
2. s3.create_bucket on a fresh output bucket in the emulator's region (add CreateBucketConfiguration.LocationConstraint when the region is not us-east-1).
3. bedrock-runtime.start_async_invoke(modelId='luma.ray-v2:0', modelInput={'prompt':'Closeup of a cute old steampunk robot. Camera zoom in.','aspect_ratio':'16:9','loop':False,'duration':'5s','resolution':'540p'}, outputDataConfig={'s3OutputDataConfig':{'s3Uri':'s3://<out-bucket>/'}}); note the returned invocationArn.
4. Loop: bedrock-runtime.get_async_invoke(invocationArn=<arn>) every 15 s and print status. Observe that status never becomes 'Completed' or 'Failed' — the loop runs until it is killed (the behavioral harness kills it at 60 s).
5. s3.list_objects_v2(Bucket=<out-bucket>, Prefix=<key prefix of the returned s3Uri>) — observe no non-empty object is ever written.
6. Expected instead: step 4 reaches status='Completed' (with endTime set and the same s3Uri), step 5 lists a non-empty object, and repeating steps 4-5 for ~60 s keeps both stable.
