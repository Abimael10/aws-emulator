Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.arbitrary-code-executes lambda.dry-run-does-not-invoke-handler lambda.invoke-code-artifact-state-errors lambda.invoke-nonexistent lambda.invoke-with-response-stream-ec2-errors lambda.nodejs-esm-named-module-handler-digests-nested-events lambda.nodejs-runtime-executes lambda.python-named-module-handler-digests-a-nested-unicode-event-on-arm64 lambda.deep-lambda-3-runtime-pin-foreign-runtime-arn-rejected lambda.deep-lambda-3-runtime-pin-manual-arn-survives-cold-start lambda.init-past-the-init-limit-is-retried-inside-the-invoke-and-timed-out-rejected lambda.invoke-without-tenant-id-on-tenant-isolated-function-rejected lambda.log-lambda-report-log-level-with-text-format-rejected lambda.log-lambda-report-start-end-report-lines-bracket-the-invoke-request-id lambda.log-lambda-report-new-environment-opens-new-stream-warm-reuse-shares-one lambda.log-lambda-report-unhandled-exception-error-line-matches-function-error-and-tail lambda.msk-esm-role-without-describe-cluster-rejected-before-the-cluster-is-looked-up lambda.thirty-way-concurrency-creates-thirty-distinct-environments
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda.dry-run-does-not-invoke-handler|run#28|AssertionError|assert nf_msg == f"Function not found: {absent_arn}:$LATEST", f"not-found message: {nf_msg!r}"
Issue-Key: lambda.invoke-code-artifact-state-errors|run#15|AssertionError|assert message == expected_invoke_message, (
Issue-Key: lambda.invoke-nonexistent|run#9|AssertionError|assert message == expected, f"expected message {expected!r}, got {message!r}"
Issue-Key: lambda.invoke-with-response-stream-ec2-errors|run#8|AssertionError|assert missing_msg == expected_missing, f"expected message {expected_missing!r}, got {missing_msg!r}"
Issue-Key: lambda.python-named-module-handler-digests-a-nested-unicode-event-on-arm64|run#22|AssertionError|assert got == gone_invoke, f"Invoke on the deleted function: expected {gone_invoke}, got {got!r}"
Issue-Key: lambda.deep-lambda-3-runtime-pin-foreign-runtime-arn-rejected|_init_start_before#10|AssertionError|assert at > 0, f"START for {rid} is the first line of {holders[0]}; no INIT_START precedes it"
Issue-Key: lambda.deep-lambda-3-runtime-pin-manual-arn-survives-cold-start|_init_start_before#10|AssertionError|assert at > 0, f"START for {rid} is the first line of {holders[0]}; no INIT_START precedes it"
Issue-Key: lambda.init-past-the-init-limit-is-retried-inside-the-invoke-and-timed-out-rejected|run#10|AssertionError|assert resp.get("FunctionError") == "Unhandled", (
Issue-Key: lambda.invoke-without-tenant-id-on-tenant-isolated-function-rejected|run#6|AssertionError|assert message == (
Issue-Key: lambda.log-lambda-report-log-level-with-text-format-rejected|_report_of#18|AssertionError|assert m, f"REPORT line does not have the documented field layout: {lines[0]!r}"
Issue-Key: lambda.log-lambda-report-start-end-report-lines-bracket-the-invoke-request-id|_report_of#21|AssertionError|assert m, f"REPORT line does not have the documented field layout: {lines[0]!r}"
Issue-Key: lambda.log-lambda-report-new-environment-opens-new-stream-warm-reuse-shares-one|run#16|AssertionError|assert len(own) == 1, (
Issue-Key: lambda.log-lambda-report-unhandled-exception-error-line-matches-function-error-and-tail|run#22|AssertionError|assert errors[0] == expected_error.rstrip("\n"), (
Issue-Key: lambda.msk-esm-role-without-describe-cluster-rejected-before-the-cluster-is-looked-up|_error#14|AssertionError|raise AssertionError(f"expected a ClientError, the call succeeded: {out!r}")
Issue-Key: lambda.thirty-way-concurrency-creates-thirty-distinct-environments|run#17|AssertionError|assert len(envs1) == WIDTH, (
Issue-Related: 20261118T165433Z-apigateway-dynamodb-lambda-logs-lambda-invoke-s-resourcenotfoundexception-message-drops-the-resolved.md
Issue-Commit: 6ad539f79015bc27d2d0777d4c5c145ae7d45c50

# lambda: 9 causes — Invoke's not-found message drops ':$LATEST', no INIT_START record precedes START, the init limit is never enforced, the tenant-isolation refusal is misworded, the REPORT line's field layout is wrong, one warm environment opens a new log stream per invoke, the [ERROR] record omits the traceback, an MSK ESM for an unreachable cluster is accepted, and 30 concurrent invokes share 4 environments

> _Note: this issue was identified against the code base at commit `6ad539f79015bc27d2d0777d4c5c145ae7d45c50`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
Nine independent net-new root causes in the Lambda data plane, its execution environment and its log delivery. ALL NINE must be closed before this issue is submitted.

Cause 1 of 9: Invoke's 'Function not found' message names the UNQUALIFIED function ARN.
AWS: Invoke on a function that does not exist answers HTTP 404 ResourceNotFoundException (Smithy: Invoke -> ResourceNotFoundException, errorKind client, httpError 404; model services/service-com-amazonaws-lambda-awsgirapiservice/smithy-model/com-amazonaws-lambda-awsgirapiservice.json) with Message = fixed text 'Function not found: ' + the FULLY QUALIFIED ARN arn:<partition>:lambda:<region>:<account-id>:function:<name> plus the resolved qualifier — ':$LATEST' when the request carried no Qualifier, otherwise ':<version>'/':<alias>'. Variable parts come from the request and credential scope; the wording is recorded behavior only. The same message is returned by InvokeWithResponseStream.
Emulator: same status and code, Message 'Function not found: arn:aws:lambda:<region>:<account>:function:<name>' — no ':$LATEST'.
Why: any client comparing or parsing the refusal, or re-deriving the qualified ARN it invoked, gets a different resource identity than AWS reports and cannot see which qualifier was resolved.
Fix: the Invoke / InvokeWithResponseStream handlers' not-found path (and the shared 'Function not found: ' message helper) must append the resolved qualifier.
Failing check ids: lambda.arbitrary-code-executes, lambda.dry-run-does-not-invoke-handler, lambda.invoke-code-artifact-state-errors, lambda.invoke-nonexistent, lambda.invoke-with-response-stream-ec2-errors, lambda.nodejs-esm-named-module-handler-digests-nested-events, lambda.nodejs-runtime-executes, lambda.python-named-module-handler-digests-a-nested-unicode-event-on-arm64.

Cause 2 of 9: a cold invocation writes no INIT_START platform record before its START line.
AWS: on a cold start the execution environment writes the platform record 'INIT_START Runtime Version: <runtime>:<version> Runtime Version ARN: <arn>' into the function's log stream BEFORE the 'START RequestId: <rid> Version: <qualifier>' line, so the runtime version ARN actually used is discoverable per environment (Lambda developer guide, 'Runtime management' / 'Using Lambda runtime logs'). Recorded behavior only: log line formats are not in the Smithy model.
Emulator: the START line is the FIRST line of the cold environment's stream — no INIT_START precedes it, so the runtime version ARN the environment pinned cannot be read at all.
Why: runtime-management behavior (a manual runtime-version pin surviving a cold start, a foreign runtime ARN being refused) is only observable through INIT_START. A client auditing which runtime version its functions ran on has no record.
Fix: the execution-environment log emitter — emit the INIT_START record, naming the resolved runtime version and its ARN, as the first event of every cold environment's stream, ahead of START.
Failing check ids: lambda.deep-lambda-3-runtime-pin-foreign-runtime-arn-rejected, lambda.deep-lambda-3-runtime-pin-manual-arn-survives-cold-start.

Cause 3 of 9: module-level init that outlasts the 10 s init limit is not cut off, so the invoke succeeds where AWS times it out.
AWS: initialisation past the 10-second init limit is aborted and retried inside the first invocation, charged against the function's Timeout; that invoke answers 200 with header X-Amz-Function-Error 'Unhandled' and a payload whose errorType is 'Sandbox.Timedout' and whose errorMessage is the fixed text 'RequestId: <request id> Error: Task timed out after <Timeout>.00 seconds' (Lambda developer guide, 'Lambda execution environment'; the request id and the configured Timeout are the variable parts).
Emulator: a handler whose module body sleeps 12 s on a function with Timeout=3 returns FunctionError=None and the handler's own payload {'ok': True}.
Why: a client cannot discover that its initialisation is too slow — the failure mode AWS surfaces loudly (Sandbox.Timedout) is invisible, and the function appears healthy in the emulator and times out in production.
Fix: the execution-environment init path — bound init at the 10 s limit, retry it inside the invoke under the function's Timeout, and fail that invoke with FunctionError 'Unhandled' and the Sandbox.Timedout payload above.
Failing check ids: lambda.init-past-the-init-limit-is-retried-inside-the-invoke-and-timed-out-rejected.

Cause 4 of 9: the missing-TenantId refusal on a tenant-isolated function carries the wrong message text.
AWS: Invoke without a TenantId on a function whose TenancyConfig sets TenantIsolationMode PER_TENANT is refused with InvalidParameterValueException (Smithy: Invoke -> InvalidParameterValueException, errorKind client, httpError 400; TenantId is the X-Amz-Tenant-Id header member) and the fixed message 'The invoked function is enabled with tenancy configuration. Add a valid tenant ID in your request and try again.' (Lambda developer guide, tenant-isolation-invoke). The message has no variable parts.
Emulator: the refusal's message is 'TenantId is required if the function is configured for tenant isolation' — different wording for the same error class.
Why: a client matching on the documented message (or surfacing it to an operator) does not recognise the refusal; the wording is part of the error contract.
Fix: the Invoke handler's tenant-isolation validation — word the InvalidParameterValueException exactly as AWS does.
Failing check ids: lambda.invoke-without-tenant-id-on-tenant-isolated-function-rejected.

Cause 5 of 9: the REPORT platform log line does not have AWS's field layout.
AWS: the text-format REPORT line is 'REPORT RequestId: <rid>\tDuration: <d>.<dd> ms\tBilled Duration: <bd> ms\tMemory Size: <m> MB\tMax Memory Used: <u> MB\t' — EVERY field, including the last, is terminated by a TAB, and on a cold start the optional field 'Init Duration: <i>.<ii> ms\t' follows (Lambda developer guide, 'Using CloudWatch logs with Lambda'). Fixed text: the field labels and their order; variable parts: the request id and the measured durations/sizes.
Emulator: the line ends 'Max Memory Used: 1 MB\n' — the trailing TAB after the last field is missing (and no Init Duration field is present), so the documented layout does not parse.
Why: log consumers (CloudWatch metric filters, log parsers, the Lambda Insights style tooling that splits REPORT on tabs) are written against that exact framing; a line without the terminating tab silently fails to match and invocation metrics are lost.
Fix: the execution environment's REPORT line emitter — terminate every field with a tab, in the documented order, and include the cold-start Init Duration field.
Failing check ids: lambda.log-lambda-report-log-level-with-text-format-rejected, lambda.log-lambda-report-start-end-report-lines-bracket-the-invoke-request-id.

Cause 6 of 9: one warm execution environment writes each invocation to a DIFFERENT log stream.
AWS: a log stream is per execution environment, named '<YYYY>/<MM>/<DD>/[<qualifier>]<32 lowercase hex>'; every invocation served by the SAME warm environment (identical AWS_LAMBDA_LOG_STREAM_NAME) lands in that one stream, and a new stream appears only when a new environment is created (Lambda developer guide, 'Using CloudWatch logs with Lambda'). Recorded behavior only.
Emulator: three successive warm invocations of one function were spread over three distinct streams ('2026/11/18/[$LATEST]47be7061...', '...b64a976f...', '...b946983f...').
Why: a client cannot correlate an environment's invocations — reading one stream no longer shows the init output and the previous invocations of the same sandbox, and warm-reuse diagnosis (module state, /tmp reuse) through logs is impossible.
Fix: the log-stream allocation in the execution environment — bind the stream name to the environment's lifetime and reuse it for every invocation that environment serves.
Failing check ids: lambda.log-lambda-report-new-environment-opens-new-stream-warm-reuse-shares-one.

Cause 7 of 9: the [ERROR] log record for an unhandled exception omits the traceback framing.
AWS: an unhandled exception in a Python handler emits one log event '[ERROR] <ExceptionType>: <message>' followed by 'Traceback (most recent call last):' and the stack frames, framed exactly as the invoke response's stackTrace reports them (the frame lines are indented with non-breaking-space pairs, matching the response payload's stackTrace entries). The log record and the response payload must agree. Recorded behavior only.
Emulator: the event is just '[ERROR] ValueError: <message>\n' — no 'Traceback (most recent call last):' line and no frames, so the log disagrees with the stackTrace the same invoke returned.
Why: the log is the only record of a failure for asynchronous invocations; without the traceback a caller cannot locate the failing frame, and tooling that reconciles the [ERROR] event with the response's stackTrace finds a mismatch.
Fix: the runtime's unhandled-error logger — write the traceback block after the '[ERROR] <type>: <message>' head, from the same frames the error payload's stackTrace carries.
Failing check ids: lambda.log-lambda-report-unhandled-exception-error-line-matches-function-error-and-tail.

Cause 8 of 9: CreateEventSourceMapping accepts an MSK mapping whose execution role cannot describe the cluster (and whose cluster does not exist).
AWS: CreateEventSourceMapping for an Amazon MSK EventSourceArn is refused BEFORE the cluster is looked up when the function's execution role lacks the describe permissions: HTTP 400 InvalidParameterValueException (Smithy: CreateEventSourceMapping -> InvalidParameterValueException, errorKind client, httpError 400) with the message 'Cannot access cluster <cluster ARN>. Please ensure the role can perform the DescribeClusterV2 (recommended) or DescribeCluster and GetBootstrapBrokers actions on your cluster in IAM.' (fixed text apart from the cluster ARN echoed from the request). With only kafka:DescribeClusterV2 granted, the same request becomes 404 ResourceNotFoundException 'The cluster resource: <cluster ARN> you requested does not exist.' — and no mapping is left behind in either case (Lambda developer guide, with-msk-permissions).
Emulator: the call is answered HTTP 202 and a mapping is created (State 'Creating', EventSourceArn naming a cluster that does not exist, UUID and EventSourceMappingArn minted).
Why: the permission and existence preconditions AWS enforces at create time are gone. A caller gets a mapping that can never poll anything, discovers the missing IAM grant only from a silent non-delivery, and its negative-path handling never runs.
Fix: CreateEventSourceMapping's MSK validation — check the execution role's kafka:DescribeClusterV2 / DescribeCluster+GetBootstrapBrokers grants first and refuse with the InvalidParameterValueException above, then resolve the cluster and refuse an absent one with the ResourceNotFoundException wording, creating nothing in either case.
Failing check ids: lambda.msk-esm-role-without-describe-cluster-rejected-before-the-cluster-is-looked-up.

Cause 9 of 9: concurrent invocations are multiplexed onto a handful of execution environments.
AWS: simultaneous invocations cannot share an execution environment — each concurrent invoke is served by its own environment, so N truly simultaneous invokes of one function report N distinct environment identities (distinct AWS_LAMBDA_LOG_STREAM_NAME / per-environment uuid) (Lambda developer guide, 'Lambda execution environment' and 'Understanding Lambda scaling'). Recorded behavior only.
Emulator: 30 simultaneous invokes were served by 4 distinct environments, i.e. one environment answered several in-flight requests at once.
Why: the isolation guarantee callers rely on is broken — concurrent requests can observe each other's module-level state and /tmp, a correctness hazard, and concurrency-sensitive behavior (reserved concurrency, cold-start counts, per-environment metrics) cannot be reproduced.
Fix: the invoke scheduler / environment pool — never dispatch a second in-flight invocation into an environment that is already serving one; create (or throttle to) one environment per concurrent invoke.
Failing check ids: lambda.thirty-way-concurrency-creates-thirty-distinct-environments.

## Evidence
Behavioral suite replay against the emulator built from this checkout; service lambda is the largest service in the suite and these are the failures not already owned by an open issue.

Cause 1 — e.g. [mismatch] lambda.invoke-nonexistent:
  AssertionError: expected message 'Function not found: arn:aws:lambda:us-west-2:000000000000:function:sim2real-behav-fn-missing-f99ac642cf87:$LATEST', got 'Function not found: arn:aws:lambda:us-west-2:000000000000:function:sim2real-behav-fn-missing-f99ac642cf87'
  and [mismatch] lambda.invoke-with-response-stream-ec2-errors, lambda.arbitrary-code-executes, lambda.dry-run-does-not-invoke-handler, lambda.invoke-code-artifact-state-errors, lambda.nodejs-runtime-executes, lambda.nodejs-esm-named-module-handler-digests-nested-events, lambda.python-named-module-handler-digests-a-nested-unicode-event-on-arm64 — all the same expected/actual pair. Status 404 and code ResourceNotFoundException already match.

Cause 2 — [mismatch] lambda.deep-lambda-3-runtime-pin-manual-arn-survives-cold-start:
  AssertionError: START for 28948751-0ced-4cf4-b879-89b8d9125a2b is the first line of 2026/11/18/[$LATEST]289487510ced4cf4b87989b8d9125a2b; no INIT_START precedes it
  and the same assertion in lambda.deep-lambda-3-runtime-pin-foreign-runtime-arn-rejected.

Cause 3 — [mismatch] lambda.init-past-the-init-limit-is-retried-inside-the-invoke-and-timed-out-rejected:
  AssertionError: a 12 s init returned FunctionError=None: {'ok': True}
  Expected instead: FunctionError 'Unhandled', payload errorType 'Sandbox.Timedout', errorMessage 'RequestId: <rid> Error: Task timed out after 3.00 seconds' (function Timeout=3, init sleep 12 s). The check's positive control (the same code with a 0 s init) is asserted to succeed, so the requirement is selective.

Cause 4 — [mismatch] lambda.invoke-without-tenant-id-on-tenant-isolated-function-rejected:
  AssertionError: unexpected message for the missing-tenant-id refusal: 'TenantId is required if the function is configured for tenant isolation'
  Expected instead: InvalidParameterValueException with 'The invoked function is enabled with tenancy configuration. Add a valid tenant ID in your request and try again.'

Cause 5 — [mismatch] lambda.log-lambda-report-start-end-report-lines-bracket-the-invoke-request-id and lambda.log-lambda-report-log-level-with-text-format-rejected:
  AssertionError: REPORT line does not have the documented field layout: 'REPORT RequestId: 2d3bee81-e506-4394-820f-3a6c3115c3c3\tDuration: 1358.58 ms\tBilled Duration: 1359 ms\tMemory Size: 192 MB\tMax Memory Used: 1 MB\n'
  The graded layout is ^REPORT RequestId: <36>\tDuration: \d+\.\d{2} ms\tBilled Duration: \d+ ms\tMemory Size: \d+ MB\tMax Memory Used: \d+ MB\t(?:Init Duration: \d+\.\d{2} ms\t)?\n$ — the emulator's line lacks the terminating tab.

Cause 6 — [mismatch] lambda.log-lambda-report-new-environment-opens-new-stream-warm-reuse-shares-one:
  AssertionError: one environment must log every invocation to one stream, got ['2026/11/18/[$LATEST]47be7061e257404c98345bb79184b564', '2026/11/18/[$LATEST]b64a976fd2ff49f9ae802bdf8e75f14a', '2026/11/18/[$LATEST]b946983f769a4825909dd85bdacd2be7']

Cause 7 — [mismatch] lambda.log-lambda-report-unhandled-exception-error-line-matches-function-error-and-tail:
  expected '[ERROR] ValueError: BOOM-<marker>\nTraceback (most recent call last):\n<nbsp><nbsp>File "index.py", line 8, in handler\n<nbsp>x4>inner(event["marker"])\n<nbsp><nbsp>File "index.py", line 2, in inner\n<nbsp>x4>raise ValueError("BOOM-" + m)'
  got      '[ERROR] ValueError: BOOM-<marker>\n'

Cause 8 — [mismatch] lambda.msk-esm-role-without-describe-cluster-rejected-before-the-cluster-is-looked-up:
  AssertionError: expected a ClientError, the call succeeded: {... 'HTTPStatusCode': 202, 'UUID': '0b17f266-...', 'EventSourceArn': 'arn:aws:kafka:us-west-2:000000000000:cluster/sim2real-absent-79014f3cd2a9/<uuid>-2', 'State': 'Creating', ...}
  Expected instead: 400 InvalidParameterValueException 'Cannot access cluster <cluster ARN>. Please ensure the role can perform the DescribeClusterV2 (recommended) or DescribeCluster and GetBootstrapBrokers actions on your cluster in IAM.', then after granting only kafka:DescribeClusterV2, 404 ResourceNotFoundException 'The cluster resource: <cluster ARN> you requested does not exist.', with no mapping created.

Cause 9 — [mismatch] lambda.thirty-way-concurrency-creates-thirty-distinct-environments:
  AssertionError: 30 simultaneous invocations must be served by 30 distinct execution environments, got 4: ['0e2d7d3b-...', '40619b5f-...', '895f54f5-...', 'f9951e0b-...']

(All request ids, stream names, function names, accounts and measured durations above are this run's values; the requirements are the fixed texts, layouts and counts described per cause.)

## Steps to Reproduce
1. Clone the repo and build/boot the emulator from this branch; replay the simparity-behavioral-aws-v1 suite and filter to service lambda.
2. Cause 1: Invoke (and InvokeWithResponseStream) a function name that does not exist, with no Qualifier; read Error.Message. Replay the eight checks named under cause 1.
3. Cause 2: create a function, invoke it cold, then read its log stream events — observe START as the first event with no INIT_START. Replay lambda.deep-lambda-3-runtime-pin-manual-arn-survives-cold-start and lambda.deep-lambda-3-runtime-pin-foreign-runtime-arn-rejected.
4. Cause 3: create a python function with Timeout=3 whose module body sleeps 12 s (driven by an env var), invoke it, and read FunctionError and the payload. Replay lambda.init-past-the-init-limit-is-retried-inside-the-invoke-and-timed-out-rejected.
5. Cause 4: create a function with TenancyConfig TenantIsolationMode PER_TENANT, Invoke it with no X-Amz-Tenant-Id, read Error.Message. Replay lambda.invoke-without-tenant-id-on-tenant-isolated-function-rejected.
6. Cause 5: invoke a function with LogType Tail (or read the log group) and match the REPORT line against the documented layout. Replay the two checks named under cause 5.
7. Cause 6: invoke the same function three times warm and list the log streams that received the events. Replay lambda.log-lambda-report-new-environment-opens-new-stream-warm-reuse-shares-one.
8. Cause 7: invoke a python handler that raises a nested exception and compare the [ERROR] log event with the response payload's stackTrace. Replay lambda.log-lambda-report-unhandled-exception-error-line-matches-function-error-and-tail.
9. Cause 8: CreateEventSourceMapping(FunctionName=<fn>, EventSourceArn=arn:aws:kafka:<region>:<account>:cluster/<absent name>/<uuid>-2, Topics=['sim2real-topic'], StartingPosition='LATEST') with an execution role lacking the kafka describe actions; observe HTTP 202 and a created mapping. Replay lambda.msk-esm-role-without-describe-cluster-rejected-before-the-cluster-is-looked-up.
10. Cause 9: fire 30 simultaneous Invokes of one function and count the distinct environment identities the handler reports. Replay lambda.thirty-way-concurrency-creates-thirty-distinct-environments.
